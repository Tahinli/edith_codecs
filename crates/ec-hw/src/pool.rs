//! A recycling pool of decode/encode surfaces.
//!
//! Surfaces are allocated once, in one `vaCreateSurfaces` call, and handed out
//! as [`PooledSurface`]s that return themselves to the free list on drop.
//! Two things hold a surface at the same time — the reference picture buffer
//! and a frame the caller has not finished with — so ownership is an
//! `Arc<PooledSurface>` and the recycle happens when the last of them goes.
//!
//! Reallocating per frame would be the alternative, and it is precisely what
//! makes a VA-API pipeline stutter: every `vaCreateSurfaces` is a kernel
//! allocation plus a driver-side tiling decision.

use std::ops::Deref;
use std::sync::{Arc, Mutex, Weak};

use ec_va::{Display, Surface, SurfaceSpec};

use crate::error::Result;

struct PoolInner {
    /// Every surface, in allocation order. Also the context's render targets.
    all: Vec<Arc<Surface>>,
    free: Mutex<Vec<Arc<Surface>>>,
}

/// A fixed-size pool of identically specified surfaces.
#[derive(Clone)]
pub struct SurfacePool {
    inner: Arc<PoolInner>,
    spec: SurfaceSpec,
}

impl SurfacePool {
    /// Allocate `count` surfaces matching `spec`.
    pub fn new(display: &Arc<Display>, spec: &SurfaceSpec, count: usize) -> Result<SurfacePool> {
        let all = Surface::create_pool(display, spec, count)?;
        Ok(SurfacePool {
            inner: Arc::new(PoolInner {
                free: Mutex::new(all.clone()),
                all,
            }),
            spec: *spec,
        })
    }

    /// Every surface in the pool — what a `Context` wants as its render targets.
    pub fn targets(&self) -> &[Arc<Surface>] {
        &self.inner.all
    }

    /// The spec every surface in this pool was allocated with.
    pub fn spec(&self) -> &SurfaceSpec {
        &self.spec
    }

    /// Total surfaces in the pool.
    pub fn len(&self) -> usize {
        self.inner.all.len()
    }

    /// True when the pool holds no surfaces at all.
    pub fn is_empty(&self) -> bool {
        self.inner.all.is_empty()
    }

    /// Surfaces currently available.
    pub fn available(&self) -> usize {
        self.inner.free.lock().map(|f| f.len()).unwrap_or(0)
    }

    /// Take a free surface, or `None` when every one is still in use.
    ///
    /// A `None` is the caller's cue to release output frames, not an error: a
    /// decoder that has handed out its whole pool is waiting on the consumer.
    pub fn acquire(&self) -> Option<Arc<PooledSurface>> {
        // A poisoned lock means another thread panicked while holding it; the
        // free list is a plain Vec, so its contents are still consistent and
        // there is nothing to recover from.
        let mut free = match self.inner.free.lock() {
            Ok(free) => free,
            Err(poisoned) => poisoned.into_inner(),
        };
        let surface = free.pop()?;
        Some(Arc::new(PooledSurface {
            surface,
            pool: Arc::downgrade(&self.inner),
        }))
    }
}

/// A surface on loan from a [`SurfacePool`]; returns to it on drop.
pub struct PooledSurface {
    surface: Arc<Surface>,
    pool: Weak<PoolInner>,
}

impl PooledSurface {
    /// The surface itself, for the calls that want an owned `Arc<Surface>`.
    pub fn surface(&self) -> &Arc<Surface> {
        &self.surface
    }
}

impl Deref for PooledSurface {
    type Target = Arc<Surface>;

    fn deref(&self) -> &Arc<Surface> {
        &self.surface
    }
}

impl std::fmt::Debug for PooledSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PooledSurface")
            .field("id", &self.surface.id())
            .finish()
    }
}

impl Drop for PooledSurface {
    fn drop(&mut self) {
        let Some(pool) = self.pool.upgrade() else {
            // The pool is gone, so the surface simply drops with this handle.
            return;
        };
        let mut free = match pool.free.lock() {
            Ok(free) => free,
            Err(poisoned) => poisoned.into_inner(),
        };
        free.push(Arc::clone(&self.surface));
    }
}
