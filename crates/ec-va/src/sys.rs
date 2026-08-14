//! Hand-written FFI for the subset of libva this family uses.
//!
//! # Why hand-written
//!
//! No `bindgen`: the family rule is no build-time tooling, and a generated
//! 40k-line binding hides exactly the thing that has to be audited — the ABI
//! assumptions. Everything here is transcribed from the system headers and is
//! short enough to read in one sitting.
//!
//! # ABI assumptions (all four are load-bearing)
//!
//! 1. **libva 1.23.0 headers**, `/usr/include/va/{va.h,va_drm.h,va_drmcommon.h}`,
//!    target `x86_64-unknown-linux-gnu`, System V C ABI.
//! 2. **C `enum` is `int` (4 bytes)** on this ABI. Every VA enum is transcribed
//!    as a plain `i32` type alias plus constants, *never* as a `#[repr(i32)]`
//!    Rust enum: the driver writes profile/entrypoint values into buffers we
//!    own, and a value we have no variant for would be instant UB on a real
//!    Rust enum. Interpretation happens in the safe layer
//!    ([`crate::caps::Profile::from_raw`]), which has an `Unknown(i32)` arm.
//!    This is the header-drift trap that broke cros-libva at libva >= 1.23.
//! 3. **Struct layouts** below are checked by the `const _: () = assert!(...)`
//!    block at the end of this file against numbers printed by
//!    `crates/ec-va/abi-probe.c` compiled against the *system* headers. Rerun
//!    that probe after a libva upgrade; a mismatch means this file drifted.
//! 4. **`VADisplay` is an opaque pointer**; we never dereference it.
//!
//! Points 1-3 are compile-time. They cannot see a libva that changes layout
//! *after* this crate is built, so [`crate::Display::open`] additionally
//! performs a **runtime** `vaInitialize` version check and refuses anything
//! older than 1.23 with a typed error.

// Every item keeps its exact C spelling so that this file can be diffed
// against the headers by eye — the whole audit story of a hand-written FFI.
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
// Fields keep their C names and are documented by the header line cited on
// each item; restating "the fourcc field is the fourcc" 60 times would bury
// the ABI notes that actually matter here.
#![allow(missing_docs)]

use std::ffi::{c_char, c_int, c_uint, c_void};

// ---------------------------------------------------------------------------
// Scalar types (va.h:260-262, 1580-1647, 2042, 4727)
// ---------------------------------------------------------------------------

/// Opaque display handle. Never dereferenced by this crate.
pub type VADisplay = *mut c_void;
/// Return status of every VA entry point. `va.h:262`.
pub type VAStatus = i32;
/// `va.h:1580`.
pub type VAGenericID = c_uint;
pub type VAConfigID = VAGenericID;
pub type VAContextID = VAGenericID;
pub type VASurfaceID = VAGenericID;
pub type VABufferID = VAGenericID;
pub type VAImageID = VAGenericID;

/// C enum `VAProfile` (`va.h:502`), kept as `i32`. See ABI assumption 2.
pub type VAProfile = i32;
/// C enum `VAEntrypoint` (`va.h:552`), kept as `i32`.
pub type VAEntrypoint = i32;
/// C enum `VAConfigAttribType` (`va.h:619`), kept as `i32`.
pub type VAConfigAttribType = i32;
/// C enum `VASurfaceAttribType` (`va.h:1689`), kept as `i32`.
pub type VASurfaceAttribType = i32;
/// C enum `VAGenericValueType` (`va.h:1651`), kept as `i32`.
pub type VAGenericValueType = i32;
/// C enum `VABufferType` (`va.h:2044`), kept as `i32`.
pub type VABufferType = i32;

pub const VA_INVALID_ID: VAGenericID = 0xffff_ffff;
pub const VA_INVALID_SURFACE: VASurfaceID = VA_INVALID_ID;

// Status codes actually branched on; the rest are reported via `vaErrorStr`.
// `va.h:264-309`.
pub const VA_STATUS_SUCCESS: VAStatus = 0x0000_0000;
pub const VA_STATUS_ERROR_INVALID_CONTEXT: VAStatus = 0x0000_0005;
pub const VA_STATUS_ERROR_UNSUPPORTED_PROFILE: VAStatus = 0x0000_000c;
pub const VA_STATUS_ERROR_UNSUPPORTED_ENTRYPOINT: VAStatus = 0x0000_000d;
pub const VA_STATUS_ERROR_UNSUPPORTED_RT_FORMAT: VAStatus = 0x0000_000e;
pub const VA_STATUS_ERROR_MAX_NUM_EXCEEDED: VAStatus = 0x0000_000b;
pub const VA_STATUS_ERROR_UNIMPLEMENTED: VAStatus = 0x0000_0014;
pub const VA_STATUS_ERROR_RESOLUTION_NOT_SUPPORTED: VAStatus = 0x0000_0013;
pub const VA_STATUS_ERROR_OPERATION_FAILED: VAStatus = 0x0000_0001;
pub const VA_STATUS_ERROR_INVALID_PARAMETER: VAStatus = 0x0000_0012;

// Entrypoints (`va.h:552`).
pub const VAEntrypointVLD: VAEntrypoint = 1;
pub const VAEntrypointEncSlice: VAEntrypoint = 6;
pub const VAEntrypointEncPicture: VAEntrypoint = 7;
pub const VAEntrypointEncSliceLP: VAEntrypoint = 8;
pub const VAEntrypointVideoProc: VAEntrypoint = 10;

// Config attribute types (`va.h:619`).
pub const VAConfigAttribRTFormat: VAConfigAttribType = 0;
pub const VAConfigAttribRateControl: VAConfigAttribType = 5;
pub const VAConfigAttribDecSliceMode: VAConfigAttribType = 6;
pub const VAConfigAttribEncPackedHeaders: VAConfigAttribType = 10;

// RT formats (`va.h:1073-1090`).
pub const VA_RT_FORMAT_YUV420: u32 = 0x0000_0001;
pub const VA_RT_FORMAT_YUV422: u32 = 0x0000_0002;
pub const VA_RT_FORMAT_YUV444: u32 = 0x0000_0004;
pub const VA_RT_FORMAT_YUV400: u32 = 0x0000_0010;
pub const VA_RT_FORMAT_YUV420_10: u32 = 0x0000_0100;
pub const VA_RT_FORMAT_YUV422_10: u32 = 0x0000_0200;
pub const VA_RT_FORMAT_YUV444_10: u32 = 0x0000_0400;
pub const VA_RT_FORMAT_YUV420_12: u32 = 0x0000_1000;
pub const VA_RT_FORMAT_RGB32: u32 = 0x0002_0000;

// Surface attribute types (`va.h:1689`) and flags (`va.h:1681-1684`).
pub const VASurfaceAttribNone: VASurfaceAttribType = 0;
pub const VASurfaceAttribPixelFormat: VASurfaceAttribType = 1;
pub const VASurfaceAttribMinWidth: VASurfaceAttribType = 2;
pub const VASurfaceAttribMaxWidth: VASurfaceAttribType = 3;
pub const VASurfaceAttribMinHeight: VASurfaceAttribType = 4;
pub const VASurfaceAttribMaxHeight: VASurfaceAttribType = 5;
pub const VASurfaceAttribMemoryType: VASurfaceAttribType = 6;
pub const VASurfaceAttribUsageHint: VASurfaceAttribType = 8;

pub const VA_SURFACE_ATTRIB_GETTABLE: u32 = 0x0000_0001;
pub const VA_SURFACE_ATTRIB_SETTABLE: u32 = 0x0000_0002;

pub const VA_SURFACE_ATTRIB_USAGE_HINT_GENERIC: u32 = 0x0000_0000;
pub const VA_SURFACE_ATTRIB_USAGE_HINT_DECODER: u32 = 0x0000_0001;
pub const VA_SURFACE_ATTRIB_USAGE_HINT_ENCODER: u32 = 0x0000_0002;
pub const VA_SURFACE_ATTRIB_USAGE_HINT_EXPORT: u32 = 0x0000_0020;

// Generic value discriminants (`va.h:1651`).
pub const VAGenericValueTypeInteger: VAGenericValueType = 1;
pub const VAGenericValueTypeFloat: VAGenericValueType = 2;
pub const VAGenericValueTypePointer: VAGenericValueType = 3;
pub const VAGenericValueTypeFunc: VAGenericValueType = 4;

// Buffer types actually used by decode/encode paths (`va.h:2044`).
pub const VAPictureParameterBufferType: VABufferType = 0;
pub const VAIQMatrixBufferType: VABufferType = 1;
pub const VASliceParameterBufferType: VABufferType = 4;
pub const VASliceDataBufferType: VABufferType = 5;
pub const VAImageBufferType: VABufferType = 9;
pub const VAProbabilityBufferType: VABufferType = 13;
pub const VAEncCodedBufferType: VABufferType = 21;
pub const VAEncSequenceParameterBufferType: VABufferType = 22;
pub const VAEncPictureParameterBufferType: VABufferType = 23;
pub const VAEncSliceParameterBufferType: VABufferType = 24;
pub const VAEncPackedHeaderParameterBufferType: VABufferType = 25;
pub const VAEncPackedHeaderDataBufferType: VABufferType = 26;
pub const VAEncMiscParameterBufferType: VABufferType = 27;

/// `vaCreateContext` flag: sequence contains only progressive frames (`va.h:1919`).
pub const VA_PROGRESSIVE: c_int = 0x1;

// DRM PRIME memory types and export flags (`va_drmcommon.h:78-94`, `va.h:4130-4150`).
pub const VA_SURFACE_ATTRIB_MEM_TYPE_VA: u32 = 0x0000_0001;
pub const VA_SURFACE_ATTRIB_MEM_TYPE_KERNEL_DRM: u32 = 0x1000_0000;
pub const VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME: u32 = 0x2000_0000;
pub const VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2: u32 = 0x4000_0000;

pub const VA_EXPORT_SURFACE_READ_ONLY: u32 = 0x0001;
pub const VA_EXPORT_SURFACE_WRITE_ONLY: u32 = 0x0002;
pub const VA_EXPORT_SURFACE_READ_WRITE: u32 = 0x0003;
pub const VA_EXPORT_SURFACE_SEPARATE_LAYERS: u32 = 0x0004;
pub const VA_EXPORT_SURFACE_COMPOSED_LAYERS: u32 = 0x0008;

// FourCCs used by the family (`va.h:4400-4700`).
pub const VA_FOURCC_NV12: u32 = 0x3231_564e;
pub const VA_FOURCC_P010: u32 = 0x3031_3050;
pub const VA_FOURCC_I420: u32 = 0x3032_3449;
pub const VA_FOURCC_YV12: u32 = 0x3231_5659;

// ---------------------------------------------------------------------------
// Structs
// ---------------------------------------------------------------------------

/// `VAConfigAttrib`, `va.h:1155`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VAConfigAttrib {
    pub type_: VAConfigAttribType,
    pub value: u32,
}

/// The `value` union of `VAGenericValue`, `va.h:1663-1673`.
///
/// Only ever read after checking the sibling `type_` discriminant.
#[repr(C)]
#[derive(Clone, Copy)]
pub union VAGenericValueUnion {
    pub i: i32,
    pub f: f32,
    pub p: *mut c_void,
    pub func: Option<extern "C" fn()>,
}

/// `VAGenericValue`, `va.h:1661`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VAGenericValue {
    pub type_: VAGenericValueType,
    pub value: VAGenericValueUnion,
}

/// `VASurfaceAttrib`, `va.h:1741`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VASurfaceAttrib {
    pub type_: VASurfaceAttribType,
    pub flags: u32,
    pub value: VAGenericValue,
}

/// `VAImageFormat`, `va.h:4712`. `va_reserved` is `VA_PADDING_LOW` = 4 u32s.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VAImageFormat {
    pub fourcc: u32,
    pub byte_order: u32,
    pub bits_per_pixel: u32,
    pub depth: u32,
    pub red_mask: u32,
    pub green_mask: u32,
    pub blue_mask: u32,
    pub alpha_mask: u32,
    pub va_reserved: [u32; 4],
}

/// `VAImage`, `va.h:4729`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VAImage {
    pub image_id: VAImageID,
    pub format: VAImageFormat,
    pub buf: VABufferID,
    pub width: u16,
    pub height: u16,
    pub data_size: u32,
    pub num_planes: u32,
    pub pitches: [u32; 3],
    pub offsets: [u32; 3],
    pub num_palette_entries: i32,
    pub entry_bytes: i32,
    pub component_order: [i8; 4],
    pub va_reserved: [u32; 4],
}

impl Default for VAImage {
    fn default() -> Self {
        // All-zero is a valid "not yet filled" VAImage; `image_id` is
        // overwritten by vaDeriveImage/vaCreateImage before any use.
        // SAFETY: every field is a plain integer or array of integers, so the
        // all-zero bit pattern is a valid value of this type.
        unsafe { std::mem::zeroed() }
    }
}

/// One DRM object of a `VADRMPRIMESurfaceDescriptor`, `va_drmcommon.h:139-147`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VADRMPRIMEObject {
    pub fd: c_int,
    pub size: u32,
    pub drm_format_modifier: u64,
}

/// One layer of a `VADRMPRIMESurfaceDescriptor`, `va_drmcommon.h:151-164`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VADRMPRIMELayer {
    pub drm_format: u32,
    pub num_planes: u32,
    pub object_index: [u32; 4],
    pub offset: [u32; 4],
    pub pitch: [u32; 4],
}

/// `VADRMPRIMESurfaceDescriptor`, `va_drmcommon.h:130`. Used with
/// [`VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2`].
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VADRMPRIMESurfaceDescriptor {
    pub fourcc: u32,
    pub width: u32,
    pub height: u32,
    pub num_objects: u32,
    pub objects: [VADRMPRIMEObject; 4],
    pub num_layers: u32,
    pub layers: [VADRMPRIMELayer; 4],
}

impl Default for VADRMPRIMESurfaceDescriptor {
    fn default() -> Self {
        // SAFETY: all fields are integers or arrays of integer structs; the
        // all-zero bit pattern is valid. `fd` = 0 is never treated as an owned
        // descriptor because `num_objects` = 0 in that state.
        unsafe { std::mem::zeroed() }
    }
}

/// `VARectangle`, `va.h:406`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VARectangle {
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

#[link(name = "va")]
unsafe extern "C" {
    /// `va.h:403`. Returns a pointer to a static string; never NULL.
    pub fn vaErrorStr(error_status: VAStatus) -> *const c_char;
    /// `va.h:453`.
    pub fn vaDisplayIsValid(dpy: VADisplay) -> c_int;
    /// `va.h:465`.
    pub fn vaInitialize(
        dpy: VADisplay,
        major_version: *mut c_int,
        minor_version: *mut c_int,
    ) -> VAStatus;
    /// `va.h:474`.
    pub fn vaTerminate(dpy: VADisplay) -> VAStatus;
    /// `va.h:486`. Static string owned by the driver, valid until `vaTerminate`.
    pub fn vaQueryVendorString(dpy: VADisplay) -> *const c_char;

    /// `va.h:1524`.
    pub fn vaMaxNumProfiles(dpy: VADisplay) -> c_int;
    /// `va.h:1529`.
    pub fn vaMaxNumEntrypoints(dpy: VADisplay) -> c_int;
    /// `va.h:1534`.
    pub fn vaMaxNumConfigAttributes(dpy: VADisplay) -> c_int;
    /// `va.h:1544`.
    pub fn vaQueryConfigProfiles(
        dpy: VADisplay,
        profile_list: *mut VAProfile,
        num_profiles: *mut c_int,
    ) -> VAStatus;
    /// `va.h:1556`.
    pub fn vaQueryConfigEntrypoints(
        dpy: VADisplay,
        profile: VAProfile,
        entrypoint_list: *mut VAEntrypoint,
        num_entrypoints: *mut c_int,
    ) -> VAStatus;
    /// `va.h:1571`. `attrib_list` is in/out: caller sets `type_`, driver fills `value`.
    pub fn vaGetConfigAttributes(
        dpy: VADisplay,
        profile: VAProfile,
        entrypoint: VAEntrypoint,
        attrib_list: *mut VAConfigAttrib,
        num_attribs: c_int,
    ) -> VAStatus;
    /// `va.h:1589`.
    pub fn vaCreateConfig(
        dpy: VADisplay,
        profile: VAProfile,
        entrypoint: VAEntrypoint,
        attrib_list: *mut VAConfigAttrib,
        num_attribs: c_int,
        config_id: *mut VAConfigID,
    ) -> VAStatus;
    /// `va.h:1601`.
    pub fn vaDestroyConfig(dpy: VADisplay, config_id: VAConfigID) -> VAStatus;

    /// `va.h:1868`. Pass NULL `attrib_list` to only query the count.
    pub fn vaQuerySurfaceAttributes(
        dpy: VADisplay,
        config: VAConfigID,
        attrib_list: *mut VASurfaceAttrib,
        num_attribs: *mut c_uint,
    ) -> VAStatus;
    /// `va.h:1894`.
    pub fn vaCreateSurfaces(
        dpy: VADisplay,
        format: c_uint,
        width: c_uint,
        height: c_uint,
        surfaces: *mut VASurfaceID,
        num_surfaces: c_uint,
        attrib_list: *mut VASurfaceAttrib,
        num_attribs: c_uint,
    ) -> VAStatus;
    /// `va.h:1914`.
    pub fn vaDestroySurfaces(
        dpy: VADisplay,
        surfaces: *mut VASurfaceID,
        num_surfaces: c_int,
    ) -> VAStatus;
    /// `va.h:4185`. `descriptor` type is chosen by `mem_type`.
    pub fn vaExportSurfaceHandle(
        dpy: VADisplay,
        surface_id: VASurfaceID,
        mem_type: u32,
        flags: u32,
        descriptor: *mut c_void,
    ) -> VAStatus;

    /// `va.h:1933`.
    pub fn vaCreateContext(
        dpy: VADisplay,
        config_id: VAConfigID,
        picture_width: c_int,
        picture_height: c_int,
        flag: c_int,
        render_targets: *mut VASurfaceID,
        num_render_targets: c_int,
        context: *mut VAContextID,
    ) -> VAStatus;
    /// `va.h:1949`.
    pub fn vaDestroyContext(dpy: VADisplay, context: VAContextID) -> VAStatus;

    /// `va.h:3833`. `data` NULL means "allocate, fill later via vaMapBuffer".
    pub fn vaCreateBuffer(
        dpy: VADisplay,
        context: VAContextID,
        type_: VABufferType,
        size: c_uint,
        num_elements: c_uint,
        data: *mut c_void,
        buf_id: *mut VABufferID,
    ) -> VAStatus;
    /// `va.h:3971`.
    pub fn vaMapBuffer(dpy: VADisplay, buf_id: VABufferID, pbuf: *mut *mut c_void) -> VAStatus;
    /// `va.h:4004`.
    pub fn vaUnmapBuffer(dpy: VADisplay, buf_id: VABufferID) -> VAStatus;
    /// `va.h:4018`.
    pub fn vaDestroyBuffer(dpy: VADisplay, buffer_id: VABufferID) -> VAStatus;

    /// `va.h:4205`.
    pub fn vaBeginPicture(
        dpy: VADisplay,
        context: VAContextID,
        render_target: VASurfaceID,
    ) -> VAStatus;
    /// `va.h:4214`.
    pub fn vaRenderPicture(
        dpy: VADisplay,
        context: VAContextID,
        buffers: *mut VABufferID,
        num_buffers: c_int,
    ) -> VAStatus;
    /// `va.h:4229`.
    pub fn vaEndPicture(dpy: VADisplay, context: VAContextID) -> VAStatus;
    /// `va.h:4271`. Blocks until the render target is idle.
    pub fn vaSyncSurface(dpy: VADisplay, render_target: VASurfaceID) -> VAStatus;

    /// `va.h:4773`.
    pub fn vaMaxNumImageFormats(dpy: VADisplay) -> c_int;
    /// `va.h:4783`.
    pub fn vaQueryImageFormats(
        dpy: VADisplay,
        format_list: *mut VAImageFormat,
        num_formats: *mut c_int,
    ) -> VAStatus;
    /// `va.h:4796`.
    pub fn vaCreateImage(
        dpy: VADisplay,
        format: *mut VAImageFormat,
        width: c_int,
        height: c_int,
        image: *mut VAImage,
    ) -> VAStatus;
    /// `va.h:4807`.
    pub fn vaDestroyImage(dpy: VADisplay, image: VAImageID) -> VAStatus;
    /// `va.h:4827`.
    pub fn vaGetImage(
        dpy: VADisplay,
        surface: VASurfaceID,
        x: c_int,
        y: c_int,
        width: c_uint,
        height: c_uint,
        image: VAImageID,
    ) -> VAStatus;
    /// `va.h:4888`. Zero-copy view of a surface, when the driver allows it.
    pub fn vaDeriveImage(dpy: VADisplay, surface: VASurfaceID, image: *mut VAImage) -> VAStatus;
}

#[link(name = "va-drm")]
unsafe extern "C" {
    /// `va_drm.h:45`. Returns NULL on failure. Does **not** take ownership of `fd`.
    pub fn vaGetDisplayDRM(fd: c_int) -> VADisplay;
}

// ---------------------------------------------------------------------------
// ABI transcription check
// ---------------------------------------------------------------------------
//
// Reference output of `crates/ec-va/abi-probe.c` compiled against
// /usr/include/va (libva 1.23.0) with gcc on x86_64:
//
//   VAConfigAttrib              size=8   align=4   type=0 value=4
//   VAGenericValue              size=16  align=8   type=0 value=8
//   VASurfaceAttrib             size=24  align=8   type=0 flags=4 value=8
//   VAImageFormat               size=48  align=4   fourcc=0 .. va_reserved=32
//   VAImage                     size=120 align=4   image_id=0 format=4 buf=52
//                                                  width=56 height=58 data_size=60
//                                                  num_planes=64 pitches=68 offsets=80
//                                                  num_palette_entries=92 entry_bytes=96
//                                                  component_order=100 va_reserved=104
//   VADRMPRIMESurfaceDescriptor size=312 align=8   fourcc=0 width=4 height=8
//                                                  num_objects=12 objects=16
//                                                  num_layers=80 layers=84
//                                                  object elem size=16, layer elem size=56
//   VARectangle                 size=8   align=2
const _: () = {
    use std::mem::{align_of, offset_of, size_of};

    assert!(size_of::<VAConfigAttrib>() == 8 && align_of::<VAConfigAttrib>() == 4);
    assert!(offset_of!(VAConfigAttrib, value) == 4);

    assert!(size_of::<VAGenericValue>() == 16 && align_of::<VAGenericValue>() == 8);
    assert!(offset_of!(VAGenericValue, value) == 8);

    assert!(size_of::<VASurfaceAttrib>() == 24 && align_of::<VASurfaceAttrib>() == 8);
    assert!(offset_of!(VASurfaceAttrib, flags) == 4);
    assert!(offset_of!(VASurfaceAttrib, value) == 8);

    assert!(size_of::<VAImageFormat>() == 48 && align_of::<VAImageFormat>() == 4);
    assert!(offset_of!(VAImageFormat, va_reserved) == 32);

    assert!(size_of::<VAImage>() == 120 && align_of::<VAImage>() == 4);
    assert!(offset_of!(VAImage, format) == 4);
    assert!(offset_of!(VAImage, buf) == 52);
    assert!(offset_of!(VAImage, width) == 56);
    assert!(offset_of!(VAImage, height) == 58);
    assert!(offset_of!(VAImage, data_size) == 60);
    assert!(offset_of!(VAImage, num_planes) == 64);
    assert!(offset_of!(VAImage, pitches) == 68);
    assert!(offset_of!(VAImage, offsets) == 80);
    assert!(offset_of!(VAImage, num_palette_entries) == 92);
    assert!(offset_of!(VAImage, entry_bytes) == 96);
    assert!(offset_of!(VAImage, component_order) == 100);
    assert!(offset_of!(VAImage, va_reserved) == 104);

    assert!(size_of::<VADRMPRIMEObject>() == 16);
    assert!(size_of::<VADRMPRIMELayer>() == 56);
    assert!(
        size_of::<VADRMPRIMESurfaceDescriptor>() == 312
            && align_of::<VADRMPRIMESurfaceDescriptor>() == 8
    );
    assert!(offset_of!(VADRMPRIMESurfaceDescriptor, objects) == 16);
    assert!(offset_of!(VADRMPRIMESurfaceDescriptor, num_layers) == 80);
    assert!(offset_of!(VADRMPRIMESurfaceDescriptor, layers) == 84);

    assert!(size_of::<VARectangle>() == 8 && align_of::<VARectangle>() == 2);

    // Assumption 2: C `enum` and `unsigned int` are both 4 bytes here.
    assert!(size_of::<VAStatus>() == 4);
    assert!(size_of::<VAGenericID>() == 4);
    assert!(size_of::<c_int>() == 4);
};
