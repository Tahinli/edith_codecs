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
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use ec_va::caps::{Entrypoint, Profile};
use ec_va::{
    CapReport, Config, Context, Display, Error, Image, Picture, Surface, SurfaceSpec, sys,
};

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

fn display() -> Arc<Display> {
    Display::open().expect("no VA-API display; this test needs the GPU")
}

// --------------------------------------------------------------------------
// Display
// --------------------------------------------------------------------------

#[test]
fn display_opens_on_a_render_node() {
    let _serial = serial();
    let display = display();
    assert!(display.is_valid());
    let (major, minor) = display.version();
    assert_eq!(major, ec_va::MIN_VA_MAJOR);
    assert!(
        minor >= ec_va::MIN_VA_MINOR,
        "runtime libva {major}.{minor} is older than the transcribed ABI"
    );
    assert!(
        display
            .device_path()
            .to_string_lossy()
            .starts_with("/dev/dri/renderD"),
        "expected a render node, got {:?}",
        display.device_path()
    );
    let vendor = display.vendor().expect("vendor string");
    assert!(!vendor.is_empty(), "driver reported an empty vendor string");
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
        main10.supports_rt_format(sys::VA_RT_FORMAT_YUV420_10),
        "HEVC Main10 VLD advertised rt_formats 0x{:08x} without a 10-bit format",
        main10.rt_formats
    );
    let p010_supported = main10
        .surfaces
        .as_ref()
        .expect("surface caps")
        .supports_fourcc(sys::VA_FOURCC_P010);
    assert!(p010_supported, "Main10 cannot allocate P010 surfaces");

    // 8-bit decode: NV12 everywhere.
    let h264 = caps
        .entry(Profile::H264Main, Entrypoint::VLD)
        .expect("H264 Main VLD");
    assert!(h264.supports_rt_format(sys::VA_RT_FORMAT_YUV420));
    assert!(
        h264.surfaces
            .as_ref()
            .expect("surface caps")
            .supports_fourcc(sys::VA_FOURCC_NV12)
    );

    // Readback formats: `vaGetImage` must be able to produce NV12 and P010.
    assert!(caps.image_formats.contains(&sys::VA_FOURCC_NV12));
    assert!(caps.image_formats.contains(&sys::VA_FOURCC_P010));

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
        surfaces.memory_types & sys::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2 != 0,
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
        .expect_err("VP9 Profile1 decode does not exist on this GPU");
    let status = err.status().expect("a VAStatus-carrying error");
    assert!(
        status == sys::VA_STATUS_ERROR_UNSUPPORTED_PROFILE
            || status == sys::VA_STATUS_ERROR_UNSUPPORTED_ENTRYPOINT,
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
        let pool = Surface::create_pool(&display, &spec, 16).expect("pool");
        assert_eq!(pool.len(), 16);
        drop(pool);
    }

    let before = vm_rss_kb();
    for _ in 0..100 {
        let pool = Surface::create_pool(&display, &spec, 16).expect("pool");
        // Every id must be distinct and valid, or the pool is aliasing memory.
        let mut ids: Vec<_> = pool.iter().map(|s| s.id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 16, "vaCreateSurfaces returned duplicate ids");
        assert!(ids.iter().all(|&id| id != sys::VA_INVALID_SURFACE));
        drop(pool);
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
    let surface = Surface::create(&display, &SurfaceSpec::nv12(64, 64)).expect("surface");
    let mut image = surface.derive_image().expect("vaDeriveImage");

    assert_eq!(image.fourcc(), sys::VA_FOURCC_NV12);
    assert_eq!(image.size(), (64, 64));
    assert_eq!(image.num_planes(), 2, "NV12 is luma + interleaved chroma");
    let pitch = image.pitch(0).expect("luma pitch");
    assert!(pitch >= 64, "luma pitch {pitch} is narrower than the image");

    {
        let mut mapped = image.map().expect("vaMapBuffer");
        let luma = mapped.plane_mut(0).expect("luma plane");
        assert_eq!(
            luma.len(),
            pitch as usize * 64,
            "luma plane length must be pitch * height"
        );
        for (i, byte) in luma.iter_mut().enumerate() {
            *byte = (i % 251) as u8;
        }
        let chroma = mapped.plane(1).expect("chroma plane");
        assert!(!chroma.is_empty());
        assert!(mapped.plane(2).is_none(), "NV12 has no third plane");

        let luma = mapped.plane(0).expect("luma plane");
        assert_eq!(luma[0], 0);
        assert_eq!(luma[250], 250);
        assert_eq!(luma[251], 0, "write did not survive within the mapping");
    }

    // Re-mapping after the guard dropped proves the unmap actually happened:
    // libva returns the same buffer only once per map, and a never-unmapped
    // mapping is the incumbent's per-frame leak.
    //
    // What is deliberately *not* asserted: that the bytes written above are
    // readable through this second mapping. On radeonsi 26.1 a derived-image
    // mapping is write-oriented staging — the write reaches the surface (see
    // `created_image_reads_a_surface_back`), but a fresh map reads back zeros.
    // Readback therefore goes through `vaGetImage`, never through a re-derive.
    let mapped = image.map().expect("second vaMapBuffer after unmap");
    assert_eq!(
        mapped.plane(0).expect("luma").len(),
        pitch as usize * 64,
        "second mapping has a different geometry"
    );
}

#[test]
fn created_image_reads_a_surface_back() {
    let _serial = serial();
    let display = display();
    let surface = Surface::create(&display, &SurfaceSpec::nv12(64, 64)).expect("surface");
    // Paint through the derived image so there is something known to read back.
    {
        let mut derived = surface.derive_image().expect("derive");
        let mut mapped = derived.map().expect("map");
        for byte in mapped.plane_mut(0).expect("luma") {
            *byte = 0x5a;
        }
    }
    surface.sync().expect("sync");

    let mut image = Image::create(&display, sys::VA_FOURCC_NV12, 64, 64).expect("vaCreateImage");
    surface
        .read_into(&mut image, 0, 0, 64, 64)
        .expect("vaGetImage");
    let mapped = image.map().expect("map");
    let luma = mapped.plane(0).expect("luma");
    assert!(
        luma.iter().take(64).all(|&b| b == 0x5a),
        "vaGetImage did not return the painted luma"
    );

    // Out-of-range regions are refused before reaching the driver.
    drop(mapped);
    let err = surface
        .read_into(&mut image, 0, 0, 128, 128)
        .expect_err("region larger than the image must be refused");
    assert!(matches!(err, Error::Protocol(_)), "{err}");
}

#[test]
fn prime_export_owns_and_releases_its_descriptors() {
    let _serial = serial();
    let display = display();
    let surface = Surface::create(&display, &SurfaceSpec::nv12(64, 64)).expect("surface");
    surface.sync().expect("sync before export");

    let baseline = open_fd_count();
    let exported = surface
        .export_prime(sys::VA_EXPORT_SURFACE_READ_ONLY)
        .expect("vaExportSurfaceHandle");

    assert_eq!(exported.fourcc, sys::VA_FOURCC_NV12);
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
    let targets = Surface::create_pool(&display, &SurfaceSpec::nv12(1920, 1088), 2).expect("pool");
    let context =
        Context::new(&config, 1920, 1088, sys::VA_PROGRESSIVE, &targets).expect("context");

    // New -> Rendering. Each transition consumes the picture, so nothing can
    // call `begin` twice or reach `sync` without `end` (see the compile_fail
    // examples in the `picture` module docs).
    let picture = Picture::new(&context, Arc::clone(&targets[0]));
    let picture = picture.begin().expect("vaBeginPicture");
    assert_eq!(picture.target().id(), targets[0].id());

    // Rendering -> Ended. Submitting a picture with no parameter buffers is a
    // driver-level error here (mesa creates the decoder lazily from the picture
    // parameters, so it has nothing to end); what matters is that it surfaces
    // as a typed error instead of a panic, an abort or a GPU hang. Real
    // submissions carry parameter/slice buffers and are `ec-hw`'s job.
    let err = picture
        .end()
        .expect_err("empty submission is rejected by radeonsi");
    assert_eq!(
        err.status(),
        Some(sys::VA_STATUS_ERROR_INVALID_CONTEXT),
        "{err}"
    );
    assert!(err.to_string().contains("vaEndPicture"), "{err}");

    // The display is still usable afterwards: an abandoned picture must not
    // wedge the context.
    let picture = Picture::new(&context, Arc::clone(&targets[1]))
        .begin()
        .expect("second vaBeginPicture after a failed end");
    drop(picture);
    assert!(display.is_valid());
}

#[test]
fn buffers_map_unmap_and_destroy() {
    let _serial = serial();
    let display = display();
    let config =
        Config::new(&display, Profile::H264Main, Entrypoint::EncSlice, &[]).expect("config");
    let targets = Surface::create_pool(&display, &SurfaceSpec::nv12(1920, 1088), 1).expect("pool");
    let context =
        Context::new(&config, 1920, 1088, sys::VA_PROGRESSIVE, &targets).expect("context");

    // Coded-output buffer: allocated empty, mapped, written, unmapped, dropped.
    const CODED_SIZE: u32 = 1 << 20;
    let mut coded = ec_va::Buffer::allocate(&context, sys::VAEncCodedBufferType, CODED_SIZE)
        .expect("vaCreateBuffer");
    assert_eq!(coded.buffer_type(), sys::VAEncCodedBufferType);
    assert_ne!(coded.id(), sys::VA_INVALID_ID);
    {
        let mut mapped = coded.map().expect("vaMapBuffer");
        // SAFETY: 64 bytes of a 1 MiB allocation.
        let slice = unsafe { mapped.as_mut_slice(64) };
        slice[0] = 0x42;
        // SAFETY: same 64-byte window, now read-only.
        assert_eq!(unsafe { mapped.as_slice(64) }[0], 0x42);
    }
    // Mapping again proves the guard released the mapping.
    drop(coded.map().expect("second vaMapBuffer after unmap"));

    // Parameter buffer: data is copied into driver memory during the call.
    let params = [0u8; 32];
    let buffer = ec_va::Buffer::from_bytes(&context, sys::VAEncMiscParameterBufferType, &params)
        .expect("vaCreateBuffer from bytes");
    assert_ne!(buffer.id(), sys::VA_INVALID_ID);

    // An empty buffer is refused before it reaches the driver.
    let err = ec_va::Buffer::from_bytes(&context, sys::VAEncMiscParameterBufferType, &[])
        .expect_err("empty buffer must be refused");
    assert!(matches!(err, Error::Protocol(_)), "{err}");
}

#[test]
fn context_keeps_its_dependencies_alive() {
    let _serial = serial();
    // Dropping the config and the surfaces while the context lives must be
    // safe: the context holds `Arc`s, so libva's destruction order (buffers ->
    // context -> surfaces -> config -> display) cannot be violated.
    let display = display();
    let config = Config::new(&display, Profile::H264Main, Entrypoint::VLD, &[]).expect("config");
    let targets = Surface::create_pool(&display, &SurfaceSpec::nv12(1920, 1088), 2).expect("pool");
    let context =
        Context::new(&config, 1920, 1088, sys::VA_PROGRESSIVE, &targets).expect("context");
    let target = Arc::clone(&targets[0]);
    drop(config);
    drop(targets);

    let picture = Picture::new(&context, target).begin().expect("begin");
    drop(picture);
    drop(context);
    assert!(display.is_valid());
}

// --------------------------------------------------------------------------
// VPP
// --------------------------------------------------------------------------

/// `vaQueryVideoProcFilters` on the real display: whatever this driver
/// advertises, the call must succeed and return values `Vpp` can act on. Not
/// a claim about which filters exist — radeonsi's list is print-only here —
/// just that the query path works end to end.
#[test]
fn vpp_filters_probe() {
    let _serial = serial();
    let display = display();
    let targets = Surface::create_pool(&display, &SurfaceSpec::nv12(1920, 1080), 1).expect("pool");
    let vpp = ec_va::Vpp::new(&display, sys::VA_RT_FORMAT_YUV420, &targets).expect("vpp opens");
    let filters = vpp.filters().expect("vaQueryVideoProcFilters");
    println!("radeonsi VAProcFilterType list: {filters:?}");
    println!(
        "HighDynamicRangeToneMapping (8) advertised: {}",
        filters.contains(&sys::VAProcFilterHighDynamicRangeToneMapping)
    );
}

/// `Vpp::convert` end to end: a P010 source surface into an NV12 destination,
/// no readback — the conversion `ec-hw`'s `encode_frame` relies on for a
/// 10-bit source.
#[test]
fn vpp_converts_p010_to_nv12() {
    let _serial = serial();
    let display = display();
    let src_spec = SurfaceSpec::p010(64, 64);
    let source = Surface::create(&display, &src_spec).expect("p010 source surface");
    let dest_targets = Surface::create_pool(&display, &SurfaceSpec::nv12(64, 64), 1).expect("pool");
    let vpp = ec_va::Vpp::new(
        &display,
        sys::VA_RT_FORMAT_YUV420 | sys::VA_RT_FORMAT_YUV420_10,
        &dest_targets,
    )
    .expect("vpp opens");
    let dest = vpp
        .convert(&source, Arc::clone(&dest_targets[0]))
        .expect("convert");
    assert_eq!(dest.id(), dest_targets[0].id());
    assert!(display.is_valid());
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
