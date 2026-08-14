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

use ec_core::registry::{CodecId, CodecParameters, Decoder as _};
use ec_core::{Buf, Frame, Packet, TimeBase};
use ec_h264::{Decoder, Error, H264Decoder, NalOutcome};

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

/// Every vector directory `scripts/fetch-vectors.sh` has populated, in name
/// order.
///
/// A hand-written list would only ever record what passed on the day it was
/// written: a vector added later would be silently untested, and one that
/// regressed from bit-exact to refused would be invisible. Walking the
/// directory makes every fixture on disk a claim this test has to account for.
fn all_vectors(base: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(base)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

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

/// Pack a decoded frame as contiguous I420 bytes.
fn frame_bytes(frame: &ec_core::frame::VideoFrame) -> Vec<u8> {
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
    out
}

/// Decode a whole Annex B stream, returning every frame in display order.
fn decode_all(bytes: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
    let mut dec = Decoder::new();
    let mut frames = Vec::new();
    for nal in ec_h264_syntax::AnnexBIter::new(bytes) {
        if dec.push_nal(nal)? == NalOutcome::PictureBoundary {
            dec.end_picture()?;
            dec.push_nal(nal)?;
        }
        while let Some(f) = dec.next_frame() {
            frames.push(frame_bytes(&f));
        }
    }
    dec.flush()?;
    while let Some(f) = dec.next_frame() {
        frames.push(frame_bytes(&f));
    }
    Ok(frames)
}

/// Every frame ffmpeg decodes from `stream`, as raw I420.
fn ffmpeg_all_frames(stream: &Path, frame_len: usize) -> Option<Vec<Vec<u8>>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(stream)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    Some(
        out.stdout
            .chunks_exact(frame_len)
            .map(<[u8]>::to_vec)
            .collect(),
    )
}

/// Encode a multi-frame clip with x264 and return the Annex B file.
fn x264_encode_gop(
    dir: &Path,
    tag: &str,
    size: &str,
    frames: u32,
    extra: &[&str],
) -> Option<PathBuf> {
    let stream = dir.join(format!("{tag}.264"));
    let source = format!("testsrc=size={size}:rate=25:duration=4");
    let mut args: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.push(source);
    if !extra.contains(&"-pix_fmt") {
        args.extend(["-pix_fmt".into(), "yuv420p".into()]);
    }
    args.extend([
        "-c:v".into(),
        "libx264".into(),
        "-frames:v".into(),
        frames.to_string(),
    ]);
    args.extend(extra.iter().map(|s| s.to_string()));
    args.extend([
        "-f".into(),
        "h264".into(),
        stream.to_string_lossy().into_owned(),
    ]);
    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    run("ffmpeg", &refs).then_some(stream)
}

/// Decode `stream` both ways and report the first frame that differs.
fn compare_sequence(stream: &Path) -> Result<usize, String> {
    let bytes = std::fs::read(stream).map_err(|e| e.to_string())?;
    let ours = decode_all(&bytes).map_err(|e| format!("{e}"))?;
    let first = ours.first().ok_or("no frames decoded")?;
    let theirs = ffmpeg_all_frames(stream, first.len()).ok_or("ffmpeg cannot decode the stream")?;
    if ours.len() != theirs.len() {
        return Err(format!(
            "{} frames decoded, ffmpeg gave {}",
            ours.len(),
            theirs.len()
        ));
    }
    for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
        if let Some(pos) = first_diff(a, b) {
            return Err(format!(
                "frame {i} differs at byte {pos}: ours {} ffmpeg {}",
                a[pos], b[pos]
            ));
        }
    }
    Ok(ours.len())
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
    let mut refused = Vec::new();
    for name in all_vectors(&base) {
        let name = name.as_str();
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
                    passed.push(name.to_string());
                } else {
                    failed.push(name.to_string());
                }
            }
            // Outside this release's scope is allowed, but only when the
            // decoder says so by name. Corrupt or NeedMore on a conformant
            // stream is a bug here, not a capability statement.
            Err(e @ Error::Unsupported { .. }) => {
                eprintln!("REFUSED {name}: {e}");
                refused.push(name.to_string());
            }
            Err(e) => {
                eprintln!("FAIL {name}: decode error: {e}");
                failed.push(name.to_string());
            }
        }
    }
    eprintln!(
        "PASS table ({} bit-exact, {} refused by name): {passed:?}",
        passed.len(),
        refused.len()
    );
    assert!(
        failed.is_empty(),
        "streams neither bit-exact nor refused by name: {failed:?}"
    );
    assert!(
        passed.len() >= 5,
        "fewer than 5 CAVLC conformance streams verified: {passed:?}"
    );
}

/// Steady-state slice decoding performs zero heap allocations.
///
/// After a warm-up pass the picture pool, every per-picture array and the
/// parameter-set store are all sized, so decoding the same stream again must
/// not allocate at all: not per macroblock, not per slice, not per picture.
/// Emitting a frame is measured separately and *does* allocate — it hands the
/// caller owned planes — which also proves the counter is alive.
#[test]
fn steady_state_decode_loop_zero_alloc() {
    let base = vectors_dir();
    if !base.is_dir() {
        eprintln!("SKIP: fixtures missing");
        return;
    }
    // One stream per entropy coder, and one with B slices and multiple
    // reference frames so the decoded picture buffer really recycles pictures.
    for name in ["BA1_Sony_D", "CABA1_SVA_B", "CABA3_SVA_B"] {
        let Some(stream) = find_stream(&base.join(name)) else {
            continue;
        };
        let bytes = std::fs::read(stream).unwrap();
        let mut dec = Decoder::new();
        let drive = |dec: &mut Decoder, count: bool| -> usize {
            let nals: Vec<&[u8]> = ec_h264_syntax::AnnexBIter::new(&bytes).collect();
            let mut frames = 0usize;
            for nal in &nals {
                if count {
                    COUNTING_HERE.with(|c| c.set(true));
                }
                let outcome = dec.push_nal(nal);
                if count {
                    COUNTING_HERE.with(|c| c.set(false));
                }
                match outcome {
                    Ok(ec_h264::NalOutcome::PictureBoundary) => {
                        dec.end_picture().expect("end_picture");
                        dec.push_nal(nal).expect("re-push");
                    }
                    Ok(_) => {}
                    Err(e) => panic!("{name}: {e}"),
                }
                while dec.next_frame().is_some() {
                    frames += 1;
                }
            }
            dec.flush().expect("flush");
            while dec.next_frame().is_some() {
                frames += 1;
            }
            frames
        };
        // Warm-up: every reusable buffer reaches its final size.
        let warm = drive(&mut dec, false);
        assert!(warm > 1, "{name}: only {warm} frames decoded");
        dec.reset_pictures();

        ALLOCS.store(0, Ordering::SeqCst);
        let again = drive(&mut dec, true);
        let n = ALLOCS.load(Ordering::SeqCst);
        assert_eq!(
            again, warm,
            "{name}: frame count changed on the second pass"
        );
        assert_eq!(
            n, 0,
            "{name}: steady-state slice decode allocated {n} times"
        );
    }

    // Sanity: the counter is alive — emitting a frame does allocate.
    let bytes = std::fs::read(find_stream(&base.join("BA1_Sony_D")).unwrap()).unwrap();
    let mut dec = Decoder::new();
    let _ = dec.decode_first_idr(&bytes).expect("decode");
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

/// Decode-rate measurement: ns per macroblock and Mpx/s, printed for the perf
/// report. Run with `--nocapture` to see it. Uses the steady-state path
/// (buffers warm) like a playback loop would.
///
/// Both halves matter: the intra measurement is what the previous release
/// reported, and the whole-sequence one is what playback actually costs, since
/// a real stream is almost all inter pictures.
#[test]
fn ns_per_macroblock_measurement() {
    let base = vectors_dir();
    if !base.is_dir() {
        eprintln!("SKIP: fixtures missing");
        return;
    }
    for name in [
        "BA1_Sony_D",
        "CI_MW_D",
        "NL1_Sony_D",
        "CABA1_SVA_B",
        "CABA3_SVA_B",
    ] {
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
    perf_whole_sequence();
}

/// The same measurement over whole sequences, where inter prediction, the
/// decoded picture buffer and output all count.
fn perf_whole_sequence() {
    let base = vectors_dir();
    for name in ["BA_MW_D", "CABA3_SVA_B", "MR1_BT_A"] {
        let Some(stream) = find_stream(&base.join(name)) else {
            continue;
        };
        let bytes = std::fs::read(stream).unwrap();
        let Ok(warm) = decode_all(&bytes) else {
            continue;
        };
        if warm.is_empty() {
            continue;
        }
        let frames = warm.len() as f64;
        let start = std::time::Instant::now();
        let iters = 5u32;
        for _ in 0..iters {
            let _ = decode_all(&bytes).expect("decode");
        }
        let elapsed = start.elapsed().as_secs_f64() / f64::from(iters);
        // Every vector here is QCIF: 11 x 9 macroblocks.
        let mbs = 99.0 * frames;
        eprintln!(
            "PERF {name}: {frames} frames, {:.0} ns/MB, {:.1} Mpx/s \
             (full sequence, single thread, deblock and output copy included)",
            elapsed * 1e9 / mbs,
            176.0 * 144.0 * frames / elapsed / 1e6
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
    // One stream per entropy coder, plus one with B slices and multiple
    // references: CABAC's arithmetic decoder indexes context and coefficient
    // arrays from stream-derived values, and inter prediction indexes the
    // decoded picture buffer, the reference lists and the motion arrays from
    // them too. All of it needs the same no-panic floor as the CAVLC tables.
    for name in ["BA1_Sony_D", "CABA1_SVA_B", "CABA3_SVA_B", "BA_MW_D"] {
        let Some(stream) = find_stream(&base.join(name)) else {
            continue;
        };
        corrupt_sweep(&std::fs::read(stream).unwrap());
    }
}

/// Truncate and bit-flip `bytes` many ways; every decode must return, error or
/// succeed, and never panic.
fn corrupt_sweep(bytes: &[u8]) {
    let bytes = bytes.to_vec();

    // Truncations at prefix lengths spanning SPS/PPS/slice-header/slice-data.
    for len in (0..bytes.len().min(4096))
        .step_by(37)
        .chain([bytes.len() / 2])
    {
        let mut dec = Decoder::new();
        let _ = dec.decode_first_idr(&bytes[..len]);
        // Truncation must also be survivable on the full-sequence path, where
        // a half-decoded picture meets the decoded picture buffer.
        let _ = decode_all(&bytes[..len]);
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
        if i % 8 == 0 {
            // The whole-sequence path costs more, so it gets a thinner sweep;
            // it is the one that exercises reference lists and marking with
            // stream-derived indices.
            let _ = decode_all(&m);
        }
    }
}

// ---------------------------------------------------------------------------
// ffmpeg-driven coverage: the JVT vectors are all QCIF, single-slice and coded
// at a handful of quantisers, which leaves most of the deblocking tables, every
// non-QCIF geometry and every refusal unexercised. x264 fills those gaps, with
// ffmpeg's own decoder as the oracle.
// ---------------------------------------------------------------------------

/// Run a command, false when it fails or is not installed.
fn run(command: &str, args: &[&str]) -> bool {
    Command::new(command)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn have_ffmpeg() -> bool {
    run(
        "ffmpeg",
        &["-hide_banner", "-loglevel", "error", "-version"],
    )
}

/// A private scratch directory for one test.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ec-h264-{tag}-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Encode one all-intra picture with x264 and return the Annex B file.
fn x264_encode(dir: &Path, tag: &str, size: &str, extra: &[&str]) -> Option<PathBuf> {
    let stream = dir.join(format!("{tag}.264"));
    let source = format!("testsrc=size={size}:rate=1:duration=1");
    let mut args: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.push(source);
    // The pixel format has to precede the encoder arguments that name a
    // profile depending on it.
    if !extra.contains(&"-pix_fmt") {
        args.extend(["-pix_fmt".into(), "yuv420p".into()]);
    }
    args.extend([
        "-c:v".into(),
        "libx264".into(),
        "-frames:v".into(),
        "1".into(),
    ]);
    args.extend(extra.iter().map(|s| s.to_string()));
    args.extend([
        "-f".into(),
        "h264".into(),
        stream.to_string_lossy().into_owned(),
    ]);
    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    run("ffmpeg", &refs).then_some(stream)
}

/// Encode, decode both ways, return the number of differing bytes.
fn round_trip(dir: &Path, tag: &str, size: &str, extra: &[&str]) -> Result<usize, String> {
    let stream =
        x264_encode(dir, tag, size, extra).ok_or(format!("{tag}: ffmpeg cannot encode"))?;
    let expected =
        ffmpeg_first_frame(&stream).ok_or(format!("{tag}: ffmpeg cannot decode its own output"))?;
    let (ours, w, h) =
        decode_first(&std::fs::read(&stream).unwrap()).map_err(|e| format!("{tag}: {e}"))?;
    if ours.len() > expected.len() {
        return Err(format!(
            "{tag}: {w}x{h} is {} bytes, ffmpeg gave {}",
            ours.len(),
            expected.len()
        ));
    }
    Ok(ours
        .iter()
        .zip(&expected[..ours.len()])
        .filter(|(a, b)| a != b)
        .count())
}

/// The two entropy coders, as ffmpeg encoder arguments. Baseline forces CAVLC;
/// Main with `-coder ac` selects CABAC. Neither profile admits the 8x8
/// transform, so both stay inside this release's scope.
const ENTROPY_MODES: [(&str, &[&str]); 2] = [
    ("cavlc", &["-profile:v", "baseline"]),
    ("cabac", &["-profile:v", "main", "-coder", "ac"]),
];

/// Every quantisation parameter, bit-exact against ffmpeg, under both entropy
/// coders.
///
/// The deblocking filter's alpha, beta and tC0 tables (8-16 and 8-17) are
/// indexed by QP; the conformance vectors touch only a few of their 52 rows,
/// and a whole wrong column there passes the vectors (it did, once). One
/// all-intra picture per QP covers every row. Under CABAC the same sweep walks
/// the context initialisation of clause 9.3.1.1 across all 52 SliceQPY values,
/// which the fixed-QP conformance vectors barely touch.
#[test]
fn every_quantiser_matches_ffmpeg() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let dir = scratch("qp-sweep");
    let mut failures = Vec::new();
    for (mode, profile) in ENTROPY_MODES {
        let mut checked = 0usize;
        for qp in 1..=51u32 {
            let mut extra = profile.to_vec();
            let qp_string = qp.to_string();
            extra.extend(["-qp", &qp_string]);
            match round_trip(&dir, &format!("{mode}-q{qp}"), "176x144", &extra) {
                Ok(0) => checked += 1,
                Ok(n) => failures.push(format!("{mode} qp {qp}: {n} bytes differ")),
                Err(e) => failures.push(e),
            }
        }
        assert!(
            checked >= 40,
            "too few {mode} quantisers exercised: {checked}"
        );
        eprintln!("{mode}: {checked} quantisers bit-exact against ffmpeg");
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Geometry and slice structure: cropping, odd sizes, multiple slices, the
/// loop filter off and a chroma QP offset — what a real camera or encoder
/// produces, none of which the QCIF single-slice vectors cover.
#[test]
fn geometry_and_slice_structure_match_ffmpeg() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let dir = scratch("geometry");
    let cases: &[(&str, &str, &[&str])] = &[
        // 1080 lines are coded as 68 macroblock rows and cropped back to 1080.
        ("hd", "1920x1080", &["-qp", "26"]),
        // A width that is not a multiple of 16 crops horizontally as well.
        ("odd", "1916x1080", &["-qp", "26"]),
        // Four slices: neighbour availability stops at every slice boundary,
        // and under CABAC every slice re-initialises its own contexts.
        (
            "slices",
            "352x288",
            &["-qp", "24", "-x264-params", "slices=4"],
        ),
        // No loop filter at all.
        (
            "nodeblock",
            "352x288",
            &["-qp", "30", "-x264-params", "no-deblock=1"],
        ),
        // A non-zero chroma QP offset moves the chroma deblock thresholds.
        (
            "chromaqp",
            "352x288",
            &["-qp", "30", "-x264-params", "chroma_qp_offset=6"],
        ),
        // One macroblock wide: every neighbour is unavailable.
        ("tiny", "16x16", &["-qp", "20"]),
    ];
    let mut table = String::new();
    let mut failures = Vec::new();
    for (mode, profile) in ENTROPY_MODES {
        for (tag, size, case) in cases {
            let mut extra = profile.to_vec();
            extra.extend_from_slice(case);
            let tag = format!("{mode}-{tag}");
            match round_trip(&dir, &tag, size, &extra) {
                Ok(0) => table.push_str(&format!("{tag:<16} {size:<10} bit-exact\n")),
                Ok(n) => {
                    table.push_str(&format!("{tag:<16} {size:<10} MISMATCH {n} bytes\n"));
                    failures.push(format!("{tag}: {n} bytes differ"));
                }
                Err(e) => {
                    table.push_str(&format!("{tag:<16} {size:<10} {e}\n"));
                    failures.push(e);
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("{table}");
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Scaling matrices are decoded, not refused: a High-profile stream carrying
/// the JVT default matrices (`cqm=jvt`) must come out bit-exact.
///
/// Pinned because it is the one High-profile tool this release implements, and
/// nothing else in the suite would notice it regressing into a refusal.
#[test]
fn jvt_scaling_matrices_decode_bit_exact() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let dir = scratch("scaling");
    let extra = [
        "-profile:v",
        "high",
        "-qp",
        "26",
        "-x264-params",
        "cabac=0:8x8dct=0:cqm=jvt",
    ];
    let result = round_trip(&dir, "cqm-jvt", "176x144", &extra);
    let _ = std::fs::remove_dir_all(&dir);
    match result {
        Ok(0) => eprintln!("cqm=jvt scaling matrices bit-exact"),
        Ok(n) => panic!("cqm=jvt: {n} bytes differ from ffmpeg"),
        Err(e) => panic!("{e}"),
    }
}

/// Every refusal, proved against a stream that really uses the feature.
///
/// A "not supported" string is a claim about this binary; without a stream
/// that triggers it, it is just as likely to be a bug being hidden.
#[test]
fn refusals_name_a_feature_the_stream_really_uses() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let dir = scratch("refusals");
    // (tag, encoder arguments, the words the refusal must contain)
    let cases: &[(&str, &[&str], &str)] = &[
        // Field or macroblock-adaptive frame/field coding: the neighbour
        // derivation of 6.4.9 is not implemented, under either entropy coder.
        (
            "interlaced",
            &[
                "-profile:v",
                "main",
                "-coder",
                "ac",
                "-qp",
                "26",
                "-x264-params",
                "interlaced=1",
            ],
            "frame_mbs_only_flag 0",
        ),
        (
            "yuv422",
            &[
                "-profile:v",
                "high422",
                "-qp",
                "26",
                "-pix_fmt",
                "yuv422p",
                "-x264-params",
                "cabac=0",
            ],
            "chroma_format_idc 2",
        ),
        (
            "high10",
            &[
                "-profile:v",
                "high10",
                "-qp",
                "26",
                "-pix_fmt",
                "yuv420p10le",
                "-x264-params",
                "cabac=0",
            ],
            "10-bit",
        ),
    ];
    let mut table = String::new();
    let mut failures = Vec::new();
    let mut proved = 0usize;
    for (tag, extra, expected) in cases {
        let Some(stream) = x264_encode(&dir, tag, "176x144", extra) else {
            table.push_str(&format!(
                "{tag:<13} not encodable by this ffmpeg, skipped\n"
            ));
            continue;
        };
        match decode_first(&std::fs::read(&stream).unwrap()) {
            Err(Error::Unsupported { what, why }) => {
                let message = format!("{what} ({why})");
                if message.contains(expected) {
                    proved += 1;
                    table.push_str(&format!("{tag:<13} refused: {what}\n"));
                } else {
                    failures.push(format!("{tag}: refused as {message}, expected {expected}"));
                }
            }
            Err(e) => failures.push(format!("{tag}: expected Unsupported({expected}), got {e}")),
            Ok(_) => failures.push(format!("{tag}: decoded a stream it does not support")),
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("{table}");
    assert!(failures.is_empty(), "{failures:#?}");
    assert!(proved >= 2, "too few refusals proved against real streams");
}

/// The `avcC` entry path: parameter sets out of band and NAL units length
/// prefixed — how an MP4 or Matroska demuxer hands H.264 over.
#[test]
fn avcc_extradata_and_length_prefixed_packets() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let dir = scratch("avcc");
    let Some(annex_b) = x264_encode(
        &dir,
        "stream",
        "176x144",
        &["-profile:v", "baseline", "-qp", "26"],
    ) else {
        panic!("ffmpeg cannot encode a baseline picture");
    };
    let data = std::fs::read(&annex_b).unwrap();
    let units: Vec<Vec<u8>> = ec_h264_syntax::AnnexBIter::new(&data)
        .map(<[u8]>::to_vec)
        .collect();
    let sps = units.iter().find(|u| u[0] & 0x1F == 7).expect("an SPS");
    let pps = units.iter().find(|u| u[0] & 0x1F == 8).expect("a PPS");

    // avcC (ISO/IEC 14496-15 clause 5.3.3.1): version, profile, compatibility,
    // level, the NAL length size, then the parameter sets.
    let mut avcc = vec![1, sps[1], sps[2], sps[3], 0xFF, 0xE1];
    avcc.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    avcc.extend_from_slice(sps);
    avcc.push(1);
    avcc.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    avcc.extend_from_slice(pps);

    let mut params = CodecParameters::new(CodecId::H264);
    params.extradata = Some(Buf::from_vec(avcc));
    let mut decoder = H264Decoder::new(params).expect("avcC parses");
    assert_eq!(
        (
            decoder.codec_parameters().video().unwrap().width,
            decoder.codec_parameters().video().unwrap().height
        ),
        (176, 144),
        "the SPS inside the avcC sets the picture size, before any packet"
    );

    // The slice NAL units, each behind a four byte length prefix.
    let mut sample = Vec::new();
    for unit in units.iter().filter(|u| matches!(u[0] & 0x1F, 1 | 5)) {
        sample.extend_from_slice(&(unit.len() as u32).to_be_bytes());
        sample.extend_from_slice(unit);
    }
    decoder
        .send_packet(&Packet::new(0, TimeBase::new(1, 25), sample).with_pts(7))
        .expect("length prefixed packet decodes");
    let Frame::Video(frame) = decoder.receive_frame().expect("a frame") else {
        panic!("video frame expected");
    };
    assert_eq!((frame.width, frame.height), (176, 144));
    assert_eq!(
        frame.pts.map(|t| t.ticks),
        Some(7),
        "packet pts reaches the frame"
    );

    // Same picture through the Annex B path: the two entry paths agree.
    let (annex_b_frame, _, _) = decode_first(&data).unwrap();
    let mut avcc_frame = Vec::with_capacity(annex_b_frame.len());
    for (index, plane) in frame.planes.iter().enumerate() {
        let (w, h) = if index == 0 { (176, 144) } else { (88, 72) };
        for row in 0..h {
            avcc_frame.extend_from_slice(plane.row(row, w).expect("plane row"));
        }
    }
    assert_eq!(avcc_frame, annex_b_frame);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Inter prediction, the decoded picture buffer and display order.
// ---------------------------------------------------------------------------

/// A P-only GOP, every frame bit-exact against ffmpeg, under both entropy
/// coders. This is the narrowest thing that exercises motion compensation, the
/// reference list, sliding-window marking and the inter boundary strengths at
/// once, so it is the first test to look at when any of them regress.
#[test]
fn p_only_gop_matches_ffmpeg_every_frame() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let dir = scratch("p-gop");
    let mut failures = Vec::new();
    for (mode, profile) in ENTROPY_MODES {
        let mut extra = profile.to_vec();
        extra.extend(["-qp", "26", "-x264-params", "bframes=0:weightp=0:ref=1"]);
        let tag = format!("{mode}-p");
        match x264_encode_gop(&dir, &tag, "176x144", 12, &extra) {
            Some(stream) => match compare_sequence(&stream) {
                Ok(n) => eprintln!("{tag}: {n} frames bit-exact"),
                Err(e) => failures.push(format!("{tag}: {e}")),
            },
            None => failures.push(format!("{tag}: ffmpeg cannot encode")),
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{failures:#?}");
}

/// JVT conformance, every frame of every vector: our display-order output
/// against the reference decoder YUV that ships with the vector.
///
/// The first-IDR test above proves intra reconstruction; this one is what
/// proves inter prediction, the decoded picture buffer and output order,
/// because a single wrong motion vector or a reference marked at the wrong
/// moment shows up as a mismatch in some later frame, not the first.
#[test]
fn jvt_full_sequence_bit_exact() {
    let base = vectors_dir();
    if !base.is_dir() {
        eprintln!("SKIP: {} missing", base.display());
        return;
    }
    let mut table = String::new();
    let mut passed = 0usize;
    let mut refused = Vec::new();
    let mut failed = Vec::new();
    for name in all_vectors(&base) {
        let dir = base.join(&name);
        let Some(stream) = find_stream(&dir) else {
            continue;
        };
        let bytes = std::fs::read(&stream).unwrap();
        match decode_all(&bytes) {
            Ok(frames) if frames.is_empty() => {
                failed.push(format!("{name}: decoded no frames"));
            }
            Ok(frames) => {
                let len = frames[0].len();
                let Some(ref_path) = find_ref_yuv(&dir) else {
                    table.push_str(&format!(
                        "{name:<16} {:>3} frames, no reference YUV\n",
                        frames.len()
                    ));
                    continue;
                };
                let reference = std::fs::read(&ref_path).unwrap();
                let want = reference.len() / len;
                let mut bad = None;
                for (i, f) in frames.iter().enumerate() {
                    let Some(chunk) = reference.get(i * len..(i + 1) * len) else {
                        break;
                    };
                    if let Some(pos) = first_diff(f, chunk) {
                        bad = Some((i, pos));
                        break;
                    }
                }
                match bad {
                    None if frames.len() == want => {
                        passed += 1;
                        table.push_str(&format!(
                            "{name:<16} {:>3}/{want:<3} frames bit-exact\n",
                            frames.len()
                        ));
                    }
                    None => {
                        failed.push(format!(
                            "{name}: {} frames decoded, reference has {want}",
                            frames.len()
                        ));
                        table.push_str(&format!(
                            "{name:<16} {:>3}/{want:<3} FRAME COUNT\n",
                            frames.len()
                        ));
                    }
                    Some((i, pos)) => {
                        failed.push(format!("{name}: frame {i} differs at byte {pos}"));
                        table.push_str(&format!(
                            "{name:<16} {:>3}/{want:<3} MISMATCH frame {i}\n",
                            frames.len()
                        ));
                    }
                }
            }
            Err(e @ Error::Unsupported { .. }) => {
                refused.push(name.clone());
                table.push_str(&format!("{name:<16} refused: {e}\n"));
            }
            Err(e) => {
                failed.push(format!("{name}: {e}"));
                table.push_str(&format!("{name:<16} ERROR {e}\n"));
            }
        }
    }
    eprintln!("{table}");
    eprintln!(
        "full-sequence: {passed} bit-exact, {} refused by name, {} failed",
        refused.len(),
        failed.len()
    );
    assert!(failed.is_empty(), "{failed:#?}");
}

/// Decode a whole stream in decode order rather than display order.
fn decode_all_in_decode_order(bytes: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
    let mut dec = Decoder::new();
    dec.set_output_order(ec_h264::OutputOrder::Decode);
    let mut frames = Vec::new();
    for nal in ec_h264_syntax::AnnexBIter::new(bytes) {
        if dec.push_nal(nal)? == NalOutcome::PictureBoundary {
            dec.end_picture()?;
            dec.push_nal(nal)?;
        }
        while let Some(f) = dec.next_frame() {
            frames.push(frame_bytes(&f));
        }
    }
    dec.flush()?;
    while let Some(f) = dec.next_frame() {
        frames.push(frame_bytes(&f));
    }
    Ok(frames)
}

/// Count I, P and B slices in an Annex B stream, straight from the slice
/// headers. A test that claims to exercise B pictures has to prove the encoder
/// actually produced some — x264's adaptive decision drops them on flat
/// synthetic sources, and the test would then pass while measuring nothing.
fn slice_type_counts(bytes: &[u8]) -> [usize; 3] {
    let mut counts = [0usize; 3];
    for nal in ec_h264_syntax::AnnexBIter::new(bytes) {
        let Some((&header, payload)) = nal.split_first() else {
            continue;
        };
        if !matches!(header & 0x1F, 1 | 5) {
            continue;
        }
        let mut r = ec_core::BitReader::new(payload);
        if r.read_ue().is_err() {
            continue;
        }
        let Ok(code) = r.read_ue() else { continue };
        match code % 5 {
            0 => counts[1] += 1,
            1 => counts[2] += 1,
            2 => counts[0] += 1,
            _ => {}
        }
    }
    counts
}

/// A GOP with B pictures, every frame bit-exact against ffmpeg, under both
/// entropy coders and both direct modes.
///
/// B pictures are where the decoded picture buffer earns its keep: two
/// reference lists, direct prediction from a co-located picture, implicit
/// weighting, and pictures that leave the buffer in a different order than
/// they arrived.
#[test]
fn b_pictures_match_ffmpeg_every_frame() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let dir = scratch("b-gop");
    let cases: &[(&str, &[&str])] = &[
        (
            "spatial",
            &[
                "-x264-params",
                "bframes=3:b-adapt=0:direct=spatial:weightp=0",
            ],
        ),
        (
            "temporal",
            &[
                "-x264-params",
                "bframes=3:b-adapt=0:direct=temporal:weightp=0",
            ],
        ),
        // B pictures used as references, which makes the buffer hold pictures
        // that are neither the newest nor the oldest.
        (
            "pyramid",
            &[
                "-x264-params",
                "bframes=3:b-adapt=0:b-pyramid=normal:direct=spatial",
            ],
        ),
        // Weighted prediction on: explicit for P, implicit for B.
        (
            "weighted",
            &[
                "-x264-params",
                "bframes=3:b-adapt=0:weightp=2:weightb=1:direct=spatial",
            ],
        ),
    ];
    let mut failures = Vec::new();
    let mut table = String::new();
    for (mode, profile) in ENTROPY_MODES {
        if mode == "cavlc" {
            continue; // baseline forbids B pictures; main covers CAVLC below
        }
        for (tag, case) in cases {
            for coder in ["ac", "0"] {
                let mut extra = vec!["-profile:v", "main", "-coder", coder, "-qp", "26"];
                extra.extend_from_slice(case);
                let name = format!("{tag}-{}", if coder == "ac" { "cabac" } else { "cavlc" });
                match x264_encode_gop(&dir, &name, "176x144", 20, &extra) {
                    Some(stream) => {
                        let counts = slice_type_counts(&std::fs::read(&stream).unwrap());
                        if counts[2] == 0 {
                            failures.push(format!("{name}: encoder produced no B slices"));
                            continue;
                        }
                        match compare_sequence(&stream) {
                            Ok(n) => table.push_str(&format!(
                                "{name:<18} {n:>3} frames bit-exact ({} B slices)\n",
                                counts[2]
                            )),
                            Err(e) => failures.push(format!("{name}: {e}")),
                        }
                    }
                    None => failures.push(format!("{name}: ffmpeg cannot encode")),
                }
            }
        }
        let _ = profile;
    }
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("{table}");
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Frames come out in display order, and that is a real reordering.
///
/// Matching ffmpeg frame for frame already implies display order, but only if
/// the stream actually reorders — so this also decodes the same stream in
/// decode order and requires the two to differ. Without that second half the
/// test would pass on a decoder that never reorders anything.
#[test]
fn output_is_display_order_and_the_reorder_is_real() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let dir = scratch("order");
    let extra = [
        "-profile:v",
        "main",
        "-coder",
        "ac",
        "-qp",
        "26",
        "-x264-params",
        "bframes=3:b-adapt=0:b-pyramid=normal",
    ];
    let stream = x264_encode_gop(&dir, "reorder", "176x144", 20, &extra).expect("encode");
    let bytes = std::fs::read(&stream).unwrap();
    let counts = slice_type_counts(&bytes);
    assert!(counts[2] > 0, "the encoder produced no B slices to reorder");
    let display = decode_all(&bytes).expect("display order decode");
    let decode = decode_all_in_decode_order(&bytes).expect("decode order decode");
    let ffmpeg = ffmpeg_all_frames(&stream, display[0].len()).expect("ffmpeg");
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(display.len(), ffmpeg.len());
    assert_eq!(decode.len(), display.len());
    for (i, (a, b)) in display.iter().zip(&ffmpeg).enumerate() {
        assert!(
            first_diff(a, b).is_none(),
            "display-order frame {i} differs from ffmpeg"
        );
    }
    // Every decode-order frame is somewhere in the display-order set: the two
    // are permutations of one another, not different pictures.
    for (i, f) in decode.iter().enumerate() {
        assert!(
            display.iter().any(|d| d == f),
            "decode-order frame {i} is not in the display-order output"
        );
    }
    let moved = decode.iter().zip(&display).filter(|(a, b)| a != b).count();
    assert!(
        moved > 0,
        "decode order and display order are identical: the stream does not \
         reorder, so this test proves nothing"
    );
    eprintln!(
        "{} of {} frames move under reordering",
        moved,
        display.len()
    );
}

/// A gap in frame_num decodes instead of stalling (clause 8.2.5.2).
///
/// This is what a seek into the middle of a GOP looks like, and what a lossy
/// transport delivers: the pictures a later frame_num implies never arrived.
/// The spec infers them; a decoder that treats the gap as corruption drops
/// every following picture, which is the failure an editor sees as a frozen
/// preview after a seek.
#[test]
fn a_frame_num_gap_keeps_decoding() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let dir = scratch("gap");
    let extra = [
        "-profile:v",
        "baseline",
        "-qp",
        "26",
        "-x264-params",
        "bframes=0:ref=1:keyint=100",
    ];
    let stream = x264_encode_gop(&dir, "gap", "176x144", 12, &extra).expect("encode");
    let bytes = std::fs::read(&stream).unwrap();
    let units: Vec<Vec<u8>> = ec_h264_syntax::AnnexBIter::new(&bytes)
        .map(<[u8]>::to_vec)
        .collect();
    let _ = std::fs::remove_dir_all(&dir);

    // Drop the fourth picture entirely, leaving frame_num 3 missing.
    let mut vcl_seen = 0;
    let mut dec = Decoder::new();
    let mut frames = 0usize;
    let mut errors = Vec::new();
    for unit in &units {
        if matches!(unit[0] & 0x1F, 1 | 5) {
            vcl_seen += 1;
            if vcl_seen == 4 {
                continue; // the lost picture
            }
        }
        match dec.push_nal(unit) {
            Ok(NalOutcome::PictureBoundary) => {
                dec.end_picture().expect("end_picture");
                dec.push_nal(unit).expect("re-push");
            }
            Ok(_) => {}
            Err(e) => errors.push(format!("{e}")),
        }
        while dec.next_frame().is_some() {
            frames += 1;
        }
    }
    dec.flush().expect("flush");
    while dec.next_frame().is_some() {
        frames += 1;
    }
    assert!(errors.is_empty(), "decoding stopped at the gap: {errors:?}");
    // 12 pictures coded, one dropped; the gap is filled by an inferred frame,
    // so every remaining picture still decodes and is output.
    assert!(
        frames >= 11,
        "only {frames} frames survived a single dropped picture"
    );
    eprintln!("frame_num gap: {frames} frames decoded after dropping one picture");
}

/// Every quantisation parameter again, this time over a GOP with motion.
///
/// The intra sweep above walks the alpha, beta and tC0 rows the bS 3 and 4
/// edges use. Boundary strengths 1 and 2 exist only between inter macroblocks,
/// so their tC0 columns (Table 8-17) had no oracle at all until now — and both
/// were wrong when this landed. One P/B GOP per quantiser covers every row of
/// both columns, and under CABAC it also walks the cabac_init_idc columns
/// across all 52 SliceQPY values.
#[test]
fn every_quantiser_with_motion_matches_ffmpeg() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let dir = scratch("qp-motion");
    let mut failures = Vec::new();
    for coder in ["ac", "0"] {
        let mut checked = 0usize;
        for qp in 1..=51u32 {
            let qp_string = qp.to_string();
            let extra = vec![
                "-profile:v",
                "main",
                "-coder",
                coder,
                "-qp",
                &qp_string,
                "-x264-params",
                "bframes=2:b-adapt=0:ref=2",
            ];
            let tag = format!("{coder}-q{qp}");
            match x264_encode_gop(&dir, &tag, "176x144", 8, &extra) {
                Some(stream) => match compare_sequence(&stream) {
                    Ok(_) => checked += 1,
                    Err(e) => failures.push(format!("{tag}: {e}")),
                },
                None => failures.push(format!("{tag}: ffmpeg cannot encode")),
            }
        }
        assert!(
            checked >= 40,
            "too few {coder} quantisers exercised: {checked}"
        );
        eprintln!("{coder}: {checked} quantisers bit-exact over a GOP with motion");
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Geometry and slice structure, over a GOP rather than one picture: cropping,
/// odd sizes, multiple slices per picture and the loop filter off, all with
/// inter prediction and reordering active.
#[test]
fn geometry_with_motion_matches_ffmpeg() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let dir = scratch("geometry-motion");
    let cases: &[(&str, &str, &[&str])] = &[
        ("hd", "1920x1080", &["bframes=2:b-adapt=0"]),
        ("odd", "1916x1078", &["bframes=2:b-adapt=0"]),
        // Neighbour availability stops at every slice boundary, and a motion
        // vector predictor at one sees no partition across it.
        ("slices", "352x288", &["bframes=2:b-adapt=0:slices=4"]),
        (
            "nodeblock",
            "352x288",
            &["bframes=2:b-adapt=0:no-deblock=1"],
        ),
        // One macroblock wide: every spatial neighbour is unavailable, which
        // is where the reference index of a missing partition matters most.
        ("tiny", "16x16", &["bframes=2:b-adapt=0"]),
        // Many references, so the list is long and reordering is real work.
        (
            "manyref",
            "352x288",
            &["bframes=3:b-adapt=0:ref=5:b-pyramid=normal"],
        ),
    ];
    let mut table = String::new();
    let mut failures = Vec::new();
    for coder in ["ac", "0"] {
        for (tag, size, params) in cases {
            let extra = vec![
                "-profile:v",
                "main",
                "-coder",
                coder,
                "-qp",
                "26",
                "-x264-params",
                params[0],
            ];
            let name = format!("{coder}-{tag}");
            match x264_encode_gop(&dir, &name, size, 10, &extra) {
                Some(stream) => match compare_sequence(&stream) {
                    Ok(n) => {
                        table.push_str(&format!("{name:<14} {size:<10} {n:>2} frames bit-exact\n"))
                    }
                    Err(e) => {
                        table.push_str(&format!("{name:<14} {size:<10} {e}\n"));
                        failures.push(format!("{name}: {e}"));
                    }
                },
                None => failures.push(format!("{name}: ffmpeg cannot encode")),
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("{table}");
    assert!(failures.is_empty(), "{failures:#?}");
}

/// SP and SI slices are refused by name, proved against a stream that really
/// carries one.
///
/// No encoder here emits switching slices, so the stream is hand built: an SPS
/// and PPS this decoder accepts, then a slice header whose slice_type says SP.
/// Without it the refusal would be an untested string, which is how a refusal
/// becomes a hidden bug.
#[test]
fn switching_slices_are_refused_by_name() {
    use ec_core::BitWriter;

    let mut annexb: Vec<u8> = Vec::new();
    let mut nal = |kind: u8, ref_idc: u8, payload: &[u8]| {
        annexb.extend_from_slice(&[0, 0, 0, 1]);
        annexb.push((ref_idc << 5) | kind);
        annexb.extend_from_slice(payload);
    };

    let mut w = BitWriter::new();
    w.write_bits(66, 8); // profile_idc baseline
    w.write_bits(0, 8);
    w.write_bits(30, 8); // level_idc
    w.write_ue(0); // sps id
    w.write_ue(0); // log2_max_frame_num_minus4
    w.write_ue(2); // pic_order_cnt_type 2
    w.write_ue(1); // max_num_ref_frames
    w.write_bit(false); // gaps_in_frame_num_value_allowed
    w.write_ue(10); // pic_width_in_mbs_minus1 -> 176
    w.write_ue(8); // pic_height_in_map_units_minus1 -> 144
    w.write_bit(true); // frame_mbs_only
    w.write_bit(true); // direct_8x8_inference
    w.write_bit(false); // no cropping
    w.write_bit(false); // no vui
    w.write_bit(true); // rbsp stop bit
    w.align_to_byte();
    nal(7, 3, w.as_bytes());

    let mut w = BitWriter::new();
    w.write_ue(0); // pps id
    w.write_ue(0); // sps id
    w.write_bit(false); // entropy_coding_mode
    w.write_bit(false); // bottom_field_pic_order_in_frame_present
    w.write_ue(0); // num_slice_groups_minus1
    w.write_ue(0); // num_ref_idx_l0_default_active_minus1
    w.write_ue(0); // num_ref_idx_l1_default_active_minus1
    w.write_bit(false); // weighted_pred
    w.write_bits(0, 2); // weighted_bipred_idc
    w.write_se(0); // pic_init_qp_minus26
    w.write_se(0); // pic_init_qs_minus26
    w.write_se(0); // chroma_qp_index_offset
    w.write_bit(false); // deblocking_filter_control_present
    w.write_bit(false); // constrained_intra_pred
    w.write_bit(false); // redundant_pic_cnt_present
    w.write_bit(true); // rbsp stop bit
    w.align_to_byte();
    nal(8, 3, w.as_bytes());

    // A slice whose slice_type is SP (3), which is what has to be refused.
    let mut w = BitWriter::new();
    w.write_ue(0); // first_mb_in_slice
    w.write_ue(3); // slice_type: SP
    w.write_ue(0); // pps id
    w.write_bits(0, 4); // frame_num
    w.write_bit(false); // num_ref_idx_active_override_flag
    w.write_bit(false); // ref_pic_list_modification_flag_l0
    w.write_bit(false); // adaptive_ref_pic_marking_mode_flag
    w.write_bit(false); // sp_for_switch_flag
    w.write_se(0); // slice_qs_delta
    w.write_se(0); // slice_qp_delta
    w.write_bit(true);
    w.align_to_byte();
    nal(1, 2, w.as_bytes());

    let mut dec = Decoder::new();
    let mut refusal = None;
    for unit in ec_h264_syntax::AnnexBIter::new(&annexb) {
        if let Err(e) = dec.push_nal(unit) {
            refusal = Some(e);
            break;
        }
    }
    match refusal {
        Some(Error::Unsupported { what, why }) => {
            assert!(
                what.contains("SP and SI"),
                "refused as {what} ({why}), expected the switching-slice refusal"
            );
        }
        other => panic!("expected an SP-slice refusal, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Real library: the files actually on this machine, not fixtures.
// ---------------------------------------------------------------------------

/// Compare a long stream against ffmpeg without holding it in memory: ffmpeg's
/// raw output is consumed from a pipe one frame at a time, in step with our
/// own decoding, so a 500-frame 1080p sweep costs two frames of memory rather
/// than three gigabytes.
fn compare_sequence_streamed(
    stream: &Path,
    frames_wanted: usize,
) -> Result<StreamedResult, String> {
    use std::io::Read;

    let bytes = std::fs::read(stream).map_err(|e| e.to_string())?;
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(stream)
        .args([
            "-frames:v",
            &frames_wanted.to_string(),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut out = child.stdout.take().expect("piped stdout");

    let mut dec = Decoder::new();
    let mut reference = Vec::new();
    let mut result = StreamedResult::default();
    let mut oracle_open = true;
    let mut take = |frame: Vec<u8>, result: &mut StreamedResult, reference: &mut Vec<u8>| {
        result.produced += 1;
        if !oracle_open || result.compared >= frames_wanted {
            return;
        }
        reference.resize(frame.len(), 0);
        if out.read_exact(reference).is_err() {
            oracle_open = false;
            return;
        }
        if let Some(pos) = first_diff(&frame, reference) {
            result.diffs.push((result.compared, pos));
        }
        result.compared += 1;
    };

    for nal in ec_h264_syntax::AnnexBIter::new(&bytes) {
        match dec.push_nal(nal) {
            Ok(NalOutcome::PictureBoundary) => {
                dec.end_picture().map_err(|e| format!("{e}"))?;
                dec.push_nal(nal).map_err(|e| format!("{e}"))?;
            }
            Ok(_) => {}
            Err(e) => return Err(format!("{e}")),
        }
        while let Some(f) = dec.next_frame() {
            take(frame_bytes(&f), &mut result, &mut reference);
        }
    }
    dec.flush().map_err(|e| format!("{e}"))?;
    while let Some(f) = dec.next_frame() {
        take(frame_bytes(&f), &mut result, &mut reference);
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok(result)
}

/// Outcome of a streamed comparison.
#[derive(Default)]
struct StreamedResult {
    /// Frames this decoder produced.
    produced: usize,
    /// Frames the oracle also produced, and which were therefore compared.
    compared: usize,
    /// `(frame index, byte offset)` of each mismatch.
    diffs: Vec<(usize, usize)>,
}

/// Extract the video elementary stream of a real file as Annex B.
fn extract_annexb(src: &Path, dst: &Path, frames: usize) -> bool {
    run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-y",
            "-i",
            &src.to_string_lossy(),
            "-map",
            "0:v:0",
            "-c",
            "copy",
            "-bsf:v",
            "h264_mp4toannexb",
            "-frames:v",
            &frames.to_string(),
            "-f",
            "h264",
            &dst.to_string_lossy(),
        ],
    )
}

/// The same comparison, but demuxed from the container by [`ec_mp4`] instead
/// of extracted to Annex B by ffmpeg's bitstream filter.
///
/// Some real MP4s produce an elementary stream that ffmpeg itself then decodes
/// only three frames of, silently. That is a property of the extraction, not of
/// the file: reading the sample table directly and handing the `avcC` and the
/// length-prefixed samples to the packet entry surface leaves the oracle able to
/// decode the original file, so the comparison is real again.
fn compare_container_streamed(
    source: &Path,
    frames_wanted: usize,
) -> Result<StreamedResult, String> {
    use ec_core::registry::{Decoder as _, Demuxer as _};
    use std::io::Read;

    let file = std::fs::File::open(source).map_err(|e| e.to_string())?;
    let mut demux = ec_mp4::Mp4Demuxer::new(std::io::BufReader::new(file))
        .map_err(|e| format!("ec-mp4 open: {e}"))?;
    let (stream_index, params) = demux
        .streams()
        .iter()
        .find(|s| s.params.codec == ec_core::registry::CodecId::H264)
        .map(|s| (s.index, s.params.clone()))
        .ok_or("no H.264 track")?;
    let mut dec = ec_h264::H264Decoder::new(params).map_err(|e| format!("{e}"))?;

    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(source)
        .args([
            "-map",
            "0:v:0",
            "-frames:v",
            &frames_wanted.to_string(),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut out = child.stdout.take().expect("piped stdout");

    let mut reference = Vec::new();
    let mut result = StreamedResult::default();
    let mut oracle_open = true;
    let mut take = |frame: Vec<u8>, result: &mut StreamedResult, reference: &mut Vec<u8>| {
        result.produced += 1;
        if !oracle_open || result.compared >= frames_wanted {
            return;
        }
        reference.resize(frame.len(), 0);
        if out.read_exact(reference).is_err() {
            oracle_open = false;
            return;
        }
        if let Some(pos) = first_diff(&frame, reference) {
            result.diffs.push((result.compared, pos));
        }
        result.compared += 1;
    };
    let mut drain =
        |dec: &mut ec_h264::H264Decoder, result: &mut StreamedResult, reference: &mut Vec<u8>| {
            while let Ok(ec_core::frame::Frame::Video(f)) = dec.receive_frame() {
                take(frame_bytes(&f), result, reference);
            }
        };
    while result.produced < frames_wanted {
        let packet = match demux.next_packet() {
            Ok(p) => p,
            Err(_) => break,
        };
        if packet.stream != stream_index {
            continue;
        }
        dec.send_packet(&packet).map_err(|e| format!("{e}"))?;
        drain(&mut dec, &mut result, &mut reference);
    }
    dec.flush().map_err(|e| format!("{e}"))?;
    drain(&mut dec, &mut result, &mut reference);
    let _ = child.kill();
    let _ = child.wait();
    Ok(result)
}

/// Decode real H.264 files from this machine's library, several hundred frames
/// each, frame for frame against ffmpeg.
///
/// Synthetic clips and conformance vectors are both written by people trying to
/// exercise a decoder. Real files are written by encoders tuned for size, at
/// resolutions and GOP structures the fixtures never use, and they are the ones
/// a user actually opens. Every file either decodes bit-exactly or is refused
/// by name; a file that decodes to different pixels is a failure.
#[test]
fn real_library_streams_match_ffmpeg() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let manifest =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/real-library-manifest.tsv");
    let Ok(text) = std::fs::read_to_string(&manifest) else {
        eprintln!("SKIP: {} missing", manifest.display());
        return;
    };
    let dir = scratch("real-library");
    const FRAMES: usize = 500;
    /// A file counts as swept only when the oracle stayed with us this long.
    const MIN_FRAMES: usize = 100;
    let mut table = String::new();
    let mut decoded = 0usize;
    let mut refused = 0usize;
    let mut failures = Vec::new();
    let mut seen_containers = std::collections::BTreeSet::new();

    for line in text.lines().skip(1) {
        let mut f = line.split('\t');
        let (Some(path), Some(container), Some(vcodec)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        if vcodec != "h264" || !Path::new(path).is_file() {
            continue;
        }
        // One file per container family per resolution class keeps the sweep
        // to a few minutes while still covering both demuxers.
        let width = f.next().unwrap_or("0");
        let key = (container.to_string(), width.to_string());
        if !seen_containers.insert(key) {
            continue;
        }
        let name: String = Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().chars().take(38).collect())
            .unwrap_or_default();
        let annexb = dir.join("stream.264");
        if !extract_annexb(Path::new(path), &annexb, FRAMES) {
            table.push_str(&format!("{name:<40} {container:<12} not extractable\n"));
            continue;
        }
        match compare_sequence_streamed(&annexb, FRAMES) {
            Ok(r) if !r.diffs.is_empty() => {
                failures.push(format!(
                    "{name}: {} of {} frames differ",
                    r.diffs.len(),
                    r.compared
                ));
                table.push_str(&format!(
                    "{name:<40} {container:<12} {width:>5}px MISMATCH {} of {}\n",
                    r.diffs.len(),
                    r.compared
                ));
            }
            // The oracle stopped before we did: ffmpeg gave up on the elementary
            // stream its own bitstream filter produced. That is a property of
            // the extraction, so the file is re-run straight out of the
            // container, where the oracle can read the original samples.
            Ok(r) if r.compared < r.produced.min(MIN_FRAMES) => {
                match compare_container_streamed(Path::new(path), FRAMES) {
                    Ok(c) if !c.diffs.is_empty() => {
                        failures.push(format!(
                            "{name} (container demux): {} of {} frames differ",
                            c.diffs.len(),
                            c.compared
                        ));
                        table.push_str(&format!(
                            "{name:<40} {container:<12} {width:>5}px MISMATCH {} of {} \
                             (container demux)\n",
                            c.diffs.len(),
                            c.compared
                        ));
                    }
                    Ok(c) if c.compared >= MIN_FRAMES => {
                        decoded += 1;
                        table.push_str(&format!(
                            "{name:<40} {container:<12} {width:>5}px {:>4} frames bit-exact \
                             (container demux)\n",
                            c.compared
                        ));
                    }
                    Ok(c) => {
                        table.push_str(&format!(
                            "{name:<40} {container:<12} {width:>5}px oracle short both ways: \
                             ffmpeg gave {} frames, we decoded {}\n",
                            c.compared, c.produced
                        ));
                    }
                    Err(e) => {
                        table.push_str(&format!(
                            "{name:<40} {container:<12} {width:>5}px oracle short, container \
                             demux failed: {e}\n"
                        ));
                    }
                }
            }
            Ok(r) => {
                decoded += 1;
                table.push_str(&format!(
                    "{name:<40} {container:<12} {width:>5}px {:>4} frames bit-exact\n",
                    r.compared
                ));
            }
            Err(e) if e.contains("unsupported") => {
                refused += 1;
                table.push_str(&format!(
                    "{name:<40} {container:<12} {width:>5}px refused: {e}\n"
                ));
            }
            Err(e) => {
                failures.push(format!("{name}: {e}"));
                table.push_str(&format!(
                    "{name:<40} {container:<12} {width:>5}px ERROR {e}\n"
                ));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("{table}");
    eprintln!("real library: {decoded} decoded bit-exact, {refused} refused by name");
    assert!(failures.is_empty(), "{failures:#?}");
    assert!(
        decoded >= 3,
        "only {decoded} real files swept end to end; the sweep is the point"
    );
}

/// The registry entry surface, driven the way a demuxer drives it: one access
/// unit per packet, frames pulled with `receive_frame`.
///
/// This is where display order has to actually arrive. Everything above tests
/// the NAL-level API; a player never touches that one, so a reordering decoder
/// whose packet API still hands back decode order would look correct in every
/// other test here and wrong on screen.
#[test]
fn packet_entry_surface_reorders_and_carries_timestamps() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let dir = scratch("entry");
    let extra = [
        "-profile:v",
        "main",
        "-coder",
        "ac",
        "-qp",
        "26",
        "-x264-params",
        "bframes=3:b-adapt=0:b-pyramid=normal",
    ];
    let stream = x264_encode_gop(&dir, "entry", "176x144", 16, &extra).expect("encode");
    let data = std::fs::read(&stream).unwrap();
    assert!(
        slice_type_counts(&data)[2] > 0,
        "the encoder produced no B slices"
    );

    // Split the stream into access units the way a demuxer would: a new one
    // starts at each parameter set or first slice of a picture.
    let units: Vec<Vec<u8>> = ec_h264_syntax::AnnexBIter::new(&data)
        .map(<[u8]>::to_vec)
        .collect();
    let mut access_units: Vec<Vec<u8>> = Vec::new();
    let mut current: Vec<u8> = Vec::new();
    let mut current_has_slice = false;
    for unit in &units {
        let is_slice = matches!(unit[0] & 0x1F, 1 | 5);
        if is_slice && current_has_slice {
            access_units.push(std::mem::take(&mut current));
            current_has_slice = false;
        }
        current_has_slice |= is_slice;
        current.extend_from_slice(&[0, 0, 0, 1]);
        current.extend_from_slice(unit);
    }
    if current_has_slice {
        access_units.push(current);
    }

    let time_base = TimeBase::new(1, 25);
    let mut decoder = H264Decoder::new(CodecParameters::new(CodecId::H264)).expect("decoder");
    let mut out: Vec<(i64, Vec<u8>)> = Vec::new();
    for (i, au) in access_units.iter().enumerate() {
        let packet = Packet::new(0, time_base, au.clone()).with_pts(i as i64);
        decoder.send_packet(&packet).expect("send_packet");
        while let Ok(Frame::Video(f)) = decoder.receive_frame() {
            out.push((f.pts.map(|t| t.ticks).unwrap_or(-1), frame_bytes(&f)));
        }
    }
    decoder.flush().expect("flush");
    while let Ok(Frame::Video(f)) = decoder.receive_frame() {
        out.push((f.pts.map(|t| t.ticks).unwrap_or(-1), frame_bytes(&f)));
    }

    let expected = ffmpeg_all_frames(&stream, out[0].1.len()).expect("ffmpeg");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.len(), expected.len(), "frame count");
    for (i, ((_, ours), theirs)) in out.iter().zip(&expected).enumerate() {
        assert!(
            first_diff(ours, theirs).is_none(),
            "packet-path frame {i} differs from ffmpeg"
        );
    }
    // Each packet was tagged with its own index in *decode* order, so the tag
    // that comes back on a picture says which packet produced it. Decoding the
    // same stream in decode order gives the pixels for each of those indices,
    // and the two have to line up: that is the timestamp staying with its
    // picture across the reordering, not merely arriving in some order.
    let by_decode_order = decode_all_in_decode_order(&data).expect("decode order");
    assert_eq!(by_decode_order.len(), out.len());
    for (position, (tag, pixels)) in out.iter().enumerate() {
        let tag = usize::try_from(*tag).expect("a tag was lost");
        assert!(
            first_diff(pixels, &by_decode_order[tag]).is_none(),
            "output position {position} carries the timestamp of packet {tag}, \
             but its pixels are a different picture"
        );
    }
    let order: Vec<i64> = out.iter().map(|(pts, _)| *pts).collect();
    assert_ne!(
        order,
        (0..order.len() as i64).collect::<Vec<_>>(),
        "output order equals packet order, so no reordering happened"
    );
    eprintln!(
        "packet entry surface: {} frames, packets emitted in order {order:?}",
        out.len()
    );
}

/// Spatial direct prediction whose co-located picture is itself a B picture.
///
/// With a B pyramid the picture at RefPicList1[0] of the first B in a run is a
/// coded B picture, so its blocks are bi-predicted and carry two reference
/// indices. colZeroFlag (8.4.1.2.2) reads refIdxCol — the index of the list
/// 8.4.1.2.1 selected — and a decoder that instead accepts "either index is 0"
/// zeroes motion vectors that must keep their prediction. Several references
/// are what makes the two readings differ at all, so this needs `-refs`.
#[test]
fn spatial_direct_over_a_b_reference_matches_ffmpeg() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let dir = scratch("direct-bref");
    let mut failures = Vec::new();
    for (tag, coder) in [("cabac", "ac"), ("cavlc", "0")] {
        let extra = [
            "-profile:v",
            "high",
            "-coder",
            coder,
            "-qp",
            "24",
            "-bf",
            "3",
            "-refs",
            "4",
            "-x264-params",
            "b-adapt=0:b-pyramid=normal:direct=spatial:8x8dct=1",
        ];
        match x264_encode_gop(&dir, tag, "352x288", 40, &extra) {
            None => failures.push(format!("{tag}: ffmpeg cannot encode")),
            Some(stream) => match compare_sequence(&stream) {
                Ok(n) => eprintln!("b-pyramid direct {tag}: {n} frames bit-exact"),
                Err(e) => failures.push(format!("{tag}: {e}")),
            },
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{failures:#?}");
}

/// True when the stream's picture parameter set enables the 8x8 transform.
///
/// Without this the sweep below would silently prove nothing: an encoder that
/// quietly dropped back to 4x4 would pass every comparison while never coding
/// a single transform_size_8x8_flag.
fn pps_enables_8x8(stream: &Path) -> bool {
    let bytes = std::fs::read(stream).unwrap_or_default();
    let mut rbsp = Vec::new();
    let mut sps = None;
    for nal in ec_h264_syntax::AnnexBIter::new(&bytes) {
        let Some((&header, payload)) = nal.split_first() else {
            continue;
        };
        let Ok(h) = ec_h264_syntax::NalHeader::parse(header) else {
            continue;
        };
        ec_h264_syntax::unescape_rbsp(payload, &mut rbsp);
        match h.unit_type {
            ec_h264_syntax::NalUnitType::Sps => sps = ec_h264_syntax::Sps::parse(&rbsp).ok(),
            ec_h264_syntax::NalUnitType::Pps => {
                if let Ok(p) = ec_h264_syntax::Pps::parse(&rbsp, |_| sps.as_ref()) {
                    return p.transform_8x8_mode;
                }
            }
            _ => {}
        }
    }
    false
}

/// The High-profile 8x8 transform, which every real-world encoder turns on by
/// default: transform_size_8x8_flag, Intra_8x8 prediction with its reference
/// filter (8.3.2), the 8x8 residual under both entropy coders and the 8x8
/// inverse transform of 8.5.13.
///
/// The quantiser sweep matters twice over here: the 8x8 dequant ladder of
/// 8.5.13.1 has its own rounding branch at QP 36, and under CABAC the whole
/// ctxIdx 402..435 initialisation is a function of SliceQPY.
#[test]
fn high_profile_8x8_matches_ffmpeg() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let dir = scratch("high8x8");
    let mut failures = Vec::new();
    // High profile with the 8x8 transform forced on, in both entropy coders.
    let modes: [(&str, &[&str]); 2] = [
        ("cabac", &["-profile:v", "high", "-coder", "ac"]),
        ("cavlc", &["-profile:v", "high", "-coder", "0"]),
    ];
    for (mode, profile) in modes {
        let mut checked = 0usize;
        for qp in 1..=51u32 {
            let mut extra = profile.to_vec();
            let qp_string = qp.to_string();
            extra.extend(["-qp", &qp_string, "-x264-params", "8x8dct=1:i8x8=1"]);
            let tag = format!("i8-{mode}-q{qp}");
            match round_trip(&dir, &tag, "176x144", &extra) {
                Ok(0) => {
                    assert!(
                        pps_enables_8x8(&dir.join(format!("{tag}.264"))),
                        "{tag}: encoder did not enable the 8x8 transform"
                    );
                    checked += 1;
                }
                Ok(n) => failures.push(format!("intra {mode} qp {qp}: {n} bytes differ")),
                Err(e) => failures.push(e),
            }
        }
        assert!(
            checked >= 40,
            "too few {mode} quantisers exercised: {checked}"
        );
        eprintln!("8x8 intra {mode}: {checked} quantisers bit-exact against ffmpeg");
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The same transform in inter macroblocks: P and B partitions carry the flag
/// only when no 8x8 is split further (7.3.5), the residual rides the inter
/// scaling list, and the deblocker must leave the internal 4-sample edges of an
/// 8x8 macroblock alone (8.7).
#[test]
fn high_profile_8x8_inter_sequences_match_ffmpeg() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let dir = scratch("high8x8-seq");
    let mut failures = Vec::new();
    let cases: &[(&str, &[&str])] = &[
        (
            "p-cabac",
            &["-profile:v", "high", "-coder", "ac", "-bf", "0"],
        ),
        (
            "p-cavlc",
            &["-profile:v", "high", "-coder", "0", "-bf", "0"],
        ),
        (
            "b-cabac",
            &[
                "-profile:v",
                "high",
                "-coder",
                "ac",
                "-bf",
                "2",
                "-x264-params",
                "b-adapt=0",
            ],
        ),
        (
            "b-cavlc",
            &[
                "-profile:v",
                "high",
                "-coder",
                "0",
                "-bf",
                "2",
                "-x264-params",
                "b-adapt=0",
            ],
        ),
        // Sub-8x8 partitions: every macroblock that splits below 8x8 must not
        // carry the flag, and the neighbouring context has to agree.
        (
            "subpart",
            &[
                "-profile:v",
                "high",
                "-coder",
                "ac",
                "-bf",
                "0",
                "-x264-params",
                "partitions=all:8x8dct=1",
            ],
        ),
    ];
    for (tag, extra) in cases {
        let mut args = extra.to_vec();
        args.extend(["-qp", "26"]);
        match x264_encode_gop(&dir, tag, "176x144", 12, &args) {
            None => failures.push(format!("{tag}: ffmpeg cannot encode")),
            Some(stream) => match compare_sequence(&stream) {
                Ok(n) => {
                    assert!(
                        pps_enables_8x8(&stream),
                        "{tag}: encoder did not enable the 8x8 transform"
                    );
                    eprintln!("8x8 {tag}: {n} frames bit-exact");
                }
                Err(e) => failures.push(format!("{tag}: {e}")),
            },
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{failures:#?}");
}
