//! [`Context`], [`Buffer`] and the [`Picture`] typestate machine.
//!
//! VA-API's submission protocol is a state machine: `vaBeginPicture` ->
//! N x `vaRenderPicture` -> `vaEndPicture` -> `vaSyncSurface`. Calling those out
//! of order is a driver-defined mess (at best an error, at worst a GPU hang),
//! and it is not something a code review reliably catches.
//!
//! So the order is in the type system. Each transition consumes the picture and
//! returns it in the next state; there is no method to skip a step and no way
//! to name an intermediate state that was never reached:
//!
//! ```text
//! Picture<New> --begin()--> Picture<Rendering> --render(buf)--> Picture<Rendering>
//!                                              --end()------->  Picture<Ended>
//!                                              --sync()------>  Picture<Synced>
//! ```
//!
//! The legal walk compiles:
//!
//! ```no_run
//! # use ec_va::{Buffer, Context, Picture, Surface};
//! # fn demo(context: &std::sync::Arc<Context>, target: std::sync::Arc<Surface>, buffer: Buffer)
//! #     -> ec_va::Result<()> {
//! let surface = Picture::new(context, target)
//!     .begin()?
//!     .render(buffer)?
//!     .end()?
//!     .sync()?
//!     .into_surface();
//! # Ok(())
//! # }
//! ```
//!
//! Ordering violations are compile errors (`E0599`: the method does not exist
//! on that state):
//!
//! ```compile_fail,E0599
//! # use ec_va::{Context, Picture, Surface};
//! # fn demo(context: &std::sync::Arc<Context>, target: std::sync::Arc<Surface>) {
//! let picture = Picture::new(context, target);
//! // ERROR: no method `end` on `Picture<New>` — begin() was never called.
//! let ended = picture.end().unwrap();
//! # }
//! ```
//!
//! ```compile_fail,E0599
//! # use ec_va::{Context, Picture, Surface};
//! # fn demo(context: &std::sync::Arc<Context>, target: std::sync::Arc<Surface>) {
//! let picture = Picture::new(context, target).begin().unwrap();
//! // ERROR: no method `sync` on `Picture<Rendering>` — end() must come first.
//! let synced = picture.sync().unwrap();
//! # }
//! ```
//!
//! ```compile_fail,E0599
//! # use ec_va::{Buffer, Context, Picture, Surface};
//! # fn demo(context: &std::sync::Arc<Context>, target: std::sync::Arc<Surface>, buffer: Buffer) {
//! let picture = Picture::new(context, target).begin().unwrap().end().unwrap();
//! // ERROR: no method `render` on `Picture<Ended>` — the picture is closed.
//! let rendered = picture.render(buffer).unwrap();
//! # }
//! ```
//!
//! ```compile_fail,E0599
//! # use ec_va::{Context, Picture, Surface};
//! # fn demo(context: &std::sync::Arc<Context>, target: std::sync::Arc<Surface>) {
//! let picture = Picture::new(context, target).begin().unwrap();
//! // ERROR: no method `begin` on `Picture<Rendering>` — no double submission.
//! let again = picture.begin().unwrap();
//! # }
//! ```

use std::marker::PhantomData;
use std::sync::Arc;

use crate::config::Config;
use crate::display::Display;
use crate::error::{Error, Result, check};
use crate::surface::Surface;
use crate::sys;

/// A live `VAContextID`.
///
/// Holds its config and render targets, so neither can be destroyed while the
/// context still references them — the ordering libva demands.
pub struct Context {
    display: Arc<Display>,
    config: Arc<Config>,
    id: sys::VAContextID,
    _targets: Vec<Arc<Surface>>,
    width: u32,
    height: u32,
}

impl Context {
    /// Create a context for `config` at `width` x `height`.
    ///
    /// `targets` are the surfaces the driver may render into. `flags` is
    /// usually [`sys::VA_PROGRESSIVE`].
    pub fn new(
        config: &Arc<Config>,
        width: u32,
        height: u32,
        flags: i32,
        targets: &[Arc<Surface>],
    ) -> Result<Arc<Context>> {
        let display = Arc::clone(config.display());
        let mut ids: Vec<sys::VASurfaceID> = targets.iter().map(|s| s.id()).collect();
        let mut id: sys::VAContextID = sys::VA_INVALID_ID;
        // SAFETY: `ids` is a valid array of `ids.len()` surface ids, all live
        // (their `Arc`s are cloned into the context below, so they outlive it);
        // `id` is a valid out-parameter. libva copies the target array.
        let status = unsafe {
            sys::vaCreateContext(
                display.handle(),
                config.id(),
                width as i32,
                height as i32,
                flags,
                ids.as_mut_ptr(),
                ids.len() as i32,
                &mut id,
            )
        };
        check("vaCreateContext", status)?;
        if id == sys::VA_INVALID_ID {
            return Err(Error::Protocol(
                "vaCreateContext succeeded but returned VA_INVALID_ID".to_string(),
            ));
        }
        Ok(Arc::new(Context {
            display,
            config: Arc::clone(config),
            id,
            _targets: targets.to_vec(),
            width,
            height,
        }))
    }

    /// The config this context was created from.
    pub fn config(&self) -> &Arc<Config> {
        &self.config
    }

    /// The display this context belongs to.
    pub fn display(&self) -> &Arc<Display> {
        &self.display
    }

    /// Coded size the context was created with.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("id", &self.id)
            .field("size", &(self.width, self.height))
            .field("profile", &self.config.profile())
            .finish()
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: `self.id` is live on `self.display`. Buffers hold an
        // `Arc<Context>` and pictures hold buffers, so none can be alive here;
        // the render targets and config are released after this call.
        unsafe { sys::vaDestroyContext(self.display.handle(), self.id) };
    }
}

/// A live `VABufferID` belonging to a context.
pub struct Buffer {
    context: Arc<Context>,
    id: sys::VABufferID,
    type_: sys::VABufferType,
}

impl Buffer {
    /// Create a buffer holding a copy of `data`.
    ///
    /// This is the parameter-buffer path: picture/slice parameters and slice
    /// data are copied into driver memory, so `data` need not outlive the call.
    pub fn from_bytes(
        context: &Arc<Context>,
        type_: sys::VABufferType,
        data: &[u8],
    ) -> Result<Buffer> {
        if data.is_empty() {
            return Err(Error::Protocol(format!(
                "refusing to create an empty VA buffer of type {type_}"
            )));
        }
        let mut id: sys::VABufferID = sys::VA_INVALID_ID;
        // SAFETY: libva copies `size * num_elements` = `data.len()` bytes out
        // of `data` during the call and never retains the pointer (va.h:3820).
        // The cast to `*mut c_void` is sound because libva only reads it: the
        // `data` parameter is documented as `in`.
        let status = unsafe {
            sys::vaCreateBuffer(
                context.display.handle(),
                context.id,
                type_,
                data.len() as u32,
                1,
                data.as_ptr() as *mut std::ffi::c_void,
                &mut id,
            )
        };
        check("vaCreateBuffer", status)?;
        Buffer::wrap(context, id, type_)
    }

    /// Create a buffer holding a copy of one plain-old-data parameter struct.
    ///
    /// # Safety
    ///
    /// `T` must be the exact `#[repr(C)]` parameter struct libva expects for
    /// `type_` (e.g. `VAPictureParameterBufferH264` for
    /// `VAPictureParameterBufferType`) and must contain no padding-sensitive
    /// invariants: the driver reads `size_of::<T>()` bytes of it verbatim.
    pub unsafe fn from_param<T: Copy>(
        context: &Arc<Context>,
        type_: sys::VABufferType,
        param: &T,
    ) -> Result<Buffer> {
        // SAFETY: `T: Copy` with the caller's guarantee that it is a repr(C)
        // POD parameter struct, so its bytes are a valid representation to
        // hand to the driver; the slice covers exactly one `T` and is only
        // read during the call below.
        let bytes =
            unsafe { std::slice::from_raw_parts((param as *const T).cast::<u8>(), size_of::<T>()) };
        Buffer::from_bytes(context, type_, bytes)
    }

    /// Allocate an empty buffer of `size` bytes to be filled via [`Buffer::map`].
    ///
    /// This is the coded-output path: `VAEncCodedBufferType` buffers are
    /// written by the GPU and read back after sync.
    pub fn allocate(context: &Arc<Context>, type_: sys::VABufferType, size: u32) -> Result<Buffer> {
        let mut id: sys::VABufferID = sys::VA_INVALID_ID;
        // SAFETY: a NULL `data` pointer is the documented "allocate only" form
        // (va.h:3826); `id` is a valid out-parameter.
        let status = unsafe {
            sys::vaCreateBuffer(
                context.display.handle(),
                context.id,
                type_,
                size,
                1,
                std::ptr::null_mut(),
                &mut id,
            )
        };
        check("vaCreateBuffer", status)?;
        Buffer::wrap(context, id, type_)
    }

    fn wrap(
        context: &Arc<Context>,
        id: sys::VABufferID,
        type_: sys::VABufferType,
    ) -> Result<Buffer> {
        if id == sys::VA_INVALID_ID {
            return Err(Error::Protocol(
                "vaCreateBuffer succeeded but returned VA_INVALID_ID".to_string(),
            ));
        }
        Ok(Buffer {
            context: Arc::clone(context),
            id,
            type_,
        })
    }

    /// Map the buffer for reading or writing. Unmaps on drop.
    pub fn map(&mut self) -> Result<MappedBuffer<'_>> {
        let mut ptr: *mut std::ffi::c_void = std::ptr::null_mut();
        // SAFETY: `self.id` is a live buffer on this display; `ptr` is a valid
        // out-parameter. libva keeps the mapping valid until vaUnmapBuffer,
        // which `MappedBuffer::drop` calls exactly once.
        let status =
            unsafe { sys::vaMapBuffer(self.context.display.handle(), self.id, &raw mut ptr) };
        check("vaMapBuffer", status)?;
        if ptr.is_null() {
            // SAFETY: the map reported success, so it must be undone even
            // though the returned pointer is unusable.
            unsafe { sys::vaUnmapBuffer(self.context.display.handle(), self.id) };
            return Err(Error::Protocol(
                "vaMapBuffer succeeded but returned NULL".to_string(),
            ));
        }
        Ok(MappedBuffer {
            buffer: self,
            base: ptr.cast::<u8>(),
        })
    }

    /// The buffer id.
    pub fn id(&self) -> sys::VABufferID {
        self.id
    }

    /// The `VABufferType` this buffer was created with.
    pub fn buffer_type(&self) -> sys::VABufferType {
        self.type_
    }
}

impl std::fmt::Debug for Buffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Buffer")
            .field("id", &self.id)
            .field("type", &self.type_)
            .finish()
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        // SAFETY: `self.id` is live and belongs to `self.context`'s display,
        // which outlives it (the `Arc<Context>` is held here). Mappings borrow
        // `&mut self`, so none can be outstanding.
        unsafe { sys::vaDestroyBuffer(self.context.display.handle(), self.id) };
    }
}

/// A mapped [`Buffer`]. Unmaps on drop.
///
/// The length of the mapping is not reported by libva for parameter buffers,
/// so access goes through explicit sizes the caller already knows (for coded
/// buffers, through the `VACodedBufferSegment` list, which `ec-hw` parses).
pub struct MappedBuffer<'a> {
    buffer: &'a mut Buffer,
    base: *mut u8,
}

impl MappedBuffer<'_> {
    /// The raw mapped pointer.
    ///
    /// # Safety
    ///
    /// The caller must not read or write beyond the size the buffer was created
    /// with, and must not keep the pointer past this guard's lifetime.
    pub unsafe fn as_ptr(&self) -> *mut u8 {
        self.base
    }

    /// The first `len` bytes of the mapping.
    ///
    /// # Safety
    ///
    /// `len` must not exceed the size the buffer was created with.
    pub unsafe fn as_slice(&self, len: usize) -> &[u8] {
        // SAFETY: the caller guarantees `len` is within the allocation, and the
        // mapping is live for as long as this guard exists.
        unsafe { std::slice::from_raw_parts(self.base, len) }
    }

    /// The first `len` bytes of the mapping, mutably.
    ///
    /// # Safety
    ///
    /// `len` must not exceed the size the buffer was created with.
    pub unsafe fn as_mut_slice(&mut self, len: usize) -> &mut [u8] {
        // SAFETY: as `as_slice`, plus `&mut self` makes this the only live
        // reference into the mapping.
        unsafe { std::slice::from_raw_parts_mut(self.base, len) }
    }
}

impl Drop for MappedBuffer<'_> {
    fn drop(&mut self) {
        // SAFETY: mapped in `Buffer::map`, unmapped exactly once here; the
        // buffer is still alive because this guard borrows it.
        unsafe { sys::vaUnmapBuffer(self.buffer.context.display.handle(), self.buffer.id) };
    }
}

mod sealed {
    /// Prevents downstream crates from inventing picture states.
    pub trait Sealed {}
}

/// Marker trait for the [`Picture`] states.
pub trait PictureState: sealed::Sealed {}

macro_rules! picture_states {
    ($($(#[$meta:meta])* $name:ident),* $(,)?) => {
        $(
            $(#[$meta])*
            #[derive(Debug, Clone, Copy, PartialEq, Eq)]
            pub struct $name;
            impl sealed::Sealed for $name {}
            impl PictureState for $name {}
        )*
    };
}

picture_states! {
    /// Nothing submitted yet.
    New,
    /// `vaBeginPicture` done; buffers may be rendered.
    Rendering,
    /// `vaEndPicture` done; the GPU is working on it.
    Ended,
    /// `vaSyncSurface` done; the target surface is readable.
    Synced,
}

/// One picture submission, parameterised by how far through the VA protocol it is.
///
/// See the [module docs](self) for the state diagram and the compile-fail
/// examples.
pub struct Picture<S: PictureState> {
    context: Arc<Context>,
    target: Arc<Surface>,
    /// Kept alive until the picture is dropped: the driver may reference
    /// parameter buffers until the submission completes.
    buffers: Vec<Buffer>,
    _state: PhantomData<S>,
}

impl<S: PictureState> Picture<S> {
    /// The surface this picture renders into.
    pub fn target(&self) -> &Arc<Surface> {
        &self.target
    }

    /// The context this picture is submitted to.
    pub fn context(&self) -> &Arc<Context> {
        &self.context
    }

    fn transition<T: PictureState>(self) -> Picture<T> {
        Picture {
            context: self.context,
            target: self.target,
            buffers: self.buffers,
            _state: PhantomData,
        }
    }
}

impl Picture<New> {
    /// Start a picture targeting `target`. No driver call yet.
    pub fn new(context: &Arc<Context>, target: Arc<Surface>) -> Picture<New> {
        Picture {
            context: Arc::clone(context),
            target,
            buffers: Vec::new(),
            _state: PhantomData,
        }
    }

    /// `vaBeginPicture`.
    ///
    /// On failure the picture is consumed and the context is left with no
    /// picture in flight; the next `begin` starts cleanly.
    pub fn begin(self) -> Result<Picture<Rendering>> {
        // SAFETY: live context and surface ids, both kept alive by the `Arc`s
        // this picture holds.
        let status = unsafe {
            sys::vaBeginPicture(
                self.context.display.handle(),
                self.context.id,
                self.target.id(),
            )
        };
        check("vaBeginPicture", status)?;
        Ok(self.transition())
    }
}

impl Picture<Rendering> {
    /// `vaRenderPicture` with one buffer, which the picture then owns.
    pub fn render(self, buffer: Buffer) -> Result<Picture<Rendering>> {
        self.render_all(vec![buffer])
    }

    /// `vaRenderPicture` with several buffers in one call.
    pub fn render_all(mut self, buffers: Vec<Buffer>) -> Result<Picture<Rendering>> {
        if buffers.is_empty() {
            return Ok(self);
        }
        let mut ids: Vec<sys::VABufferID> = buffers.iter().map(|b| b.id).collect();
        // SAFETY: `ids` is a valid array of `ids.len()` live buffer ids; the
        // buffers themselves are moved into `self.buffers` below, so they
        // outlive the submission. libva does not retain the id array.
        let status = unsafe {
            sys::vaRenderPicture(
                self.context.display.handle(),
                self.context.id,
                ids.as_mut_ptr(),
                ids.len() as i32,
            )
        };
        // Keep the buffers alive regardless of the outcome: on failure the
        // driver may still hold references until the context is torn down.
        self.buffers.extend(buffers);
        check("vaRenderPicture", status)?;
        Ok(self)
    }

    /// `vaEndPicture` — hand the picture to the GPU.
    pub fn end(self) -> Result<Picture<Ended>> {
        // SAFETY: live context id; a picture is in flight because this method
        // exists only on `Picture<Rendering>`, i.e. after a successful begin.
        let status = unsafe { sys::vaEndPicture(self.context.display.handle(), self.context.id) };
        check("vaEndPicture", status)?;
        Ok(self.transition())
    }
}

impl Picture<Ended> {
    /// `vaSyncSurface` — block until the target surface is complete.
    pub fn sync(self) -> Result<Picture<Synced>> {
        self.target.sync()?;
        Ok(self.transition())
    }
}

impl Picture<Synced> {
    /// The completed surface, ready to export or map.
    pub fn into_surface(self) -> Arc<Surface> {
        self.target
    }

    /// The buffers submitted with this picture, e.g. to read a coded buffer.
    pub fn buffers_mut(&mut self) -> &mut [Buffer] {
        &mut self.buffers
    }
}

impl<S: PictureState> std::fmt::Debug for Picture<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Picture")
            .field("state", &std::any::type_name::<S>())
            .field("target", &self.target.id())
            .field("buffers", &self.buffers.len())
            .finish()
    }
}
