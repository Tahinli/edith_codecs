//! A recycling pool of decode/encode surfaces.
//!
//! Surfaces are allocated once, in one `vaCreateSurfaces` call, and handed out
//! as [`PooledSurface`]s that return themselves to the free list on drop.
//! Two things hold a surface at the same time — the reference picture buffer
//! and a frame the caller has not finished with — so ownership is an
//! `Rc<PooledSurface>` and the recycle happens when the last of them goes.
//!
//! Reallocating per frame would be the alternative, and it is precisely what
//! makes a VA-API pipeline stutter: every `vaCreateSurfaces` is a kernel
//! allocation plus a driver-side tiling decision.

use ec_va::{Surface, SurfaceSpec};
use std::ops::Deref;
use std::rc::{Rc, Weak};
use std::sync::Mutex;

struct PoolInner {
    /// Every surface, in allocation order.
    all: Vec<Rc<Surface>>,
    free: Mutex<Vec<Rc<Surface>>>,
}

/// A fixed-size pool of identically specified surfaces.
#[derive(Clone)]
pub struct SurfacePool {
    inner: Rc<PoolInner>,
    spec: SurfaceSpec,
}

impl SurfacePool {
    /// Wrap surfaces that were allocated with `spec`. The caller creates them
    /// with [`ec_va::create_pool`] first, because a decode context needs them as
    /// its render targets before the pool can own them.
    pub fn from_surfaces(surfaces: Vec<Surface>, spec: &SurfaceSpec) -> SurfacePool {
        let all: Vec<Rc<Surface>> = surfaces.into_iter().map(Rc::new).collect();
        SurfacePool {
            inner: Rc::new(PoolInner {
                free: Mutex::new(all.clone()),
                all,
            }),
            spec: *spec,
        }
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
    pub fn acquire(&self) -> Option<Rc<PooledSurface>> {
        // A poisoned lock means another thread panicked while holding it; the
        // free list is a plain Vec, so its contents are still consistent.
        let mut free = match self.inner.free.lock() {
            Ok(free) => free,
            Err(poisoned) => poisoned.into_inner(),
        };
        let surface = free.pop()?;
        Some(Rc::new(PooledSurface {
            surface,
            pool: Rc::downgrade(&self.inner),
        }))
    }
}

/// A surface on loan from a [`SurfacePool`]; returns to it on drop.
pub struct PooledSurface {
    surface: Rc<Surface>,
    pool: Weak<PoolInner>,
}

impl PooledSurface {
    /// The surface itself, for the calls that want an owned `Rc<Surface>`.
    pub fn surface(&self) -> &Rc<Surface> {
        &self.surface
    }
}

impl Deref for PooledSurface {
    type Target = Rc<Surface>;

    fn deref(&self) -> &Rc<Surface> {
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
        free.push(Rc::clone(&self.surface));
    }
}
