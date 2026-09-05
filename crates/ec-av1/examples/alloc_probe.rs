//! Temporary lane-alloc instrument: count allocations and sample backtraces.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNT: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
static SIZES: [AtomicUsize; 24] = [const { AtomicUsize::new(0) }; 24];
static TRACES: Mutex<Option<HashMap<String, usize>>> = Mutex::new(None);

thread_local! { static IN: Cell<bool> = const { Cell::new(false) }; }

struct Track;
unsafe impl GlobalAlloc for Track {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let n = COUNT.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(l.size(), Ordering::Relaxed);
        let cls = (usize::BITS - l.size().leading_zeros()) as usize;
        SIZES[cls.min(23)].fetch_add(1, Ordering::Relaxed);
        if n % 512 == 0 {
            let re = IN.with(|f| f.replace(true));
            if !re {
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
                drop(g);
                IN.with(|f| f.set(false));
            }
        }
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static A: Track = Track;

fn main() {
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
    println!(
        "allocs={} bytes={}",
        COUNT.load(Ordering::Relaxed),
        BYTES.load(Ordering::Relaxed)
    );
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
