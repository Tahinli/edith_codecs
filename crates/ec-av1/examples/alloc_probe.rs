//! Temporary lane-alloc instrument: count allocations and sample backtraces.
//!
//! Allocation totals come from `stats_alloc`. The per-size histogram and the
//! sampled backtraces come from `tracking-allocator`, which calls back into a
//! tracker on every allocation. Neither crate needs `unsafe` in this file.
//! Reallocs are excluded from totals and histogram, as the old hand-written
//! allocator did.
use std::alloc::System;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use stats_alloc::StatsAlloc;
use tracking_allocator::{AllocationGroupId, AllocationRegistry, AllocationTracker, Allocator};

static SIZES: [AtomicUsize; 24] = [const { AtomicUsize::new(0) }; 24];
static TRACES: Mutex<Option<HashMap<String, usize>>> = Mutex::new(None);
/// `reallocations` as of the last tracked allocation. A realloc bumps that counter
/// and then reaches the tracker as an allocation, so a bump means "not a new alloc".
static SEEN_REALLOCS: AtomicUsize = AtomicUsize::new(0);

struct Track;
impl AllocationTracker for Track {
    fn allocated(&self, _addr: usize, object_size: usize, _wrapped: usize, _g: AllocationGroupId) {
        let st = A.stats();
        if st.reallocations != SEEN_REALLOCS.swap(st.reallocations, Ordering::Relaxed) {
            return;
        }
        // `allocations` already includes this allocation; index it from 0 like the old counter.
        let n = st.allocations - 1;
        let cls = (usize::BITS - object_size.leading_zeros()) as usize;
        SIZES[cls.min(23)].fetch_add(1, Ordering::Relaxed);
        if n.is_multiple_of(512) {
            let bt = format!("{}", std::backtrace::Backtrace::force_capture());
            let mut key: Vec<String> = Vec::new();
            let lines: Vec<&str> = bt.lines().collect();
            for (i, l) in lines.iter().enumerate() {
                if l.contains("ec_av1::") && key.len() < 6 {
                    let at = lines
                        .get(i + 1)
                        .filter(|n| n.trim_start().starts_with("at "))
                        .map(|n| n.trim().to_string())
                        .unwrap_or_default();
                    key.push(format!("{} {}", l.trim(), at));
                }
            }
            let mut g = TRACES.lock().unwrap();
            *g.get_or_insert_with(HashMap::new).entry(key.join(" <- ")).or_insert(0) += 1;
            // The capture's own reallocs were not tracked; skip them too.
            SEEN_REALLOCS.store(A.stats().reallocations, Ordering::Relaxed);
        }
    }
    fn deallocated(
        &self,
        _addr: usize,
        _object_size: usize,
        _wrapped: usize,
        _source: AllocationGroupId,
        _current: AllocationGroupId,
    ) {
    }
}

#[global_allocator]
static A: StatsAlloc<Allocator<System>> = StatsAlloc::new(Allocator::system());

fn main() {
    AllocationRegistry::set_global_tracker(Track).expect("tracker");
    AllocationRegistry::enable_tracking();
    let path = std::env::args().nth(1).expect("usage: alloc_probe <stream.obu>");
    let data = std::fs::read(&path).expect("read");
    let mut shown = 0usize;
    let r = ec_av1::stream::decode_stream_with(&data, |_f, _i, s| {
        if s {
            shown += 1;
        }
        Ok(())
    });
    println!("shown={shown} result={:?}", r.is_ok());
    let st = A.stats();
    println!("allocs={} bytes={}", st.allocations, st.bytes_allocated);
    for (i, c) in SIZES.iter().enumerate() {
        let v = c.load(Ordering::Relaxed);
        if v > 0 {
            println!("size 2^{i} : {v}");
        }
    }
    let g = TRACES.lock().unwrap();
    if let Some(m) = g.as_ref() {
        let mut v: Vec<_> = m.iter().collect();
        v.sort_by_key(|(_, c)| std::cmp::Reverse(**c));
        for (k, c) in v.into_iter().take(30) {
            println!("{c:6}  {k}");
        }
    }
}
