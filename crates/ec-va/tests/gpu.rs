//! Live GPU tests. Not `#[ignore]`d: this machine has the hardware, and a test
//! that never runs is not a test. On a machine with no VA-API device every one
//! of these fails at `Display::open`, which is the honest outcome — the crate
//! has no meaning without a driver.
//!
//! Everything lives in one test binary on purpose (a file per feature is a
//! serial link-and-run tax). Every test opens its own display and takes
//! [`serial`], so the two process-wide measurements below stay meaningful.
//!
//! Reference: `vainfo` on this box (Mesa 26.1.6, radeonsi gfx1200, RX 9060 XT).

use std::os::fd::AsRawFd;
use std::rc::Rc;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use ec_va::caps::{Entrypoint, Profile};
use ec_va::va;
use ec_va::{CapReport, Config, Display, Image, Surface, SurfaceExt, SurfaceSpec, create_pool};

/// Runs the tests in this binary one at a time.
///
/// Two of them measure process-wide state — `VmRSS` for the surface-pool leak
/// check and `/proc/self/fd` for the PRIME export ownership check — and a
/// sibling test allocating a 1080p pool on another thread would make both
/// meaningless. Serialising is cheaper and more honest than loosening the
/// thresholds until concurrency stops mattering.
fn serial() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        // A panicking test poisons the mutex; the rest are still worth running.
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn display() -> Rc<Display> {
    ec_va::open().expect("no VA-API display; this test needs the GPU")
}

/// The driver's `VAImageFormat` for `fourcc`.
fn image_format(display: &Display, fourcc: u32) -> va::VAImageFormat {
    display
        .query_image_formats()
        .expect("vaQueryImageFormats")
        .into_iter()
        .find(|f| f.fourcc == fourcc)
        .expect("driver lacks the image format")
}

fn pool(display: &Rc<Display>, spec: &SurfaceSpec, n: usize) -> Vec<Surface> {
    create_pool(display, spec, n).expect("pool")
}

fn one(display: &Rc<Display>, spec: &SurfaceSpec) -> Surface {
    pool(display, spec, 1).pop().expect("surface")
}

// --------------------------------------------------------------------------
// Display
// --------------------------------------------------------------------------

// The raw `Display::version` / `device_path` / `is_valid` checks no longer
// exist in cros-libva; what remains observable is that the open succeeded and
// the driver answers real queries.
#[test]
fn display_opens_on_a_render_node() {
    let _serial = serial();
    let display = display();
    let vendor = ec_va::display::vendor(&display).expect("vendor string");
    assert!(!vendor.is_empty(), "driver reported an empty vendor string");
    assert!(
        !display
            .query_config_profiles()
            .expect("profiles")
            .is_empty()
    );
}

// --------------------------------------------------------------------------
// Capabilities — the table must match `vainfo` on this GPU
// --------------------------------------------------------------------------

#[test]
fn caps_match_vainfo_on_this_gpu() {
    let _serial = serial();
    let display = display();
    let caps = CapReport::probe(&display).expect("probe");

    // Decode: every codec the family hands to hardware.
    for profile in [
        Profile::H264ConstrainedBaseline,
        Profile::H264Main,
        Profile::H264High,
        Profile::HEVCMain,
        Profile::HEVCMain10,
        Profile::VP9Profile0,
        Profile::VP9Profile2,
        Profile::AV1Profile0,
        Profile::AV1Profile2,
    ] {
        assert!(
            caps.supports(profile, Entrypoint::VLD),
            "{} VLD missing from the probe but present in vainfo",
            profile.name()
        );
    }

    // Encode: H.264 and HEVC are the two the family ships (AV1 encode exists on
    // this GPU too, but stays opt-in — see the family plan).
    for profile in [
        Profile::H264ConstrainedBaseline,
        Profile::H264Main,
        Profile::H264High,
        Profile::HEVCMain,
        Profile::HEVCMain10,
    ] {
        assert!(
            caps.supports(profile, Entrypoint::EncSlice),
            "{} EncSlice missing from the probe but present in vainfo",
            profile.name()
        );
    }

    // Post-processing, used later for colour conversion.
    assert!(caps.supports(Profile::None, Entrypoint::VideoProc));

    // A pair this GPU genuinely lacks must be absent, not silently invented:
    // vainfo lists VP9 profiles 0 and 2 only.
    assert!(!caps.supports(Profile::VP9Profile1, Entrypoint::VLD));
    assert!(!caps.supports(Profile::VP9Profile3, Entrypoint::VLD));
    assert!(!caps.supports(Profile::VP9Profile0, Entrypoint::EncSlice));

    // 10-bit decode must advertise a 10-bit RT format, or `ec-hw` would derive
    // an 8-bit context for Main10 — the incumbent's D2 defect.
    let main10 = caps
        .entry(Profile::HEVCMain10, Entrypoint::VLD)
        .expect("HEVC Main10 VLD");
    assert!(
        main10.supports_rt_format(va::VA_RT_FORMAT_YUV420_10),
        "HEVC Main10 VLD advertised rt_formats 0x{:08x} without a 10-bit format",
        main10.rt_formats
    );
    let p010_supported = main10
        .surfaces
        .as_ref()
        .expect("surface caps")
        .supports_fourcc(va::VA_FOURCC_P010);
    assert!(p010_supported, "Main10 cannot allocate P010 surfaces");

    // 8-bit decode: NV12 everywhere.
    let h264 = caps
        .entry(Profile::H264Main, Entrypoint::VLD)
        .expect("H264 Main VLD");
    assert!(h264.supports_rt_format(va::VA_RT_FORMAT_YUV420));
    assert!(
        h264.surfaces
            .as_ref()
            .expect("surface caps")
            .supports_fourcc(va::VA_FOURCC_NV12)
    );

    // Readback formats: `vaGetImage` must be able to produce NV12 and P010.
    assert!(caps.image_formats.contains(&va::VA_FOURCC_NV12));
    assert!(caps.image_formats.contains(&va::VA_FOURCC_P010));

    assert!(
        caps.profiles_for(Entrypoint::VLD).len() >= 9,
        "expected at least 9 decode profiles, got {:?}",
        caps.profiles_for(Entrypoint::VLD)
    );
}

#[test]
fn caps_report_driver_legal_surface_sizes() {
    let _serial = serial();
    let display = display();
    let caps = CapReport::probe(&display).expect("probe");
    let entry = caps
        .entry(Profile::H264Main, Entrypoint::VLD)
        .expect("H264 Main VLD");
    let surfaces = entry.surfaces.as_ref().expect("surface caps");

    // radeonsi rejects decode surfaces below 64x64. Probing with a hardcoded
    // 16x16 config is what used to panic here, so the minimum is reported
    // rather than assumed.
    assert_eq!(surfaces.min_width, Some(64));
    assert_eq!(surfaces.min_height, Some(64));
    assert!(surfaces.max_width.unwrap() >= 4096);
    assert!(
        !surfaces.allows(16, 16),
        "16x16 must be refused by the caps"
    );
    assert!(surfaces.allows(1920, 1088));

    // DRM PRIME 2 export must be advertised: that is how frames leave the GPU.
    assert!(
        surfaces.memory_types & va::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2 != 0,
        "memory types 0x{:08x} lack DRM_PRIME_2",
        surfaces.memory_types
    );
}

#[test]
fn unsupported_pair_is_refused_by_the_driver() {
    let _serial = serial();
    // A refusal is a claim: prove the capability is genuinely absent rather
    // than assumed absent.
    let display = display();
    let caps = CapReport::probe(&display).expect("probe");
    assert!(!caps.supports(Profile::VP9Profile1, Entrypoint::VLD));

    let err = Config::new(&display, Profile::VP9Profile1, Entrypoint::VLD, &[])
        .err()
        .expect("VP9 Profile1 decode does not exist on this GPU");
    let status = err.status().expect("a VAStatus-carrying error");
    assert!(
        status == va::VA_STATUS_ERROR_UNSUPPORTED_PROFILE as i32
            || status == va::VA_STATUS_ERROR_UNSUPPORTED_ENTRYPOINT as i32,
        "unexpected refusal: {err}"
    );
    assert!(err.to_string().contains("vaCreateConfig"), "{err}");
}

// --------------------------------------------------------------------------
// Surfaces
// --------------------------------------------------------------------------

#[test]
fn surface_pool_create_and_destroy_is_leak_free() {
    let _serial = serial();
    let display = display();
    let spec = SurfaceSpec::nv12(64, 64);

    // Warm up so first-touch allocations are not counted as a leak.
    for _ in 0..4 {
        let p = pool(&display, &spec, 16);
        assert_eq!(p.len(), 16);
        drop(p);
    }

    let before = vm_rss_kb();
    for _ in 0..100 {
        let p = pool(&display, &spec, 16);
        // Every id must be distinct and valid, or the pool is aliasing memory.
        let mut ids: Vec<_> = p.iter().map(|s| s.id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 16, "vaCreateSurfaces returned duplicate ids");
        assert!(ids.iter().all(|&id| id != va::VA_INVALID_SURFACE));
        drop(p);
    }
    let after = vm_rss_kb();

    // 1600 surfaces created and destroyed. A per-surface leak would be obvious
    // at this scale; the incumbent's unmatched map leaked ~3MB per frame.
    let delta_kb = after.saturating_sub(before);
    assert!(
        delta_kb < 50 * 1024,
        "VmRSS grew {delta_kb} kB over 100 create/destroy cycles"
    );
}

#[test]
fn derive_image_maps_and_reads_back() {
    let _serial = serial();
    let display = display();
    let surface = one(&display, &SurfaceSpec::nv12(64, 64));
    let mut image = Image::derive_from(&surface, (64, 64)).expect("vaDeriveImage");

    let va_image = *image.image();
    assert_eq!(va_image.format.fourcc, va::VA_FOURCC_NV12);
    assert_eq!(image.display_resolution(), (64, 64));
    assert!(image.is_derived());
    assert_eq!(va_image.num_planes, 2, "NV12 is luma + interleaved chroma");
    let pitch = va_image.pitches[0] as usize;
    assert!(pitch >= 64, "luma pitch {pitch} is narrower than the image");
    assert!(va_image.pitches[1] > 0, "chroma pitch missing");
    let luma_off = va_image.offsets[0] as usize;
    let chroma_off = va_image.offsets[1] as usize;
    assert!(
        chroma_off >= luma_off + pitch * 64,
        "luma plane must be at least pitch * height"
    );

    {
        let bytes: &mut [u8] = image.as_mut();
        assert!(
            bytes.len() > chroma_off,
            "mapping shorter than chroma plane"
        );
        for (i, byte) in bytes[luma_off..luma_off + pitch * 64]
            .iter_mut()
            .enumerate()
        {
            *byte = (i % 251) as u8;
        }
        let luma = &bytes[luma_off..];
        assert_eq!(luma[0], 0);
        assert_eq!(luma[250], 250);
        assert_eq!(luma[251], 0, "write did not survive within the mapping");
    }
    drop(image);

    // Re-deriving after the drop proves the unmap/destroy actually happened.
    //
    // What is deliberately *not* asserted: that the bytes written above are
    // readable through this second mapping. On radeonsi 26.1 a derived-image
    // mapping is write-oriented staging — the write reaches the surface (see
    // `created_image_reads_a_surface_back`), but a fresh map reads back zeros.
    let again = Image::derive_from(&surface, (64, 64)).expect("second vaDeriveImage");
    assert_eq!(
        again.image().pitches[0] as usize,
        pitch,
        "second mapping has a different geometry"
    );
    assert!(!AsRef::<[u8]>::as_ref(&again).is_empty());
}

#[test]
fn created_image_reads_a_surface_back() {
    let _serial = serial();
    let display = display();
    let surface = one(&display, &SurfaceSpec::nv12(64, 64));
    // Paint through the derived image so there is something known to read back.
    {
        let mut derived = Image::derive_from(&surface, (64, 64)).expect("derive");
        let luma_off = derived.image().offsets[0] as usize;
        let pitch = derived.image().pitches[0] as usize;
        let bytes: &mut [u8] = derived.as_mut();
        for byte in &mut bytes[luma_off..luma_off + pitch * 64] {
            *byte = 0x5a;
        }
    }
    surface.sync().expect("sync");

    let fmt = image_format(&display, va::VA_FOURCC_NV12);
    let image = Image::create_from(&surface, fmt, (64, 64), (64, 64)).expect("vaGetImage");
    let luma_off = image.image().offsets[0] as usize;
    let bytes: &[u8] = image.as_ref();
    assert!(
        bytes[luma_off..luma_off + 64].iter().all(|&b| b == 0x5a),
        "vaGetImage did not return the painted luma"
    );
    drop(image);

    // A region larger than the surface must be refused, not read out of bounds.
    let fmt = image_format(&display, va::VA_FOURCC_NV12);
    let refused = Image::create_from(&surface, fmt, (128, 128), (128, 128));
    assert!(
        refused.is_err(),
        "region larger than the surface must be refused"
    );
}

#[test]
fn prime_export_owns_and_releases_its_descriptors() {
    let _serial = serial();
    let display = display();
    let surface = one(&display, &SurfaceSpec::nv12(64, 64));
    surface
        .sync_timeout(Duration::from_secs(5))
        .expect("sync before export");

    let baseline = open_fd_count();
    let exported = surface.export().expect("vaExportSurfaceHandle");

    assert_eq!(exported.fourcc, va::VA_FOURCC_NV12);
    assert_eq!((exported.width, exported.height), (64, 64));
    assert!(!exported.objects.is_empty(), "no DRM objects exported");
    assert!(!exported.layers.is_empty(), "no layers exported");
    for object in &exported.objects {
        assert!(object.fd.as_raw_fd() >= 0);
        assert!(object.size > 0);
    }
    for layer in &exported.layers {
        assert!(!layer.planes.is_empty());
        for &(object_index, _offset, pitch) in &layer.planes {
            assert!((object_index as usize) < exported.objects.len());
            assert!(pitch > 0);
        }
    }

    let with_export = open_fd_count();
    assert_eq!(
        with_export - baseline,
        exported.objects.len(),
        "exported fd count does not match the descriptor"
    );
    drop(exported);
    assert_eq!(
        open_fd_count(),
        baseline,
        "dropping the export leaked file descriptors"
    );
}

// --------------------------------------------------------------------------
// Context / picture protocol
// --------------------------------------------------------------------------

#[test]
fn picture_typestate_walks_the_va_protocol() {
    let _serial = serial();
    let display = display();
    let config = Config::new(&display, Profile::H264Main, Entrypoint::VLD, &[]).expect("config");
    let targets = pool(&display, &SurfaceSpec::nv12(1920, 1088), 2);
    let context = ec_va::create_context(&display, &config, 1920, 1088, Some(&targets), true)
        .expect("context");
    let targets: Vec<Rc<Surface>> = targets.into_iter().map(Rc::new).collect();

    // New -> Begin. Each transition consumes the picture, so nothing can call
    // `begin` twice or reach `sync` without `end`.
    let picture = ec_va::Picture::new(0, Rc::clone(&context), Rc::clone(&targets[0]));
    let picture = picture.begin::<()>().expect("vaBeginPicture");
    assert_eq!(picture.surface::<()>().id(), targets[0].id());

    // Begin -> Render -> End. Submitting a picture with no parameter buffers
    // is a driver-level error here (mesa creates the decoder lazily from the
    // picture parameters); what matters is that it surfaces as a typed error
    // instead of a panic, an abort or a GPU hang. Real submissions carry
    // parameter/slice buffers and are `ec-hw`'s job.
    let failed = picture.render().and_then(|p| p.end());
    let err = failed
        .err()
        .expect("empty submission is rejected by radeonsi");
    assert_eq!(
        err.va_status(),
        va::VA_STATUS_ERROR_INVALID_CONTEXT as i32,
        "{err}"
    );

    // The display is still usable afterwards: an abandoned picture must not
    // wedge the context.
    let picture = ec_va::Picture::new(0, Rc::clone(&context), Rc::clone(&targets[1]))
        .begin::<()>()
        .expect("second vaBeginPicture after a failed end");
    drop(picture);
    assert!(display.query_vendor_string().is_ok());
}

#[test]
fn buffers_map_unmap_and_destroy() {
    let _serial = serial();
    let display = display();
    let config =
        Config::new(&display, Profile::H264Main, Entrypoint::EncSlice, &[]).expect("config");
    let targets = pool(&display, &SurfaceSpec::nv12(1920, 1088), 1);
    let context = ec_va::create_context(&display, &config, 1920, 1088, Some(&targets), true)
        .expect("context");

    // Coded-output buffer: allocated empty, mapped, unmapped, dropped.
    const CODED_SIZE: usize = 1 << 20;
    let coded = context
        .create_enc_coded(CODED_SIZE)
        .expect("vaCreateBuffer");
    assert_ne!(coded.id(), va::VA_INVALID_ID);
    {
        let mapped = va::MappedCodedBuffer::new(&coded).expect("vaMapBuffer");
        // Never filled by an encode: no segment carries payload bytes.
        assert!(mapped.iter().all(|s| s.buf.is_empty()));
    }
    // Mapping again proves the guard released the mapping.
    drop(va::MappedCodedBuffer::new(&coded).expect("second vaMapBuffer after unmap"));

    // Parameter buffer: data is copied into driver memory during the call.
    let buffer = ec_va::picture::buffer(
        &context,
        va::BufferType::EncMiscParameter(va::EncMiscParameter::FrameRate(
            va::EncMiscParameterFrameRate::new(30, 0),
        )),
    )
    .expect("vaCreateBuffer from a typed parameter");
    drop(buffer);
    drop(coded);
}

#[test]
fn context_keeps_its_dependencies_alive() {
    let _serial = serial();
    // Dropping the config and the surfaces while the context lives must be
    // safe: the context holds `Rc`s, so libva's destruction order (buffers ->
    // context -> surfaces -> config -> display) cannot be violated.
    let display = display();
    let config = Config::new(&display, Profile::H264Main, Entrypoint::VLD, &[]).expect("config");
    let targets = pool(&display, &SurfaceSpec::nv12(1920, 1088), 2);
    let context = ec_va::create_context(&display, &config, 1920, 1088, Some(&targets), true)
        .expect("context");
    let target = Rc::new(targets.into_iter().next().expect("target"));
    drop(config);

    let picture = ec_va::Picture::new(0, Rc::clone(&context), target)
        .begin::<()>()
        .expect("begin");
    drop(picture);
    drop(context);
    assert!(display.query_vendor_string().is_ok());
}

// --------------------------------------------------------------------------
// VPP
// --------------------------------------------------------------------------

// `vaQueryVideoProcFilters` is no longer wrapped (cros-libva has no such
// call), so the filter list cannot be printed. The equivalent: a pipeline
// still opens on the real display and runs a whole-surface NV12 pass.
#[test]
fn vpp_filters_probe() {
    let _serial = serial();
    let display = display();
    let targets = pool(&display, &SurfaceSpec::nv12(1920, 1080), 2);
    let vpp = ec_va::Vpp::new(&display, va::VA_RT_FORMAT_YUV420, 1920, 1080).expect("vpp opens");
    let mut it = targets.into_iter();
    let src = it.next().expect("src");
    let dst = Rc::new(it.next().expect("dst"));
    vpp.convert(&src, dst).expect("vpp pipeline runs");
}

/// `Vpp::convert` end to end: a P010 source surface into an NV12 destination,
/// no readback — the conversion `ec-hw`'s `encode_frame` relies on for a
/// 10-bit source.
#[test]
fn vpp_converts_p010_to_nv12() {
    let _serial = serial();
    let display = display();
    let source = one(&display, &SurfaceSpec::p010(64, 64));
    let dest = Rc::new(one(&display, &SurfaceSpec::nv12(64, 64)));
    let vpp = ec_va::Vpp::new(
        &display,
        va::VA_RT_FORMAT_YUV420 | va::VA_RT_FORMAT_YUV420_10,
        64,
        64,
    )
    .expect("vpp opens");
    let out = vpp.convert(&source, Rc::clone(&dest)).expect("convert");
    assert_eq!(out.id(), dest.id());
}

// --------------------------------------------------------------------------
// helpers
// --------------------------------------------------------------------------

fn vm_rss_kb() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("/proc/self/status");
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest
                .split_whitespace()
                .next()
                .and_then(|v| v.parse().ok())
                .expect("VmRSS value");
        }
    }
    panic!("VmRSS not found in /proc/self/status");
}

fn open_fd_count() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .expect("/proc/self/fd")
        .count()
}

/// Export a painted NV12 surface as DMA-BUFs, import the fds as a new surface,
/// and read both back: the two must be byte-identical plane for plane. The
/// import also has to leave no fds behind once it is dropped.
#[test]
fn dma_buf_import_reads_back_identical_pixels() {
    let _serial = serial();
    let display = display();
    let (width, height) = (256u32, 192u32);
    let surface = one(&display, &SurfaceSpec::nv12(width, height));
    let fmt = image_format(&display, va::VA_FOURCC_NV12);

    // A pattern that varies by row and column, so a shifted or swapped plane
    // cannot read back equal by accident. The image is written back to the
    // surface (vaPutImage) when it drops.
    {
        let mut source =
            Image::create_from(&surface, fmt, (width, height), (width, height)).expect("image");
        let va_image = *source.image();
        let bytes: &mut [u8] = source.as_mut();
        for plane in 0..2usize {
            let pitch = va_image.pitches[plane] as usize;
            let off = va_image.offsets[plane] as usize;
            let rows = if plane == 0 { height } else { height / 2 } as usize;
            for row in 0..rows {
                for col in 0..pitch {
                    bytes[off + row * pitch + col] = (row * 7 + col * 3 + plane * 101) as u8;
                }
            }
        }
    }
    surface.sync().expect("sync");

    let baseline = open_fd_count();
    let exported = surface.export().expect("export");
    let imported =
        ec_va::surface::import_prime(&display, &exported).expect("import the exported fds");
    imported.sync().expect("sync the import");

    let from_original =
        Image::create_from(&surface, fmt, (width, height), (width, height)).expect("read original");
    let from_import = Image::create_from(&imported, fmt, (width, height), (width, height))
        .expect("read imported");

    let (io, ic) = (*from_original.image(), *from_import.image());
    let (original, copy): (&[u8], &[u8]) = (from_original.as_ref(), from_import.as_ref());
    for plane in 0..2usize {
        let rows = if plane == 0 { height } else { height / 2 } as usize;
        let (po, pc) = (io.pitches[plane] as usize, ic.pitches[plane] as usize);
        let (oo, oc) = (io.offsets[plane] as usize, ic.offsets[plane] as usize);
        assert_eq!(po, pc, "plane {plane}: pitches differ");
        let want = &original[oo..oo + po * rows];
        let got = &copy[oc..oc + pc * rows];
        assert!(
            want.iter().any(|&b| b != 0),
            "plane {plane}: the source read back empty, so the comparison proves nothing"
        );
        assert_eq!(
            want, got,
            "plane {plane}: the import reads back different pixels"
        );
    }
    drop(from_original);
    drop(from_import);

    // The export owns its fds: they close with the descriptor, not before.
    assert!(
        open_fd_count() >= baseline,
        "the import closed its fds early"
    );
    drop(imported);
    drop(exported);
    assert_eq!(
        open_fd_count(),
        baseline,
        "dropping the import and the export leaked file descriptors"
    );
}
