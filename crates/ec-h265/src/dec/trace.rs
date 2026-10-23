//! Opt-in timing of the decoder's threads, for finding where they sit idle.
//!
//! `EC_H265_TRACE=1` turns it on. Off (the default), every hook is one check of a cached
//! flag and no clock is read. On, each thread adds the time it spends in each [`Phase`] to
//! its own totals. A thread's totals are merged into the table of its role when the thread
//! ends (or, for the calling thread, when [`report`] runs), and [`report`] prints the table
//! to stderr at the end of the stream.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

/// The phases a thread's time is split into.
#[derive(Clone, Copy, Debug)]
pub enum Phase {
    /// Decoding a slice, a wavefront row, or finishing a picture (wall time; it includes
    /// the waits below that happen inside it).
    Decode = 0,
    /// Blocked until a reference picture has the rows a motion compensation reads.
    WaitRows = 1,
    /// Blocked joining the oldest in-flight picture to free a picture slot.
    WaitAdmit = 2,
    /// Blocked on the CABAC row-sync: the row above must be two CTBs ahead.
    WaitSync = 3,
    /// A picture worker idle, waiting for the next slice or the finish message.
    WaitMsg = 4,
    /// Blocked joining the picture workers at end of stream.
    WaitJoin = 5,
    /// Serial work on the calling thread outside the slice decode: picture start and
    /// finish (RPS, bumping, DPB insert).
    Serial = 6,
    /// Blocked until the collocated picture has the motion rows temporal prediction reads.
    WaitCol = 7,
}

const N: usize = 8;
/// Whether tracing is on (`EC_H265_TRACE` set to anything but `0`).
pub fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("EC_H265_TRACE").is_some_and(|v| v != "0"))
}

/// The start of a timed span, or `None` when tracing is off (no clock is read).
#[inline]
pub fn now() -> Option<Instant> {
    if on() { Some(Instant::now()) } else { None }
}

/// Add the time since `start` to `phase` of this thread.
#[inline]
pub fn since(phase: Phase, start: Option<Instant>) {
    if let Some(t) = start {
        add(phase, t.elapsed());
    }
}

fn add(phase: Phase, d: Duration) {
    let _ = ACC.try_with(|a| {
        a.borrow_mut().ns[phase as usize] += d.as_nanos() as u64;
    });
}

/// Name this thread's role in the report (the default is `caller`).
pub fn set_role(role: &'static str) {
    if on() {
        let _ = ACC.try_with(|a| a.borrow_mut().role = role);
    }
}

#[derive(Default)]
struct Totals {
    threads: usize,
    ns: [u64; N],
}

fn totals() -> MutexGuard<'static, BTreeMap<&'static str, Totals>> {
    static T: Mutex<BTreeMap<&'static str, Totals>> = Mutex::new(BTreeMap::new());
    T.lock().unwrap_or_else(|e| e.into_inner())
}

struct Acc {
    role: &'static str,
    ns: [u64; N],
}

impl Acc {
    /// Merge this thread's totals into its role's table and clear them.
    fn merge(&mut self) {
        if self.ns.iter().all(|&n| n == 0) {
            return;
        }
        let mut t = totals();
        let e = t.entry(self.role).or_default();
        e.threads += 1;
        for (dst, src) in e.ns.iter_mut().zip(self.ns.iter()) {
            *dst += src;
        }
        self.ns = [0; N];
    }
}

impl Drop for Acc {
    fn drop(&mut self) {
        self.merge();
    }
}

thread_local! {
    static ACC: RefCell<Acc> = const { RefCell::new(Acc { role: "caller", ns: [0; N] }) };
}

/// Merge the calling thread's totals, then print every role's table to stderr and clear
/// the tables. Worker threads have merged already: they are joined before this runs.
pub fn report() {
    if !on() {
        return;
    }
    let _ = ACC.try_with(|a| a.borrow_mut().merge());
    let mut t = totals();
    if t.is_empty() {
        return;
    }
    eprintln!("EC_H265_TRACE (ms, summed over the threads of each role)");
    eprintln!(
        "{:<12}{:>8}{:>12}{:>12}{:>12}{:>12}{:>12}{:>12}{:>12}{:>12}",
        "role",
        "threads",
        "decode",
        "wait_rows",
        "wait_col",
        "wait_admit",
        "wait_sync",
        "wait_msg",
        "wait_join",
        "serial"
    );
    for (role, tot) in t.iter() {
        let ms = |p: Phase| tot.ns[p as usize] as f64 / 1e6;
        eprintln!(
            "{:<12}{:>8}{:>12.1}{:>12.1}{:>12.1}{:>12.1}{:>12.1}{:>12.1}{:>12.1}{:>12.1}",
            role,
            tot.threads,
            ms(Phase::Decode),
            ms(Phase::WaitRows),
            ms(Phase::WaitCol),
            ms(Phase::WaitAdmit),
            ms(Phase::WaitSync),
            ms(Phase::WaitMsg),
            ms(Phase::WaitJoin),
            ms(Phase::Serial),
        );
    }
    t.clear();
}
