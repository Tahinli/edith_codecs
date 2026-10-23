//! Persistent band workers for the slice-parallel encoder.
//!
//! One thread per band, started once and fed one job per picture over a
//! channel, so a picture costs a send and a receive per band instead of a
//! thread spawn and join. Created by the encoder on its first multi-band
//! picture and joined when the encoder is dropped. std only.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

/// A fixed set of worker threads, each applying one function to the jobs
/// sent to it, in order.
pub(super) struct BandPool<J: Send + 'static, R: Send + 'static> {
    lanes: Vec<Lane<J, R>>,
}

struct Lane<J, R> {
    /// Taken on drop, which ends the worker's loop.
    jobs: Option<Sender<J>>,
    done: Receiver<R>,
    handle: Option<JoinHandle<()>>,
}

impl<J: Send + 'static, R: Send + 'static> BandPool<J, R> {
    /// `n` idle workers, each running `work` on the jobs it is sent.
    pub(super) fn new(n: usize, work: fn(J) -> R) -> Self {
        let lanes = (0..n)
            .map(|i| {
                let (jobs, job_rx) = channel::<J>();
                let (done_tx, done) = channel::<R>();
                let handle = std::thread::Builder::new()
                    .name(format!("ec-h264-band-{i}"))
                    .spawn(move || {
                        while let Ok(job) = job_rx.recv() {
                            if done_tx.send(work(job)).is_err() {
                                break;
                            }
                        }
                    })
                    .expect("start a band worker thread");
                Lane {
                    jobs: Some(jobs),
                    done,
                    handle: Some(handle),
                }
            })
            .collect();
        BandPool { lanes }
    }

    /// Queue `job` on worker `lane`.
    pub(super) fn submit(&self, lane: usize, job: J) {
        let jobs = self.lanes[lane]
            .jobs
            .as_ref()
            .expect("a band worker is running");
        jobs.send(job).expect("a band worker panicked");
    }

    /// Wait for worker `lane` to answer its oldest job.
    pub(super) fn collect(&self, lane: usize) -> R {
        self.lanes[lane]
            .done
            .recv()
            .expect("a band worker panicked")
    }
}

impl<J: Send + 'static, R: Send + 'static> Drop for BandPool<J, R> {
    fn drop(&mut self) {
        for lane in &mut self.lanes {
            drop(lane.jobs.take());
        }
        for lane in &mut self.lanes {
            if let Some(handle) = lane.handle.take() {
                // A worker that panicked has already reported it; nothing to add.
                let _ = handle.join();
            }
        }
    }
}
