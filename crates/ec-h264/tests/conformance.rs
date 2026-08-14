//! JVT conformance: first IDR picture, bit-exact.
//!
//! Fixture-gated like tools/oracle: fixtures/ is gitignored, so a checkout
//! without `scripts/fetch-vectors.sh` skips loudly instead of failing.
//!
//! Oracles, in order of authority:
//! 1. The reference decoder YUV that ships with each JVT vector (first
//!    frame) — ffmpeg-independent.
//! 2. `ffmpeg -i <stream> -frames:v 1` raw output, cross-checked when the
//!    binary is present.
//!
//! The same binary hosts the zero-allocation proof and the ns/MB
//! measurement so the crate keeps a single integration-test link unit.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use ec_h264::{Decoder, Error};

// ---------------------------------------------------------------------------
// Counting allocator: proves the steady-state decode loop allocates nothing.
// Lives in the test binary only — the shipped crates have no allocator games.
// ---------------------------------------------------------------------------

struct CountingAlloc;

static ALLOCS: AtomicU64 = AtomicU64::new(0);

std::thread_local! {
    /// Only the test thread that armed the counter is counted — other test
    /// threads (and their ffmpeg child plumbing) allocate freely.
    static COUNTING_HERE: Cell<bool> = const { Cell::new(false) };
}

fn counting_here() -> bool {
    COUNTING_HERE.try_with(Cell::get).unwrap_or(false)
}

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if counting_here() {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if counting_here() {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static A: CountingAlloc = CountingAlloc;

// ---------------------------------------------------------------------------

fn vectors_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/vectors/h264-jvt")
}

/// The CAVLC, progressive, single-slice-group streams whose first IDR this
/// decoder must reproduce bit-exactly.
const CAVLC_STREAMS: &[&str] = &[
    "AUD_MW_E",
    "BA1_Sony_D",
    "BA3_SVA_C",
    "BA_MW_D",
    "BANM_MW_D",
    "BASQP1_Sony_C",
    "CI_MW_D",
    "MIDR_MW_D",
    "NL1_Sony_D",
    "NL3_SVA_E",
    "NRF_MW_E",
    "SVA_BA2_D",
    "SVA_CL1_E",
    "SVA_NL2_E",
    "CVPCMNL2_SVA_C",
    // First PPS/picture of these carries no FMO; the first IDR is plain
    // CAVLC baseline and must be bit-exact like the rest.
    "SVA_FM1_E",
    "SL1_SVA_B",
    "MR1_BT_A",
];

/// Streams that must fail with a named Unsupported error (wrong output is
/// forbidden; refusal strings are claims backed by these tests).
const UNSUPPORTED_STREAMS: &[(&str, &str)] = &[
    ("CABA1_SVA_B", "CABAC"),      // entropy_coding_mode_flag = 1
    ("MR9_BT_B", "CABAC"),         // Main profile, CABAC
    ("CAMACI3_Sony_C", "CABAC"),   // CABAC + interlace tools
    ("FM2_SVA_C", "FMO"),          // num_slice_groups > 1
    ("CVFI1_SVA_C", "interlaced"), // field pictures
    ("FI1_Sony_E", "interlaced"),  // field pictures
];

fn find_stream(dir: &Path) -> Option<PathBuf> {
    let mut fallback = None;
    for entry in std::fs::read_dir(dir).ok()? {
        let p = entry.ok()?.path();
        let ext = p.extension()?.to_str()?.to_ascii_lowercase();
        if matches!(ext.as_str(), "jsv" | "264" | "avc" | "26l" | "h264" | "jvt") {
            return Some(p);
        }
        if ext == "bit" {
            fallback = Some(p);
        }
    }
    fallback
}

fn find_ref_yuv(dir: &Path) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()? {
        let p = entry.ok()?.path();
        let ext = p.extension()?.to_str()?.to_ascii_lowercase();
        if ext == "yuv" || ext == "qcif" {
            return Some(p);
        }
    }
    None
}

fn ffmpeg_first_frame(stream: &Path) -> Option<Vec<u8>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(stream)
        .args([
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-",
        ])
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    Some(out.stdout)
}

/// Decode the first IDR and return (frame bytes as I420, width, height).
fn decode_first(stream_bytes: &[u8]) -> Result<(Vec<u8>, usize, usize), Error> {
    let mut dec = Decoder::new();
    let frame = dec.decode_first_idr(stream_bytes)?;
    let (w, h) = (frame.width as usize, frame.height as usize);
    let mut out = Vec::with_capacity(w * h * 3 / 2);
    let dims = [
        (w, h),
        (w.div_ceil(2), h.div_ceil(2)),
        (w.div_ceil(2), h.div_ceil(2)),
    ];
    for (plane, (pw, ph)) in frame.planes.iter().zip(dims) {
        for y in 0..ph {
            out.extend_from_slice(plane.row(y, pw).expect("plane row"));
        }
    }
    Ok((out, w, h))
}

fn first_diff(a: &[u8], b: &[u8]) -> Option<usize> {
    a.iter().zip(b).position(|(x, y)| x != y)
}

#[test]
fn jvt_cavlc_first_idr_bit_exact() {
    let base = vectors_dir();
    if !base.is_dir() {
        eprintln!(
            "SKIP: {} missing — run scripts/fetch-vectors.sh",
            base.display()
        );
        return;
    }
    let mut passed = Vec::new();
    let mut failed = Vec::new();
    for name in CAVLC_STREAMS {
        let dir = base.join(name);
        let Some(stream) = find_stream(&dir) else {
            eprintln!("SKIP {name}: no bitstream file");
            continue;
        };
        let bytes = std::fs::read(&stream).unwrap();
        match decode_first(&bytes) {
            Ok((ours, w, h)) => {
                let frame_len = w * h * 3 / 2;
                // Oracle 1: reference decoder YUV, first frame.
                let mut ok = true;
                if let Some(ref_path) = find_ref_yuv(&dir) {
                    let reference = std::fs::read(&ref_path).unwrap();
                    assert!(
                        reference.len() >= frame_len,
                        "{name}: reference YUV shorter than one frame"
                    );
                    if let Some(pos) = first_diff(&ours, &reference[..frame_len]) {
                        eprintln!(
                            "FAIL {name}: differs from reference YUV at byte {pos} \
                             (plane offset, {w}x{h}): ours {} ref {}",
                            ours[pos], reference[pos]
                        );
                        ok = false;
                    }
                } else {
                    eprintln!("WARN {name}: no reference YUV, ffmpeg only");
                }
                // Oracle 2: ffmpeg.
                if let Some(ff) = ffmpeg_first_frame(&stream) {
                    assert!(ff.len() >= frame_len, "{name}: ffmpeg output short");
                    if let Some(pos) = first_diff(&ours, &ff[..frame_len]) {
                        eprintln!(
                            "FAIL {name}: differs from ffmpeg at byte {pos}: ours {} ff {}",
                            ours[pos], ff[pos]
                        );
                        ok = false;
                    }
                }
                if ok {
                    passed.push(*name);
                } else {
                    failed.push(*name);
                }
            }
            Err(e) => {
                eprintln!("FAIL {name}: decode error: {e}");
                failed.push(*name);
            }
        }
    }
    eprintln!(
        "PASS table ({} streams bit-exact): {passed:?}",
        passed.len()
    );
    assert!(failed.is_empty(), "streams not bit-exact: {failed:?}");
    assert!(
        passed.len() >= 5,
        "fewer than 5 CAVLC conformance streams verified: {passed:?}"
    );
}

#[test]
fn unsupported_streams_refuse_with_named_errors() {
    let base = vectors_dir();
    if !base.is_dir() {
        eprintln!("SKIP: fixtures missing");
        return;
    }
    for (name, what) in UNSUPPORTED_STREAMS {
        let dir = base.join(name);
        let Some(stream) = find_stream(&dir) else {
            eprintln!("SKIP {name}: no bitstream file");
            continue;
        };
        let bytes = std::fs::read(&stream).unwrap();
        match decode_first(&bytes) {
            Err(Error::Unsupported { what: w, .. }) => {
                let w_lower = w.to_lowercase();
                let expect = what.to_lowercase();
                assert!(
                    w_lower.contains(&expect)
                        || (expect == "interlaced" && w_lower.contains("interlace")),
                    "{name}: expected Unsupported({what}), got Unsupported({w})"
                );
            }
            Ok(_) => panic!("{name}: expected a named Unsupported error, decode succeeded"),
            Err(e) => panic!("{name}: expected Unsupported({what}), got: {e}"),
        }
    }
}

/// Steady-state decode loop performs zero heap allocations: after one warm-up
/// picture, re-decoding the same slices into the reused picture context
/// (including deblocking) must not allocate. Frame emission (`frame()`)
/// allocates the output planes and is measured separately as nonzero to prove
/// the counter works.
#[test]
fn steady_state_decode_loop_zero_alloc() {
    let base = vectors_dir();
    if !base.is_dir() {
        eprintln!("SKIP: fixtures missing");
        return;
    }
    let stream = find_stream(&base.join("BA1_Sony_D")).expect("BA1_Sony_D fixture");
    let bytes = std::fs::read(stream).unwrap();
    let mut dec = Decoder::new();
    // Warm-up: full first picture, growing every reusable buffer.
    let _ = dec.decode_first_idr(&bytes).expect("warm-up decode");

    // Steady state: same IDR slices again, counted.
    let nals: Vec<&[u8]> = ec_h264_syntax::AnnexBIter::new(&bytes).collect();
    ALLOCS.store(0, Ordering::SeqCst);
    COUNTING_HERE.with(|c| c.set(true));
    let mut decoded = false;
    for nal in &nals {
        match dec.push_nal(nal) {
            Ok(ec_h264::NalOutcome::PictureBoundary) => break,
            Ok(ec_h264::NalOutcome::SliceDecoded) => decoded = true,
            Ok(_) => {}
            Err(e) => panic!("steady-state push failed: {e}"),
        }
    }
    dec.end_picture().expect("end_picture");
    COUNTING_HERE.with(|c| c.set(false));
    assert!(decoded, "no slice decoded in steady-state pass");
    let n = ALLOCS.load(Ordering::SeqCst);
    assert_eq!(n, 0, "steady-state decode loop allocated {n} times");

    // Sanity: the counter is alive — frame emission does allocate.
    ALLOCS.store(0, Ordering::SeqCst);
    COUNTING_HERE.with(|c| c.set(true));
    let frame = dec.frame().expect("frame");
    COUNTING_HERE.with(|c| c.set(false));
    assert!(frame.width > 0);
    assert!(
        ALLOCS.load(Ordering::SeqCst) > 0,
        "allocation counter appears dead"
    );
}

/// Decode-rate measurement: ns per macroblock and Mpx/s over the first IDR,
/// printed for the perf report. Run with `--nocapture` to see it. Uses the
/// steady-state path (buffers warm) like a playback loop would.
#[test]
fn ns_per_macroblock_measurement() {
    let base = vectors_dir();
    if !base.is_dir() {
        eprintln!("SKIP: fixtures missing");
        return;
    }
    for name in ["BA1_Sony_D", "CI_MW_D", "NL1_Sony_D"] {
        let Some(stream) = find_stream(&base.join(name)) else {
            continue;
        };
        let bytes = std::fs::read(stream).unwrap();
        let mut dec = Decoder::new();
        let frame = dec.decode_first_idr(&bytes).expect("warm-up");
        let (w, h) = (frame.width as u64, frame.height as u64);
        let mbs = (w.div_ceil(16)) * (h.div_ceil(16));
        let nals: Vec<&[u8]> = ec_h264_syntax::AnnexBIter::new(&bytes).collect();

        let iters = 200u32;
        let start = std::time::Instant::now();
        for _ in 0..iters {
            for nal in &nals {
                match dec.push_nal(nal) {
                    Ok(ec_h264::NalOutcome::PictureBoundary) => break,
                    Ok(_) => {}
                    Err(e) => panic!("{e}"),
                }
            }
            dec.end_picture().expect("end_picture");
        }
        let elapsed = start.elapsed();
        let ns_per_mb = elapsed.as_nanos() as f64 / f64::from(iters) / mbs as f64;
        let mpx_s = (w * h) as f64 * f64::from(iters) / elapsed.as_secs_f64() / 1e6;
        eprintln!(
            "PERF {name}: {w}x{h}, {mbs} MBs, {ns_per_mb:.0} ns/MB, {mpx_s:.1} Mpx/s \
             (intra IDR, single thread, deblock included, output copy excluded)"
        );
    }
}

/// Corrupt-input smoke sweep (fuzz-lite, deterministic): single-byte
/// mutations and truncations of a real stream must produce `Err` or a frame,
/// never a panic. A cargo-fuzz target owns the deep search later (S18 lane
/// done-criterion); this keeps the invariant enforced in every test run.
#[test]
fn corrupt_streams_never_panic() {
    let base = vectors_dir();
    if !base.is_dir() {
        eprintln!("SKIP: fixtures missing");
        return;
    }
    let stream = find_stream(&base.join("BA1_Sony_D")).expect("BA1_Sony_D fixture");
    let bytes = std::fs::read(stream).unwrap();

    // Truncations at prefix lengths spanning SPS/PPS/slice-header/slice-data.
    for len in (0..bytes.len().min(4096))
        .step_by(37)
        .chain([bytes.len() / 2])
    {
        let mut dec = Decoder::new();
        let _ = dec.decode_first_idr(&bytes[..len]);
    }
    // Single-byte corruptions: xor a walking pattern through the first 6KB
    // (covers parameter sets and the first slices), plus scattered hits.
    let mut xorshift = 0x243F_6A88u32; // deterministic PRNG, no rand dep
    // Debug builds carry overflow checks but decode ~10x slower; a smaller
    // sweep keeps the suite fast while release runs the full one.
    let count = if cfg!(debug_assertions) { 400 } else { 2000 };
    let dense = count * 3 / 4;
    for i in 0..count {
        let mut m = bytes.clone();
        let pos = if i < dense {
            i * 4 % m.len().min(6144)
        } else {
            xorshift ^= xorshift << 13;
            xorshift ^= xorshift >> 17;
            xorshift ^= xorshift << 5;
            (xorshift as usize) % m.len()
        };
        m[pos] ^= (0x5Bu8.wrapping_add(i as u8)).max(1);
        let mut dec = Decoder::new();
        let _ = dec.decode_first_idr(&m); // must not panic; any Err is fine
    }
}
