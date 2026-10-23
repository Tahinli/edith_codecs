//! VA surfaces: creation, DRM PRIME export and import, timed sync.

use std::os::fd::AsRawFd;
use std::rc::Rc;
use std::time::{Duration, Instant};

use cros_libva::{
    DrmPrimeSurfaceDescriptor, ExternalBufferDescriptor, MemoryType, UsageHint,
    VADRMPRIMESurfaceDescriptor, VASurfaceStatus,
};

use crate::display::Display;
use crate::error::{Error, Result, va_ok};

pub use cros_libva::Image;

/// A VA surface with no client-side memory descriptor: the driver allocates it.
pub type Surface = cros_libva::Surface<()>;

/// What a pool of surfaces is made of.
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
    /// 8-bit 4:2:0 NV12.
    pub fn nv12(width: u32, height: u32) -> Self {
        SurfaceSpec {
            rt_format: cros_libva::VA_RT_FORMAT_YUV420,
            fourcc: Some(cros_libva::VA_FOURCC_NV12),
            width,
            height,
            usage_hint: 0,
        }
    }

    /// 10-bit 4:2:0 P010 (samples in the top 10 bits of 16-bit words).
    pub fn p010(width: u32, height: u32) -> Self {
        SurfaceSpec {
            rt_format: cros_libva::VA_RT_FORMAT_YUV420_10,
            fourcc: Some(cros_libva::VA_FOURCC_P010),
            width,
            height,
            usage_hint: 0,
        }
    }

    /// The same spec with a `VA_SURFACE_ATTRIB_USAGE_HINT_*` bitmask.
    pub fn with_usage_hint(mut self, hint: u32) -> Self {
        self.usage_hint = hint;
        self
    }
}

/// Allocate `count` surfaces described by `spec` (`vaCreateSurfaces`).
pub fn create_pool(
    display: &Rc<Display>,
    spec: &SurfaceSpec,
    count: usize,
) -> Result<Vec<Surface>> {
    let usage = (spec.usage_hint != 0).then(|| UsageHint::from_bits_truncate(spec.usage_hint));
    va_ok(
        "vaCreateSurfaces",
        display.create_surfaces(
            spec.rt_format,
            spec.fourcc,
            spec.width,
            spec.height,
            usage,
            vec![(); count],
        ),
    )
}

/// One exported memory object: a DMA-BUF file descriptor and its size.
#[derive(Debug)]
pub struct PrimeObject {
    /// The DMA-BUF. Owned: it is closed when this value drops.
    pub fd: std::os::fd::OwnedFd,
    /// Size of the object in bytes.
    pub size: u32,
    /// DRM format modifier of the object.
    pub modifier: u64,
}

/// One layer of an exported surface: a DRM format and its planes.
#[derive(Debug, Clone)]
pub struct PrimeLayer {
    /// The DRM fourcc of this layer.
    pub drm_format: u32,
    /// `(object index, offset, pitch)` for each plane.
    pub planes: Vec<(u32, u32, u32)>,
}

/// A surface exported as DRM PRIME: its objects (DMA-BUFs) and layers.
#[derive(Debug)]
pub struct PrimeSurface {
    /// Pixel format fourcc.
    pub fourcc: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// The memory objects backing the surface.
    pub objects: Vec<PrimeObject>,
    /// The layers referring to those objects.
    pub layers: Vec<PrimeLayer>,
}

impl From<DrmPrimeSurfaceDescriptor> for PrimeSurface {
    fn from(d: DrmPrimeSurfaceDescriptor) -> Self {
        PrimeSurface {
            fourcc: d.fourcc,
            width: d.width,
            height: d.height,
            objects: d
                .objects
                .into_iter()
                .map(|o| PrimeObject {
                    fd: o.fd,
                    size: o.size,
                    modifier: o.drm_format_modifier,
                })
                .collect(),
            layers: d
                .layers
                .into_iter()
                .map(|l| PrimeLayer {
                    drm_format: l.drm_format,
                    planes: (0..l.num_planes as usize)
                        .map(|i| (u32::from(l.object_index[i]), l.offset[i], l.pitch[i]))
                        .collect(),
                })
                .collect(),
        }
    }
}

/// Surface operations that need more than the cros-libva surface offers.
pub trait SurfaceExt {
    /// Export as DRM PRIME for zero-copy import elsewhere
    /// (`VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2`, read-only, composed layers).
    /// Does not synchronise: call [`SurfaceExt::sync_timeout`] first.
    fn export(&self) -> Result<PrimeSurface>;

    /// Wait until the surface is no longer being rendered or displayed, or
    /// fail with `VA_STATUS_ERROR_TIMEDOUT` after `timeout`.
    ///
    /// cros-libva does not wrap `vaSyncSurface2`, so this polls
    /// `vaQuerySurfaceStatus` with a short back-off until the deadline.
    fn sync_timeout(&self, timeout: Duration) -> Result<()>;
}

impl<D: cros_libva::SurfaceMemoryDescriptor> SurfaceExt for cros_libva::Surface<D> {
    fn export(&self) -> Result<PrimeSurface> {
        let d = va_ok("vaExportSurfaceHandle", self.export_prime())?;
        Ok(PrimeSurface::from(d))
    }

    fn sync_timeout(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let mut pause = Duration::from_micros(50);
        loop {
            let status = va_ok("vaQuerySurfaceStatus", self.query_status())?;
            if status & (VASurfaceStatus::VASurfaceRendering | VASurfaceStatus::VASurfaceDisplaying)
                == 0
            {
                return Ok(());
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(Error::Va {
                    op: "vaSyncSurface",
                    status: cros_libva::VA_STATUS_ERROR_TIMEDOUT as i32,
                    message: "surface still busy at the deadline".to_string(),
                });
            }
            std::thread::sleep(pause.min(deadline - now));
            pause = (pause * 2).min(Duration::from_millis(2));
        }
    }
}

/// A PRIME descriptor as a cros-libva memory descriptor: its
/// `VASurfaceAttribExternalBuffers` attribute is the descriptor itself.
/// The memory descriptor of a surface imported from DRM PRIME.
pub struct PrimeImport(VADRMPRIMESurfaceDescriptor);

impl ExternalBufferDescriptor for PrimeImport {
    const MEMORY_TYPE: MemoryType = MemoryType::DrmPrime2;
    type DescriptorAttribute = VADRMPRIMESurfaceDescriptor;

    fn va_surface_attribute(&mut self) -> VADRMPRIMESurfaceDescriptor {
        self.0
    }
}

/// Import a DRM PRIME surface as a VA surface (`vaCreateSurfaces` with
/// `VASurfaceAttribExternalBuffers`). The descriptors' file descriptors are
/// borrowed for the call: the caller keeps ownership and closes them.
pub fn import_prime(
    display: &Rc<Display>,
    prime: &PrimeSurface,
) -> Result<cros_libva::Surface<PrimeImport>> {
    if prime.objects.len() > 4 || prime.layers.len() > 4 {
        return Err(Error::Protocol(format!(
            "a PRIME surface has {} objects and {} layers, max 4 each",
            prime.objects.len(),
            prime.layers.len()
        )));
    }
    let mut desc = VADRMPRIMESurfaceDescriptor {
        fourcc: prime.fourcc,
        width: prime.width,
        height: prime.height,
        num_objects: prime.objects.len() as u32,
        num_layers: prime.layers.len() as u32,
        ..Default::default()
    };
    for (i, o) in prime.objects.iter().enumerate() {
        desc.objects[i].fd = o.fd.as_raw_fd();
        desc.objects[i].size = o.size;
        desc.objects[i].drm_format_modifier = o.modifier;
    }
    for (i, l) in prime.layers.iter().enumerate() {
        if l.planes.len() > 4 {
            return Err(Error::Protocol(format!(
                "a PRIME layer has {} planes, max 4",
                l.planes.len()
            )));
        }
        desc.layers[i].drm_format = l.drm_format;
        desc.layers[i].num_planes = l.planes.len() as u32;
        for (p, &(obj, offset, pitch)) in l.planes.iter().enumerate() {
            desc.layers[i].object_index[p] = obj;
            desc.layers[i].offset[p] = offset;
            desc.layers[i].pitch[p] = pitch;
        }
    }
    let mut out = va_ok(
        "vaCreateSurfaces",
        display.create_surfaces(
            cros_libva::VA_RT_FORMAT_YUV420,
            Some(prime.fourcc),
            prime.width,
            prime.height,
            None,
            vec![PrimeImport(desc)],
        ),
    )?;
    out.pop()
        .ok_or_else(|| Error::Protocol("vaCreateSurfaces returned no surface".into()))
}

/// The `VAImageFormat` the driver offers for `fourcc` (`vaQueryImageFormats`).
pub fn image_format(display: &Display, fourcc: u32) -> Result<cros_libva::VAImageFormat> {
    va_ok("vaQueryImageFormats", display.query_image_formats())?
        .into_iter()
        .find(|f| f.fourcc == fourcc)
        .ok_or_else(|| {
            Error::Protocol(format!(
                "the driver offers no image format for {}",
                crate::caps::fourcc_str(fourcc)
            ))
        })
}

/// `vaCreateImage` plus `vaGetImage`: a copy of `surface`'s pixels in an image
/// of `format`, of `coded` size, showing `visible`.
pub fn create_image<'a, D: cros_libva::SurfaceMemoryDescriptor>(
    surface: &'a cros_libva::Surface<D>,
    format: cros_libva::VAImageFormat,
    coded: (u32, u32),
    visible: (u32, u32),
) -> Result<Image<'a>> {
    va_ok(
        "vaGetImage",
        Image::create_from(surface, format, coded, visible),
    )
}
