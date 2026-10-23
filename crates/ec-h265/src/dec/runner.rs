//! Where a picture is decoded: on the calling thread, or on a worker thread of its own.
//!
//! With frame threads each picture gets one worker thread. The decoder sends the worker
//! the picture's slices in decoding order, then its finish, and goes on with the next
//! picture. The worker runs the slices on its [`PicJob`] and publishes each row as it
//! becomes final ([`super::shared`]). A worker only waits for pictures decoded before its
//! own (its references and collocated picture), and the decoder keeps at most `threads`
//! pictures in flight, so the waits form no cycle.
//!
//! With one thread every picture is decoded inline: the same job, run on the calling
//! thread as each slice arrives, so an error comes back from the call that caused it.
//!
//! A worker that fails (a corrupt slice, or a panic) marks its picture as failed in the
//! shared picture, so anything waiting on it wakes with the error.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

use super::picjob::{PicJob, SliceJob};
use super::trace::{self, Phase};
use ec_core::error::{Error, Result};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // The guarded values are plain data; a poisoned lock is still usable.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The worker threads of the decoder, shared by the pictures in flight (frame threads)
/// and the wavefront row workers of each picture. A picture always holds one thread while
/// it is in flight; the rest of the budget is handed out as extra row workers by
/// [`Budget::take_extra`], which never waits.
#[derive(Debug)]
pub struct Budget {
    total: usize,
    used: Mutex<BudgetUse>,
}

#[derive(Debug, Default)]
struct BudgetUse {
    pictures: usize,
    extra: usize,
}

impl Budget {
    /// A budget of `total` threads.
    pub fn new(total: usize) -> Arc<Budget> {
        Arc::new(Budget {
            total: total.max(1),
            used: Mutex::new(BudgetUse::default()),
        })
    }

    /// Take up to `want` extra threads, as many as the threads no picture or row worker
    /// is using allow. The threads are given back when the returned guard drops.
    pub fn take_extra(self: &Arc<Self>, want: usize) -> ExtraThreads {
        let mut used = lock(&self.used);
        let free = self.total.saturating_sub(used.pictures + used.extra);
        let n = want.min(free);
        used.extra += n;
        ExtraThreads {
            budget: Arc::clone(self),
            n,
        }
    }
}

/// Extra threads taken from a [`Budget`] for one picture's row workers.
#[derive(Debug)]
pub struct ExtraThreads {
    budget: Arc<Budget>,
    n: usize,
}

impl ExtraThreads {
    /// How many extra threads were taken.
    pub fn count(&self) -> usize {
        self.n
    }
}

impl Drop for ExtraThreads {
    fn drop(&mut self) {
        lock(&self.budget.used).extra -= self.n;
    }
}

/// One picture's hold on the budget: taken when the picture is created, released when it
/// is dropped (after its worker has finished, or with the inline job).
#[derive(Debug)]
pub struct PictureSlot {
    budget: Arc<Budget>,
}

impl PictureSlot {
    /// Take a picture's thread from `budget`.
    pub fn new(budget: Arc<Budget>) -> PictureSlot {
        lock(&budget.used).pictures += 1;
        PictureSlot { budget }
    }
}

impl Drop for PictureSlot {
    fn drop(&mut self) {
        lock(&self.budget.used).pictures -= 1;
    }
}

/// The motion checksum of one picture (see [`super::Decoder::motion_checksum`]), set when
/// the picture finishes. The value is `None` for a picture no slice ran in, and also for a
/// picture that failed.
#[derive(Debug, Default)]
pub struct Checksum {
    value: Mutex<Option<Option<u64>>>,
    changed: Condvar,
}

impl Checksum {
    /// Store the picture's value and wake the readers.
    pub fn set(&self, value: Option<u64>) {
        *lock(&self.value) = Some(value);
        self.changed.notify_all();
    }

    /// Wait until the value is stored.
    pub fn wait(&self) -> Option<u64> {
        let mut value = lock(&self.value);
        loop {
            if let Some(v) = *value {
                return v;
            }
            value = self.changed.wait(value).unwrap_or_else(|e| e.into_inner());
        }
    }
}

/// A message to a picture's worker.
pub enum Msg {
    /// Run this slice segment.
    Slice(Box<SliceJob>),
    /// The picture is complete: finish it and stop.
    Finish,
}

/// The decoding of one picture, on the calling thread or on its own worker.
pub enum Runner {
    /// On the calling thread.
    Inline {
        /// The picture's job.
        job: Box<PicJob>,
        /// Set when the picture finishes.
        checksum: Arc<Checksum>,
    },
    /// On a worker thread that owns the job.
    Worker {
        /// The queue of messages to the worker.
        tx: Sender<Msg>,
        /// The worker thread.
        handle: JoinHandle<()>,
    },
}

impl Runner {
    /// Start the decoding of the picture of `job`. `threaded` gives it a worker thread.
    pub fn start(job: PicJob, checksum: Arc<Checksum>, threaded: bool) -> Runner {
        if !threaded {
            return Runner::Inline {
                job: Box::new(job),
                checksum,
            };
        }
        let (tx, rx) = mpsc::channel();
        let handle = std::thread::spawn(move || run_worker(rx, job, checksum));
        Runner::Worker { tx, handle }
    }

    /// Give the picture one slice segment. Inline, the slice runs now and its error is
    /// returned; on a worker it is queued, and an error surfaces when the picture's
    /// output is taken.
    pub fn submit(&mut self, slice: SliceJob) -> Result<()> {
        match self {
            Runner::Inline { job, .. } => job.run_slice(slice),
            Runner::Worker { tx, .. } => {
                // A worker that is gone has already failed its picture.
                let _ = tx.send(Msg::Slice(Box::new(slice)));
                Ok(())
            }
        }
    }

    /// The picture is complete. Inline, it finishes now. A worker is told to finish and
    /// goes on in the background; its handle is returned to be joined later.
    pub fn finish(self) -> Option<JoinHandle<()>> {
        match self {
            Runner::Inline { mut job, checksum } => {
                checksum.set(job.finish());
                None
            }
            Runner::Worker { tx, handle } => {
                let _ = tx.send(Msg::Finish);
                Some(handle)
            }
        }
    }
}

/// The body of a picture's worker thread. A failure, a panic, or a picture dropped before
/// its finish marks the picture as failed, so that nothing waits on it forever.
fn run_worker(rx: Receiver<Msg>, mut job: PicJob, checksum: Arc<Checksum>) {
    let shared = Arc::clone(job.shared());
    trace::set_role("picture");
    let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<Option<u64>> {
        loop {
            let idle = trace::now();
            let msg = rx.recv();
            trace::since(Phase::WaitMsg, idle);
            let Ok(msg) = msg else { break };
            let busy = trace::now();
            match msg {
                Msg::Slice(slice) => job.run_slice(*slice)?,
                Msg::Finish => {
                    let sum = job.finish();
                    trace::since(Phase::Decode, busy);
                    return Ok(sum);
                }
            }
            trace::since(Phase::Decode, busy);
        }
        Err(Error::corrupt(
            "HEVC decode: a picture was dropped before it finished".to_string(),
        ))
    }));
    match outcome {
        Ok(Ok(sum)) => checksum.set(sum),
        Ok(Err(e)) => {
            shared.fail(&e.to_string());
            checksum.set(None);
        }
        Err(_) => {
            shared.fail("HEVC decode: the picture's worker thread panicked");
            checksum.set(None);
        }
    }
}
