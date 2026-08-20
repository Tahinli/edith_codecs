//! [`Vpp`] — a `VAProfileNone` / `VAEntrypointVideoProc` video-processing
//! pipeline: GPU-resident surface conversion (format, and any filter the
//! driver advertises) with no system-memory round trip.
//!
//! This is the same `Config`/`Context`/`Buffer`/`Picture` machinery decode and
//! encode use — VPP is just another entrypoint on the same display, and
//! `Picture`'s typestate protocol applies unchanged.

use std::sync::Arc;

use crate::caps::{Entrypoint, Profile};
use crate::config::{Config, ConfigAttrib};
use crate::display::Display;
use crate::error::Result;
use crate::picture::{Buffer, Context, Picture};
use crate::surface::Surface;
use crate::sys;

/// A video-processing pipeline bound to one display.
pub struct Vpp {
    context: Arc<Context>,
}

impl Vpp {
    /// Build a pipeline whose config accepts `rt_formats` (the OR of every
    /// `VA_RT_FORMAT_*` a surface fed through it will use, source or
    /// destination — one config covers both ends of a conversion) and whose
    /// context targets `targets` (every surface this pipeline will render
    /// into; VA-API contexts, like decode/encode ones, are created with the
    /// full set of render targets up front).
    pub fn new(display: &Arc<Display>, rt_formats: u32, targets: &[Arc<Surface>]) -> Result<Vpp> {
        let config = Config::new(
            display,
            Profile::None,
            Entrypoint::VideoProc,
            &[ConfigAttrib::rt_format(rt_formats)],
        )?;
        // The size a VPP context is created with is informational only — each
        // submission carries its own source and destination surfaces, which
        // may be smaller or larger — so the first target's size is as good a
        // hint as any.
        let (width, height) = targets
            .first()
            .map(|s| s.size())
            .unwrap_or((1, 1));
        let context = Context::new(&config, width, height, 0, targets)?;
        Ok(Vpp { context })
    }

    /// `VAProcFilterType` values this pipeline's driver advertises
    /// (`vaQueryVideoProcFilters`), e.g. to check for
    /// `VAProcFilterHighDynamicRangeToneMapping` before asking for it.
    pub fn filters(&self) -> Result<Vec<i32>> {
        let mut filters = vec![0i32; sys::VAProcFilterCount as usize];
        let mut count = filters.len() as u32;
        // SAFETY: `filters` has room for exactly `count` entries and `count`
        // is passed alongside; libva writes only within that bound and
        // rewrites `count` to the number actually filled in (va_vpp.h:1518).
        let status = unsafe {
            sys::vaQueryVideoProcFilters(
                self.context.display().handle(),
                self.context_id(),
                filters.as_mut_ptr(),
                &mut count,
            )
        };
        crate::error::check("vaQueryVideoProcFilters", status)?;
        filters.truncate(count as usize);
        Ok(filters)
    }

    /// Convert `source` into `dest`: one pipeline-parameter buffer, one
    /// `begin`/`render`/`end`/`sync` submission, whole-surface, no filters,
    /// driver-chosen colour handling — the plain format conversion this
    /// crate exists for. Returns `dest` back, synced and ready to read or
    /// hand to an encoder.
    pub fn convert(&self, source: &Arc<Surface>, dest: Arc<Surface>) -> Result<Arc<Surface>> {
        let param = sys::VAProcPipelineParameterBuffer {
            surface: source.id(),
            ..Default::default()
        };
        // SAFETY: `VAProcPipelineParameterBuffer` is the exact repr(C)
        // parameter struct for `VAProcPipelineParameterBufferType`
        // (va_vpp.h:886); it has no padding-sensitive invariants beyond the
        // zeroed reserved tail `Default` already establishes.
        let buffer = unsafe {
            Buffer::from_param(&self.context, sys::VAProcPipelineParameterBufferType, &param)?
        };
        let surface = Picture::new(&self.context, dest)
            .begin()?
            .render(buffer)?
            .end()?
            .sync()?
            .into_surface();
        Ok(surface)
    }

    fn context_id(&self) -> sys::VAContextID {
        // `Context` does not expose its raw id outside the crate; VPP needs
        // it for `vaQueryVideoProcFilters`, which has no typestate-protected
        // equivalent to route through `Picture`.
        self.context.raw_id()
    }
}
