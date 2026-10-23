//! Contexts, typed parameter buffers and the decode/encode picture protocol.
//!
//! The protocol is cros-libva's typestate `Picture`: `new -> begin -> render
//! -> end -> sync`, each step consuming the previous state. [`submit`] runs the
//! first four steps for the common case.

use std::borrow::Borrow;
use std::rc::Rc;

use cros_libva::{BufferType, SurfaceMemoryDescriptor};

use crate::config::Config;
use crate::display::Display;
use crate::error::{Result, va_ok};

pub use cros_libva::{
    Buffer, Context, EncCodedBuffer, MappedCodedBuffer, MappedCodedSegment, Picture, PictureBegin,
    PictureEnd, PictureNew, PictureRender, PictureSync,
};

/// `vaCreateContext` for `config` at `coded_width` x `coded_height`, with the
/// given surfaces as its render targets. `progressive` selects a progressive
/// (non-interlaced) context.
pub fn create_context<D: SurfaceMemoryDescriptor>(
    display: &Rc<Display>,
    config: &Config,
    coded_width: u32,
    coded_height: u32,
    render_targets: Option<&Vec<cros_libva::Surface<D>>>,
    progressive: bool,
) -> Result<Rc<Context>> {
    va_ok(
        "vaCreateContext",
        display.create_context(
            config.raw(),
            coded_width,
            coded_height,
            render_targets,
            progressive,
        ),
    )
}

/// Create one parameter, slice or data buffer on `context` (`vaCreateBuffer`).
pub fn buffer(context: &Rc<Context>, ty: BufferType) -> Result<Buffer> {
    va_ok("vaCreateBuffer", context.create_buffer(ty))
}

/// Submit one picture: `begin`, one `render` carrying every buffer in
/// `buffers`, then `end`. The result is still to be synced by the caller.
pub fn submit<D, T>(
    context: &Rc<Context>,
    target: T,
    buffers: Vec<Buffer>,
) -> Result<Picture<PictureEnd, T>>
where
    D: SurfaceMemoryDescriptor,
    T: Borrow<cros_libva::Surface<D>>,
{
    let mut picture = Picture::new(0, Rc::clone(context), target);
    for b in buffers {
        picture.add_buffer(b);
    }
    let begun = va_ok("vaBeginPicture", picture.begin())?;
    let rendered = va_ok("vaRenderPicture", begun.render())?;
    va_ok("vaEndPicture", rendered.end())
}
