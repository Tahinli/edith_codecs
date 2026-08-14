//! [`Surface`] — GPU-side picture storage, plus the two ways of getting at its
//! contents: DRM PRIME export (zero-copy handoff) and image mapping (readback).

use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::Arc;

use crate::display::Display;
use crate::error::{Error, Result, check};
use crate::sys;

/// What kind of surface to allocate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceSpec {
    /// `VA_RT_FORMAT_*` for the config this surface will be used with.
    pub rt_format: u32,
    /// Exact pixel layout (FourCC). `None` lets the driver choose.
    pub fourcc: Option<u32>,
    /// Width in pixels. Must satisfy [`crate::SurfaceCaps::allows`].
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `VA_SURFACE_ATTRIB_USAGE_HINT_*` bitmask; drivers use it to pick tiling.
    pub usage_hint: u32,
}

impl SurfaceSpec {
    /// 8-bit 4:2:0 NV12, the decode output format of every codec in the family.
    pub fn nv12(width: u32, height: u32) -> Self {
        SurfaceSpec {
            rt_format: sys::VA_RT_FORMAT_YUV420,
            fourcc: Some(sys::VA_FOURCC_NV12),
            width,
            height,
            usage_hint: sys::VA_SURFACE_ATTRIB_USAGE_HINT_GENERIC,
        }
    }

    /// 10-bit 4:2:0 P010, for HEVC Main10 / VP9 Profile 2 / AV1 10-bit.
    ///
    /// The RT format is derived from the stream's bit depth, never hardcoded to
    /// 8-bit: an 8-bit context on a 10-bit stream is the incumbent defect this
    /// constructor exists to make impossible to write by accident.
    pub fn p010(width: u32, height: u32) -> Self {
        SurfaceSpec {
            rt_format: sys::VA_RT_FORMAT_YUV420_10,
            fourcc: Some(sys::VA_FOURCC_P010),
            width,
            height,
            usage_hint: sys::VA_SURFACE_ATTRIB_USAGE_HINT_GENERIC,
        }
    }

    /// Set the usage hint (`VA_SURFACE_ATTRIB_USAGE_HINT_*`).
    pub fn with_usage_hint(mut self, hint: u32) -> Self {
        self.usage_hint = hint;
        self
    }
}

/// A live `VASurfaceID`.
///
/// Surfaces may only be destroyed after every context using them is gone;
/// [`crate::Context`] therefore holds `Arc<Surface>` for its render targets and
/// this `Drop` cannot run too early.
pub struct Surface {
    display: Arc<Display>,
    id: sys::VASurfaceID,
    spec: SurfaceSpec,
}

impl Surface {
    /// Allocate a single surface.
    pub fn create(display: &Arc<Display>, spec: &SurfaceSpec) -> Result<Arc<Surface>> {
        let mut pool = Surface::create_pool(display, spec, 1)?;
        Ok(pool.pop().expect("create_pool(1) returns one surface"))
    }

    /// Allocate `count` surfaces in one driver call.
    ///
    /// Each returned surface owns its own id and destroys exactly that id, so a
    /// pool can be torn down incrementally as frames are released.
    pub fn create_pool(
        display: &Arc<Display>,
        spec: &SurfaceSpec,
        count: usize,
    ) -> Result<Vec<Arc<Surface>>> {
        if count == 0 {
            return Ok(Vec::new());
        }
        let mut attribs: Vec<sys::VASurfaceAttrib> = Vec::with_capacity(2);
        if let Some(fourcc) = spec.fourcc {
            attribs.push(int_attrib(sys::VASurfaceAttribPixelFormat, fourcc as i32));
        }
        if spec.usage_hint != sys::VA_SURFACE_ATTRIB_USAGE_HINT_GENERIC {
            attribs.push(int_attrib(
                sys::VASurfaceAttribUsageHint,
                spec.usage_hint as i32,
            ));
        }

        let mut ids = vec![sys::VA_INVALID_SURFACE; count];
        // SAFETY: `ids` has room for exactly `count` surface ids and `attribs`
        // for exactly `attribs.len()` attributes; both counts are passed
        // alongside. libva writes only within those bounds and does not retain
        // either pointer past the call.
        let status = unsafe {
            sys::vaCreateSurfaces(
                display.handle(),
                spec.rt_format,
                spec.width,
                spec.height,
                ids.as_mut_ptr(),
                count as u32,
                attribs.as_mut_ptr(),
                attribs.len() as u32,
            )
        };
        check("vaCreateSurfaces", status)?;

        if ids.contains(&sys::VA_INVALID_SURFACE) {
            // Destroy whatever did get created rather than leaking it.
            let mut good: Vec<sys::VASurfaceID> = ids
                .iter()
                .copied()
                .filter(|&id| id != sys::VA_INVALID_SURFACE)
                .collect();
            if !good.is_empty() {
                // SAFETY: every id in `good` was returned by the successful
                // vaCreateSurfaces above and has not been destroyed yet.
                unsafe {
                    sys::vaDestroySurfaces(display.handle(), good.as_mut_ptr(), good.len() as i32)
                };
            }
            return Err(Error::Protocol(
                "vaCreateSurfaces succeeded but left VA_INVALID_SURFACE in the array".to_string(),
            ));
        }

        Ok(ids
            .into_iter()
            .map(|id| {
                Arc::new(Surface {
                    display: Arc::clone(display),
                    id,
                    spec: *spec,
                })
            })
            .collect())
    }

    /// Block until every operation targeting this surface has completed.
    pub fn sync(&self) -> Result<()> {
        // SAFETY: `self.id` is live for the lifetime of `self` on
        // `self.display`. vaSyncSurface only blocks; it mutates no Rust state.
        let status = unsafe { sys::vaSyncSurface(self.display.handle(), self.id) };
        check("vaSyncSurface", status)
    }

    /// Export as DRM PRIME (`VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2`).
    ///
    /// `flags` is a combination of `VA_EXPORT_SURFACE_*`. This performs no
    /// synchronisation: call [`Surface::sync`] first if the contents will be
    /// read.
    pub fn export_prime(&self, flags: u32) -> Result<PrimeSurface> {
        let mut desc = sys::VADRMPRIMESurfaceDescriptor::default();
        // SAFETY: `desc` is a valid, fully initialized (zeroed) descriptor of
        // exactly the type `VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2` selects
        // (va_drmcommon.h:96-99); passing any other mem_type with this pointer
        // would be the unsound case, which is why mem_type is not a parameter.
        let status = unsafe {
            sys::vaExportSurfaceHandle(
                self.display.handle(),
                self.id,
                sys::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2,
                flags,
                (&raw mut desc).cast(),
            )
        };
        check("vaExportSurfaceHandle", status)?;
        PrimeSurface::from_raw(desc)
    }

    /// A directly mapped view of the surface's own memory (`vaDeriveImage`).
    ///
    /// Falls back is the caller's business: drivers may refuse to derive (tiled
    /// or compressed layouts), reporting `VA_STATUS_ERROR_OPERATION_FAILED`.
    pub fn derive_image(self: &Arc<Self>) -> Result<Image> {
        let mut image = sys::VAImage::default();
        // SAFETY: valid display and surface id; `image` is a valid
        // out-parameter that libva fully overwrites on success.
        let status = unsafe { sys::vaDeriveImage(self.display.handle(), self.id, &raw mut image) };
        check("vaDeriveImage", status)?;
        Ok(Image {
            display: Arc::clone(&self.display),
            _surface: Some(Arc::clone(self)),
            raw: image,
        })
    }

    /// Copy the surface into an [`Image`] created with [`Image::create`].
    ///
    /// The readback path for drivers that refuse [`Surface::derive_image`].
    /// The image must be at least as large as the requested region.
    pub fn read_into(
        &self,
        image: &mut Image,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    ) -> Result<()> {
        let (img_w, img_h) = image.size();
        if width > img_w || height > img_h {
            return Err(Error::Protocol(format!(
                "vaGetImage region {width}x{height} exceeds image {img_w}x{img_h}"
            )));
        }
        // SAFETY: valid display and surface id; `image.raw.image_id` is a live
        // image on the same display. The region is bounded above by the image
        // size, which is what vaGetImage requires (va.h:4827).
        let status = unsafe {
            sys::vaGetImage(
                self.display.handle(),
                self.id,
                x,
                y,
                width,
                height,
                image.raw.image_id,
            )
        };
        check("vaGetImage", status)
    }

    /// Copy an [`Image`] into the surface (`vaPutImage`).
    ///
    /// The upload path an encoder needs: fill a standalone image with source
    /// pixels, then hand it to the driver, which detiles and converts on the
    /// way in. The region is `(0, 0, width, height)` on both sides — scaling
    /// during upload is a video-processing job, not an encoder's.
    pub fn write_from(&self, image: &Image, width: u32, height: u32) -> Result<()> {
        let (img_w, img_h) = image.size();
        if width > img_w || height > img_h {
            return Err(Error::Protocol(format!(
                "vaPutImage region {width}x{height} exceeds image {img_w}x{img_h}"
            )));
        }
        // SAFETY: valid display and surface id; `image.raw.image_id` is a live
        // image on the same display, and the region is bounded above by both
        // the image and the surface size (va.h:4860).
        let status = unsafe {
            sys::vaPutImage(
                self.display.handle(),
                self.id,
                image.raw.image_id,
                0,
                0,
                width,
                height,
                0,
                0,
                width,
                height,
            )
        };
        check("vaPutImage", status)
    }

    /// The surface id, for FFI-level callers (`ec-hw` picture parameters).
    pub fn id(&self) -> sys::VASurfaceID {
        self.id
    }

    /// The spec this surface was allocated with.
    pub fn spec(&self) -> &SurfaceSpec {
        &self.spec
    }

    /// Allocated size in pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.spec.width, self.spec.height)
    }

    /// The display this surface belongs to.
    pub fn display(&self) -> &Arc<Display> {
        &self.display
    }
}

fn int_attrib(type_: sys::VASurfaceAttribType, value: i32) -> sys::VASurfaceAttrib {
    sys::VASurfaceAttrib {
        type_,
        flags: sys::VA_SURFACE_ATTRIB_SETTABLE,
        value: sys::VAGenericValue {
            type_: sys::VAGenericValueTypeInteger,
            value: sys::VAGenericValueUnion { i: value },
        },
    }
}

impl std::fmt::Debug for Surface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Surface")
            .field("id", &self.id)
            .field("size", &(self.spec.width, self.spec.height))
            .finish()
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        let mut id = self.id;
        // SAFETY: `id` is live and belongs to `self.display`. Any context that
        // used it held an `Arc<Surface>`, so it was destroyed first; any
        // derived Image also held one.
        unsafe { sys::vaDestroySurfaces(self.display.handle(), &raw mut id, 1) };
    }
}

/// One DRM object (buffer object) of an exported surface.
#[derive(Debug)]
pub struct PrimeObject {
    /// The PRIME file descriptor, owned and closed on drop.
    pub fd: OwnedFd,
    /// Total size of the object in bytes.
    pub size: u32,
    /// DRM format modifier (tiling/compression layout).
    pub modifier: u64,
}

/// One layer (plane group) of an exported surface.
#[derive(Debug, Clone)]
pub struct PrimeLayer {
    /// `DRM_FORMAT_*` FourCC of this layer.
    pub drm_format: u32,
    /// Per-plane `(object_index, offset, pitch)`.
    pub planes: Vec<(u32, u32, u32)>,
}

/// A surface exported as DRM PRIME file descriptors.
///
/// Owns the descriptors: dropping this closes them. libva hands over ownership
/// on export (`va.h:4155`), so anything less would leak one fd per frame.
#[derive(Debug)]
pub struct PrimeSurface {
    /// Surface FourCC (`VA_FOURCC_*`).
    pub fourcc: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Backing DRM objects.
    pub objects: Vec<PrimeObject>,
    /// Layers referencing those objects.
    pub layers: Vec<PrimeLayer>,
}

impl PrimeSurface {
    /// Take ownership of the fds in a filled descriptor.
    fn from_raw(desc: sys::VADRMPRIMESurfaceDescriptor) -> Result<PrimeSurface> {
        // The array is 4 elements wide; a driver reporting more would mean our
        // transcription and the runtime disagree about the struct, so refuse
        // rather than read past the end.
        if desc.num_objects as usize > desc.objects.len()
            || desc.num_layers as usize > desc.layers.len()
        {
            return Err(Error::Protocol(format!(
                "vaExportSurfaceHandle reported {} objects / {} layers, max 4 each",
                desc.num_objects, desc.num_layers
            )));
        }

        let mut objects = Vec::with_capacity(desc.num_objects as usize);
        for obj in &desc.objects[..desc.num_objects as usize] {
            if obj.fd < 0 {
                return Err(Error::Protocol(format!(
                    "vaExportSurfaceHandle returned fd {} for an exported object",
                    obj.fd
                )));
            }
            objects.push(PrimeObject {
                // SAFETY: libva transfers ownership of every exported fd to the
                // caller (va.h:4155). Each fd appears exactly once in the
                // descriptor, so it is wrapped exactly once here and closed
                // exactly once when this `PrimeSurface` drops.
                fd: unsafe { OwnedFd::from_raw_fd(obj.fd) },
                size: obj.size,
                modifier: obj.drm_format_modifier,
            });
        }

        let mut layers = Vec::with_capacity(desc.num_layers as usize);
        for layer in &desc.layers[..desc.num_layers as usize] {
            if layer.num_planes as usize > layer.object_index.len() {
                return Err(Error::Protocol(format!(
                    "vaExportSurfaceHandle reported {} planes in a layer, max 4",
                    layer.num_planes
                )));
            }
            layers.push(PrimeLayer {
                drm_format: layer.drm_format,
                planes: (0..layer.num_planes as usize)
                    .map(|i| (layer.object_index[i], layer.offset[i], layer.pitch[i]))
                    .collect(),
            });
        }

        Ok(PrimeSurface {
            fourcc: desc.fourcc,
            width: desc.width,
            height: desc.height,
            objects,
            layers,
        })
    }
}

/// A `VAImage` bound to a surface (derived) or standalone (created).
pub struct Image {
    display: Arc<Display>,
    /// Kept alive because a derived image aliases the surface's memory.
    _surface: Option<Arc<Surface>>,
    raw: sys::VAImage,
}

impl Image {
    /// Allocate a standalone image to read a surface back into
    /// ([`Surface::read_into`]).
    ///
    /// `fourcc` must be one of [`crate::CapReport::image_formats`]; the other
    /// [`sys::VAImageFormat`] fields are only meaningful for RGB layouts and
    /// are left zero, as libva's own examples do for planar YUV.
    pub fn create(display: &Arc<Display>, fourcc: u32, width: u32, height: u32) -> Result<Image> {
        let mut format = sys::VAImageFormat {
            fourcc,
            byte_order: 1, // VA_LSB_FIRST
            bits_per_pixel: match fourcc {
                sys::VA_FOURCC_NV12 | sys::VA_FOURCC_I420 | sys::VA_FOURCC_YV12 => 12,
                sys::VA_FOURCC_P010 => 24,
                _ => 0,
            },
            ..sys::VAImageFormat::default()
        };
        let mut image = sys::VAImage::default();
        // SAFETY: `format` and `image` are valid, initialized in/out parameters
        // that outlive the call; libva copies the format and fully overwrites
        // `image` on success.
        let status = unsafe {
            sys::vaCreateImage(
                display.handle(),
                &raw mut format,
                width as i32,
                height as i32,
                &raw mut image,
            )
        };
        check("vaCreateImage", status)?;
        Ok(Image {
            display: Arc::clone(display),
            _surface: None,
            raw: image,
        })
    }

    /// Pixel format FourCC.
    pub fn fourcc(&self) -> u32 {
        self.raw.format.fourcc
    }

    /// `(width, height)` of the image, which may exceed the requested size.
    pub fn size(&self) -> (u32, u32) {
        (self.raw.width as u32, self.raw.height as u32)
    }

    /// Number of planes (at most 3).
    pub fn num_planes(&self) -> u32 {
        self.raw.num_planes.min(3)
    }

    /// Row pitch of plane `i` in bytes.
    pub fn pitch(&self, plane: usize) -> Option<u32> {
        self.raw.pitches.get(plane).copied()
    }

    /// Map the image's buffer into this process.
    ///
    /// The returned guard unmaps on drop. That is not politeness: an unmatched
    /// `vaMapBuffer` leaks the mapping for the lifetime of the display, which
    /// is how the incumbent stack lost ~3MB per frame.
    pub fn map(&mut self) -> Result<MappedImage<'_>> {
        let mut ptr: *mut std::ffi::c_void = std::ptr::null_mut();
        // SAFETY: `self.raw.buf` is the image buffer libva created and still
        // owns; `ptr` is a valid out-parameter. On success libva guarantees the
        // mapping stays valid until vaUnmapBuffer, which `MappedImage::drop`
        // calls exactly once.
        let status = unsafe { sys::vaMapBuffer(self.display.handle(), self.raw.buf, &raw mut ptr) };
        check("vaMapBuffer", status)?;
        if ptr.is_null() {
            // SAFETY: the map call reported success, so the buffer is mapped
            // and must be unmapped even though the pointer is unusable.
            unsafe { sys::vaUnmapBuffer(self.display.handle(), self.raw.buf) };
            return Err(Error::Protocol(
                "vaMapBuffer succeeded but returned NULL".to_string(),
            ));
        }
        Ok(MappedImage {
            image: self,
            base: ptr.cast::<u8>(),
        })
    }
}

impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Image")
            .field("fourcc", &crate::caps::fourcc_str(self.raw.format.fourcc))
            .field("size", &(self.raw.width, self.raw.height))
            .field("num_planes", &self.raw.num_planes)
            .field("data_size", &self.raw.data_size)
            .finish()
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        // SAFETY: `image_id` came from vaDeriveImage/vaCreateImage on
        // `self.display` and is destroyed once. Any mapping was released
        // first: `map` borrows `self` mutably, so no `MappedImage` can outlive
        // this. The surface (if any) is released after, as libva requires.
        unsafe { sys::vaDestroyImage(self.display.handle(), self.raw.image_id) };
    }
}

/// A mapped [`Image`]. Unmaps on drop.
pub struct MappedImage<'a> {
    image: &'a mut Image,
    base: *mut u8,
}

impl MappedImage<'_> {
    /// Byte range of plane `i` within the mapped buffer, as `(offset, len)`.
    ///
    /// The length runs to the next plane's offset, or to `data_size` for the
    /// last plane — libva does not report per-plane sizes.
    fn plane_range(&self, plane: usize) -> Option<(usize, usize)> {
        let n = self.image.num_planes() as usize;
        if plane >= n {
            return None;
        }
        let raw = &self.image.raw;
        let offset = raw.offsets[plane] as usize;
        let end = raw
            .offsets
            .get(plane + 1)
            .copied()
            .filter(|_| plane + 1 < n)
            .unwrap_or(raw.data_size) as usize;
        // A driver reporting decreasing or out-of-range offsets would otherwise
        // produce a slice past the mapping.
        if end < offset || end > raw.data_size as usize {
            return None;
        }
        Some((offset, end - offset))
    }

    /// Read-only view of plane `plane`.
    pub fn plane(&self, plane: usize) -> Option<&[u8]> {
        let (offset, len) = self.plane_range(plane)?;
        // SAFETY: `base` points at a mapping of `data_size` bytes that libva
        // keeps valid until vaUnmapBuffer (called only in `drop`). `plane_range`
        // guarantees `offset + len <= data_size`, so the slice stays inside the
        // mapping. The borrow is tied to `&self`, so it cannot outlive it.
        Some(unsafe { std::slice::from_raw_parts(self.base.add(offset), len) })
    }

    /// Mutable view of plane `plane`, for upload paths.
    pub fn plane_mut(&mut self, plane: usize) -> Option<&mut [u8]> {
        let (offset, len) = self.plane_range(plane)?;
        // SAFETY: as `plane`, plus: `&mut self` guarantees no other slice into
        // the mapping is alive, so this is the only reference to those bytes.
        Some(unsafe { std::slice::from_raw_parts_mut(self.base.add(offset), len) })
    }

    /// Row pitch of plane `plane` in bytes.
    pub fn pitch(&self, plane: usize) -> Option<u32> {
        self.image.pitch(plane)
    }

    /// The image being mapped.
    pub fn image(&self) -> &Image {
        self.image
    }
}

impl Drop for MappedImage<'_> {
    fn drop(&mut self) {
        // SAFETY: the buffer was mapped in `Image::map` and is unmapped exactly
        // once, here; `self.image` is still alive because this guard borrows it.
        unsafe { sys::vaUnmapBuffer(self.image.display.handle(), self.image.raw.buf) };
    }
}
