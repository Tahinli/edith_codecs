//! [`Vpp`] — a `VAProfileNone` / `VAEntrypointVideoProc` video-processing
//! pipeline: GPU-resident surface conversion with no system-memory round trip.
//!
//! This is the same `Config`/`Context`/`Buffer`/`Picture` machinery decode and
//! encode use: VPP is another entrypoint on the same display.
//!
//! There is no filter query. cros-libva does not wrap
//! `vaQueryVideoProcFilters`, so a caller that wants a filter asks for it and
//! handles the error from the pipeline call instead.

use std::rc::Rc;

use cros_libva::{
    ProcColorProperties, ProcPipelineParameterBuffer, SurfaceMemoryDescriptor, VARectangle,
};

use crate::caps::{Entrypoint, Profile};
use crate::config::{Config, ConfigAttrib};
use crate::display::Display;
use crate::error::{Result, va_ok};
use crate::picture::{Context, buffer, create_context, submit};

/// A video-processing pipeline bound to one display.
pub struct Vpp {
    context: Rc<Context>,
}

/// A rectangle as libva takes it: `(x, y, width, height)` in pixels.
pub type Region = (u32, u32, u32, u32);

fn rect(r: Region) -> VARectangle {
    VARectangle {
        x: r.0 as i16,
        y: r.1 as i16,
        width: r.2 as u16,
        height: r.3 as u16,
    }
}

impl Vpp {
    /// Build a pipeline whose config accepts `rt_formats` (the OR of every
    /// `VA_RT_FORMAT_*` a surface fed through it will use). The context is
    /// sized `width` x `height`, which is informational: each submission
    /// carries its own source and destination.
    pub fn new(display: &Rc<Display>, rt_formats: u32, width: u32, height: u32) -> Result<Vpp> {
        let config = Config::new(
            display,
            Profile::None,
            Entrypoint::VideoProc,
            &[ConfigAttrib::rt_format(rt_formats)],
        )?;
        let context = create_context::<()>(display, &config, width, height, None, true)?;
        Ok(Vpp { context })
    }

    /// Convert `source` into `dest`: one pipeline-parameter buffer, one
    /// `begin`/`render`/`end` submission, whole-surface, driver-chosen colour
    /// handling. Returns `dest`, synced and ready to read or hand to an encoder.
    pub fn convert<D, E>(
        &self,
        source: &cros_libva::Surface<D>,
        dest: Rc<cros_libva::Surface<E>>,
    ) -> Result<Rc<cros_libva::Surface<E>>>
    where
        D: SurfaceMemoryDescriptor,
        E: SurfaceMemoryDescriptor,
    {
        self.convert_region(source, (0, 0, 0, 0), dest, (0, 0, 0, 0))
    }

    /// Convert `source_region` of `source` into `dest_region` of `dest` with
    /// no scaling (both regions are the same size in every caller), leaving
    /// the rest of `dest` as it was. A zero-sized region means "the whole
    /// surface". This is how a visible picture smaller than its block-aligned
    /// coded surface lands inside a differently-padded destination.
    pub fn convert_region<D, E>(
        &self,
        source: &cros_libva::Surface<D>,
        source_region: Region,
        dest: Rc<cros_libva::Surface<E>>,
        dest_region: Region,
    ) -> Result<Rc<cros_libva::Surface<E>>>
    where
        D: SurfaceMemoryDescriptor,
        E: SurfaceMemoryDescriptor,
    {
        let region = |r: Region| (r.2 != 0 && r.3 != 0).then(|| rect(r));
        let param = ProcPipelineParameterBuffer::new(
            source.id(),
            region(source_region),
            0,
            region(dest_region),
            0,
            0,
            0,
            0,
            None,
            None,
            None,
            0,
            None,
            0,
            None,
            0,
            0,
            ProcColorProperties::default(),
            ProcColorProperties::default(),
            0,
            None,
        );
        let buf = buffer(
            &self.context,
            cros_libva::BufferType::ProcPipelineParameter(param),
        )?;
        let synced = submit(&self.context, Rc::clone(&dest), vec![buf])?;
        drop(synced);
        va_ok("vaSyncSurface", dest.sync())?;
        Ok(dest)
    }
}
