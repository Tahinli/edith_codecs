//! lane-timeline: `EC_AV1_TIMELINE=1` frame-granularity decode timeline.
//!
//! The threaded decoder (`stream.rs`) is bounded by *something* -- reference
//! latency, the dispatcher's in-flight cap, the filter tail or the output
//! path -- and no instrument could tell which: `EC_AV1_WAVE_STATS` counts
//! superblocks, `perf` counts instructions, neither sees the dependency
//! graph. This records ~10 timestamps per coded frame (submit, worker start,
//! reference waits, parse, recon, filters, slot publish, grain, output) on
//! one monotonic clock and dumps them as TSV at stream end;
//! `scripts/av1-timeline-report.py` reads that and walks the critical path.
//!
//! Cost when off: one relaxed atomic load ([`env_flag!`]'s `LazyLock<bool>`)
//! per event, at frame granularity -- ~10 loads per frame, nothing per block.

use std::sync::Mutex;
use std::time::Instant;

/// Process start, the zero of every timestamp below.
static START: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);

/// `EC_AV1_TIMELINE` -- one atomic load per call site after the first.
pub(crate) fn on() -> bool {
    crate::envflags::env_flag!("EC_AV1_TIMELINE")
}

fn now_us() -> u64 {
    START.elapsed().as_micros() as u64
}

/// One coded frame's timestamps, in microseconds since [`START`]; 0 = never
/// reached (a key frame decodes inline, so it has no submit/start/publish).
#[derive(Clone, Default)]
struct Rec {
    used: bool,
    ftype: char,
    show: bool,
    submit: u64,
    start: u64,
    refs_ready: u64,
    hdr_done: u64,
    tile_done: u64,
    recon_done: u64,
    filters_done: u64,
    slot_fulfil: u64,
    grain_done: u64,
    output: u64,
    /// Slot this frame blocked on longest, which frame produced it, and
    /// for how long.
    wait_slot: i32,
    wait_owner: i64,
    wait_us: u64,
    /// The producing frame of every slot it waited on, for the DAG.
    deps: Vec<usize>,
    pipe: bool,
}

#[derive(Default)]
struct Log {
    frames: Vec<Rec>,
    /// Dispatcher blocks at the in-flight bound: (kind, begin, end).
    caps: Vec<(&'static str, u64, u64)>,
}

static LOG: Mutex<Option<Log>> = Mutex::new(None);

fn with<F: FnOnce(&mut Log)>(f: F) {
    let mut g = LOG.lock().expect("timeline");
    f(g.get_or_insert_with(Log::default));
}

fn rec<F: FnOnce(&mut Rec)>(idx: usize, f: F) {
    with(|log| {
        if log.frames.len() <= idx {
            log.frames.resize(idx + 1, Rec { wait_slot: -1, wait_owner: -1, ..Rec::default() });
        }
        let r = &mut log.frames[idx];
        r.used = true;
        f(r);
    });
}

// The frame this thread is decoding, so `decode.rs`'s phase marks need no
// new parameter threaded through the tile decoders.
thread_local! {
    static CUR: std::cell::Cell<usize> = const { std::cell::Cell::new(usize::MAX) };
}

/// Dispatch (parsing thread), before the job is queued.
#[cold]
pub(crate) fn submit(idx: usize, ftype: char, show: bool) {
    if !on() {
        return;
    }
    let t = now_us();
    rec(idx, |r| {
        r.ftype = ftype;
        r.show = show;
        r.submit = t;
    });
}

/// A worker picked the job up. Also binds this thread to `idx`.
#[cold]
pub(crate) fn start(idx: usize) {
    if !on() {
        return;
    }
    CUR.with(|c| c.set(idx));
    let t = now_us();
    rec(idx, |r| r.start = t);
}

/// Binds a thread to `idx` without a start timestamp (the inline key-frame
/// path, which is dispatched by nobody).
#[cold]
pub(crate) fn bind(idx: usize, ftype: char, show: bool) {
    if !on() {
        return;
    }
    CUR.with(|c| c.set(idx));
    rec(idx, |r| {
        r.ftype = ftype;
        r.show = show;
    });
}

/// One resolved `SlotPromise::wait`, with the slot and how long it blocked.
#[cold]
pub(crate) fn ref_wait(idx: usize, slot: usize, owner: usize, waited_us: u64) {
    if !on() {
        return;
    }
    rec(idx, |r| {
        if owner != usize::MAX && !r.deps.contains(&owner) {
            r.deps.push(owner);
        }
        if waited_us > r.wait_us {
            r.wait_us = waited_us;
            r.wait_slot = slot as i32;
            r.wait_owner = if owner == usize::MAX { -1 } else { owner as i64 };
        }
    });
}

macro_rules! stamp {
    ($name:ident, $field:ident) => {
        #[doc = concat!("Marks `", stringify!($field), "` for the frame this thread is decoding.")]
        #[cold]
        pub(crate) fn $name() {
            if !on() {
                return;
            }
            let (idx, t) = (CUR.with(std::cell::Cell::get), now_us());
            if idx != usize::MAX {
                rec(idx, |r| r.$field = t);
            }
        }
    };
}
stamp!(refs_ready, refs_ready);
stamp!(hdr_done, hdr_done);
stamp!(tile_done, tile_done);
stamp!(recon_done, recon_done);
stamp!(filters_done, filters_done);
stamp!(slot_fulfil, slot_fulfil);
stamp!(grain_done, grain_done);

/// Whether this frame's filters ran pipelined per superblock row.
#[cold]
pub(crate) fn note_pipe(pipe: bool) {
    if !on() {
        return;
    }
    let idx = CUR.with(std::cell::Cell::get);
    if idx != usize::MAX {
        rec(idx, |r| r.pipe = pipe);
    }
}

/// The picture reached the sink. Only a frame that finished on a worker
/// (`grain_done`) and has not been emitted yet takes the stamp -- a
/// `show_existing_frame` re-output reports a decode index that is not its
/// own, and must not overwrite another frame's row.
#[cold]
pub(crate) fn output(idx: usize) {
    if !on() {
        return;
    }
    let t = now_us();
    with(|log| {
        if let Some(r) = log.frames.get_mut(idx)
            && r.output == 0
            // A frame that finished on a worker, or the inline key frame
            // (never submitted) that has passed its filters.
            && (r.grain_done != 0 || (r.submit == 0 && r.filters_done != 0))
        {
            r.output = t;
        }
    });
}

/// The parsing thread blocked at the in-flight bound.
#[cold]
pub(crate) fn cap_wait(kind: &'static str, begin: u64, end: u64) {
    if !on() {
        return;
    }
    with(|log| log.caps.push((kind, begin, end)));
}

/// `now_us` for the two call sites that need a begin timestamp of their own.
pub(crate) fn now() -> u64 {
    if !on() {
        return 0;
    }
    now_us()
}

/// Writes every row to stderr as TSV and clears the log.
#[cold]
pub(crate) fn dump() {
    if !on() {
        return;
    }
    let taken = LOG.lock().expect("timeline").take();
    let Some(log) = taken else { return };
    let mut out = String::new();
    out.push_str(
        "TL_HDR\tidx\ttype\tshow\tsubmit\tstart\trefs_ready\thdr_done\ttile_done\trecon_done\tfilters_done\tslot_fulfil\tgrain_done\toutput\twait_slot\twait_owner\twait_us\tpipe\tdeps\n",
    );
    for (idx, r) in log.frames.iter().enumerate() {
        if !r.used {
            continue;
        }
        let deps: Vec<String> = r.deps.iter().map(std::string::ToString::to_string).collect();
        out.push_str(&format!(
            "TL\t{idx}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            r.ftype,
            u8::from(r.show),
            r.submit,
            r.start,
            r.refs_ready,
            r.hdr_done,
            r.tile_done,
            r.recon_done,
            r.filters_done,
            r.slot_fulfil,
            r.grain_done,
            r.output,
            r.wait_slot,
            r.wait_owner,
            r.wait_us,
            u8::from(r.pipe),
            if deps.is_empty() { "-".to_string() } else { deps.join(",") },
        ));
    }
    for (kind, b, e) in &log.caps {
        out.push_str(&format!("TL_CAP\t{kind}\t{b}\t{e}\n"));
    }
    out.push_str(&format!("TL_END\t{}\n", now_us()));
    eprint!("{out}");
}
