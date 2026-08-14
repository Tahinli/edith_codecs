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
        match dec.push_nal(nal)? {
            NalOutcome::PictureBoundary => {
                dec.end_picture()?;
                dec.push_nal(nal)?;
            }
            _ => {}
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
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error"]);
    if std::env::var_os("EC_H264_NO_DEBLOCK").is_some() {
        cmd.args(["-skip_loop_filter", "all"]);
    }
    let out = cmd
        .arg("-i")
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
    let theirs =
        ffmpeg_all_frames(stream, first.len()).ok_or("ffmpeg cannot decode the stream")?;
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
    // One stream per entropy coder: the CABAC reader carries its 402 context
    // variables inline for exactly this reason.
    let mut dec = Decoder::new();
    for name in ["BA1_Sony_D", "CABA1_SVA_B"] {
        let stream = find_stream(&base.join(name)).expect("fixture");
        let bytes = std::fs::read(stream).unwrap();
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
        assert!(decoded, "no slice decoded in steady-state pass of {name}");
        let n = ALLOCS.load(Ordering::SeqCst);
        assert_eq!(n, 0, "{name}: steady-state decode loop allocated {n} times");
    }
    let bytes = std::fs::read(find_stream(&base.join("BA1_Sony_D")).unwrap()).unwrap();
    let _ = dec.decode_first_idr(&bytes).expect("decode");

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
    // One stream per entropy coder: CABAC's arithmetic decoder indexes context
    // and coefficient arrays from stream-derived values, so it needs the same
    // no-panic floor as the CAVLC tables.
    for name in ["BA1_Sony_D", "CABA1_SVA_B"] {
        let stream = find_stream(&base.join(name)).expect("fixture");
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
            "transform8x8",
            &[
                "-profile:v",
                "high",
                "-qp",
                "26",
                "-x264-params",
                "cabac=0:8x8dct=1",
            ],
            "8x8 transform",
        ),
        // The 8x8 transform is refused under CABAC too, where the flag is a
        // context-coded bin rather than a raw one.
        (
            "transform8x8-cabac",
            &[
                "-profile:v",
                "high",
                "-qp",
                "26",
                "-coder",
                "ac",
                "-x264-params",
                "8x8dct=1",
            ],
            "8x8 transform",
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

/// Inter slices are refused by name under CABAC as well as CAVLC: the
/// arithmetic decoder landing does not imply P and B macroblock parsing, and a
/// stream whose first picture decodes must not silently produce wrong pixels
/// for its second.
#[test]
fn inter_slices_under_cabac_are_refused_by_name() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let dir = scratch("inter-cabac");
    let stream = dir.join("gop.264");
    let ok = run(
        "ffmpeg",
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=176x144:rate=25:duration=1",
            "-pix_fmt",
            "yuv420p",
            "-c:v",
            "libx264",
            "-frames:v",
            "6",
            "-profile:v",
            "main",
            "-coder",
            "ac",
            "-qp",
            "26",
            "-f",
            "h264",
            &stream.to_string_lossy(),
        ],
    );
    assert!(ok, "ffmpeg cannot encode a CABAC GOP");
    let bytes = std::fs::read(&stream).unwrap();

    // The first (IDR) picture decodes; the first P slice must refuse by name.
    let mut dec = Decoder::new();
    let first = dec.decode_first_idr(&bytes).expect("CABAC IDR decodes");
    assert_eq!((first.width, first.height), (176, 144));

    let mut refusal = None;
    let mut pictures = 1;
    for nal in ec_h264_syntax::AnnexBIter::new(&bytes) {
        match dec.push_nal(nal) {
            Ok(ec_h264::NalOutcome::PictureBoundary) => {
                dec.end_picture().expect("end_picture");
                pictures += 1;
                if let Err(e) = dec.push_nal(nal) {
                    refusal = Some(e);
                    break;
                }
            }
            Ok(_) => {}
            Err(e) => {
                refusal = Some(e);
                break;
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    match refusal {
        Some(Error::Unsupported { what, why }) => {
            assert!(
                what.contains("non-I slice") || why.contains("P/B"),
                "refused as {what} ({why}), expected the inter-slice refusal"
            );
            eprintln!("CABAC GOP: {pictures} intra picture(s) decoded, then refused: {what}");
        }
        other => panic!("expected an inter-slice refusal, got {other:?}"),
    }
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

/// Scratch driver kept out of the default run: decode a stream named by
/// `EC_H264_DEBUG_STREAM` frame by frame and report the first mismatch.
#[test]
#[ignore]
fn debug_named_stream() {
    let Ok(path) = std::env::var("EC_H264_DEBUG_STREAM") else {
        return;
    };
    let stream = PathBuf::from(path);
    let bytes = std::fs::read(&stream).unwrap();
    let mut dec = Decoder::new();
    let mut pictures = 0usize;
    for (i, nal) in ec_h264_syntax::AnnexBIter::new(&bytes).enumerate() {
        let kind = nal[0] & 0x1F;
        match dec.push_nal(nal) {
            Ok(NalOutcome::PictureBoundary) => {
                dec.end_picture().unwrap();
                pictures += 1;
                if let Err(e) = dec.push_nal(nal) {
                    panic!("nal {i} (type {kind}) after {pictures} pictures: {e}");
                }
            }
            Ok(_) => {}
            Err(e) => panic!("nal {i} (type {kind}) after {pictures} pictures: {e}"),
        }
        while dec.next_frame().is_some() {}
    }
    dec.flush().unwrap();
    eprintln!("decoded {pictures} pictures");
    let ours = decode_all(&bytes).unwrap();
    let theirs = match std::env::var("EC_H264_DEBUG_REF") {
        Ok(p) => {
            let raw = std::fs::read(p).unwrap();
            raw.chunks_exact(ours[0].len()).map(<[u8]>::to_vec).collect()
        }
        Err(_) => ffmpeg_all_frames(&stream, ours[0].len()).unwrap(),
    };
    if std::env::var_os("EC_H264_DUMP").is_some() {
        let w = 176usize;
        for (label, f) in [("ref0", &ours[0]), ("ours1", &ours[1]), ("ff1", &theirs[1])] {
            eprintln!("--- {label} MB(0,0) rows 0..4 ---");
            for y in 0..4 {
                let row: Vec<u8> = (0..16).map(|x| f[y * w + x]).collect();
                eprintln!("{row:?}");
            }
        }
    }
    for (i, a) in ours.iter().enumerate().take(4) {
        let best = theirs
            .iter()
            .enumerate()
            .map(|(j, b)| (a.iter().zip(b).filter(|(x, y)| x != y).count(), j))
            .min();
        eprintln!("our frame {i} best matches reference {best:?}");
    }
    let w = 176usize;
    let h = 144usize;
    for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
        let diff = a.iter().zip(b).filter(|(x, y)| x != y).count();
        if diff == 0 {
            eprintln!("frame {i}: exact");
            continue;
        }
        let mut mbs = std::collections::BTreeSet::new();
        for (k, (x, y)) in a.iter().zip(b).enumerate().take(w * h) {
            if x != y {
                mbs.insert(((k / w) / 16 * (w / 16) + (k % w) / 16, ((k % w) / 16, (k / w) / 16)));
            }
        }
        eprintln!("  first differing MBs: {:?}", mbs.iter().take(6).map(|m| m.1).collect::<Vec<_>>());
        let max = a
            .iter()
            .zip(b)
            .map(|(x, y)| (i32::from(*x) - i32::from(*y)).abs())
            .max()
            .unwrap_or(0);
        eprintln!(
            "frame {i}: {diff} bytes differ, max delta {max}, {} luma MBs",
            mbs.len()
        );
    }
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
                    table.push_str(&format!("{name:<16} {:>3} frames, no reference YUV\n", frames.len()));
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
                        table.push_str(&format!("{name:<16} {:>3}/{want:<3} frames bit-exact\n", frames.len()));
                    }
                    None => {
                        failed.push(format!(
                            "{name}: {} frames decoded, reference has {want}",
                            frames.len()
                        ));
                        table.push_str(&format!("{name:<16} {:>3}/{want:<3} FRAME COUNT\n", frames.len()));
                    }
                    Some((i, pos)) => {
                        failed.push(format!("{name}: frame {i} differs at byte {pos}"));
                        table.push_str(&format!("{name:<16} {:>3}/{want:<3} MISMATCH frame {i}\n", frames.len()));
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
