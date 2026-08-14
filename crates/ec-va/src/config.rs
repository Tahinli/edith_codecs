//! [`Config`] — a `VAConfigID` for one (profile, entrypoint) pair, plus the
//! surface geometry the driver will accept for it.

use std::sync::Arc;

use crate::caps::{Entrypoint, Profile};
use crate::display::Display;
use crate::error::{Error, Result, check};
use crate::sys;

/// A configuration attribute, e.g. `RTFormat = VA_RT_FORMAT_YUV420`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigAttrib {
    /// `VAConfigAttrib*` type constant from [`crate::sys`].
    pub type_: sys::VAConfigAttribType,
    /// Attribute value; meaning depends on `type_`.
    pub value: u32,
}

impl ConfigAttrib {
    /// `VAConfigAttribRTFormat` with the given `VA_RT_FORMAT_*` mask.
    pub fn rt_format(mask: u32) -> Self {
        ConfigAttrib {
            type_: sys::VAConfigAttribRTFormat,
            value: mask,
        }
    }
}

/// Surface constraints reported for a config.
///
/// The minimum sizes are the reason this type exists: radeonsi rejects
/// surfaces below 64x64, and probing with a hardcoded 16x16 is how the
/// incumbent stack used to panic on this GPU.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SurfaceCaps {
    /// Smallest legal surface width, if reported.
    pub min_width: Option<u32>,
    /// Largest legal surface width, if reported.
    pub max_width: Option<u32>,
    /// Smallest legal surface height, if reported.
    pub min_height: Option<u32>,
    /// Largest legal surface height, if reported.
    pub max_height: Option<u32>,
    /// Pixel formats (FourCCs) the driver will allocate for this config.
    pub pixel_formats: Vec<u32>,
    /// `VASurfaceAttribMemoryType` bitmask, e.g. DRM PRIME 2 support.
    pub memory_types: u32,
}

impl SurfaceCaps {
    /// True if `width` x `height` is within the reported bounds. Unreported
    /// bounds are treated as "no constraint".
    pub fn allows(&self, width: u32, height: u32) -> bool {
        self.min_width.is_none_or(|v| width >= v)
            && self.max_width.is_none_or(|v| width <= v)
            && self.min_height.is_none_or(|v| height >= v)
            && self.max_height.is_none_or(|v| height <= v)
    }

    /// True if the driver will allocate this FourCC for the config.
    pub fn supports_fourcc(&self, fourcc: u32) -> bool {
        self.pixel_formats.contains(&fourcc)
    }
}

/// A live `VAConfigID`.
///
/// Held by `Arc` because a [`crate::Context`] must not outlive its config.
pub struct Config {
    display: Arc<Display>,
    id: sys::VAConfigID,
    profile: Profile,
    entrypoint: Entrypoint,
}

impl Config {
    /// Create a config for `(profile, entrypoint)` with the given attributes.
    ///
    /// Pass `&[]` for the driver defaults — enough for capability probing;
    /// decode contexts want at least an explicit `RTFormat` so that a 10-bit
    /// stream gets a 10-bit config instead of silently getting an 8-bit one.
    pub fn new(
        display: &Arc<Display>,
        profile: Profile,
        entrypoint: Entrypoint,
        attribs: &[ConfigAttrib],
    ) -> Result<Arc<Config>> {
        let mut raw: Vec<sys::VAConfigAttrib> = attribs
            .iter()
            .map(|a| sys::VAConfigAttrib {
                type_: a.type_,
                value: a.value,
            })
            .collect();
        let mut id: sys::VAConfigID = sys::VA_INVALID_ID;
        // SAFETY: `raw` is a valid array of `raw.len()` VAConfigAttrib (a null
        // pointer with len 0 is also accepted by libva, and `as_mut_ptr` on an
        // empty Vec is a dangling-but-aligned pointer, which libva never
        // dereferences because the count is 0). `id` is a valid out-parameter.
        let status = unsafe {
            sys::vaCreateConfig(
                display.handle(),
                profile.as_raw(),
                entrypoint.as_raw(),
                raw.as_mut_ptr(),
                raw.len() as i32,
                &mut id,
            )
        };
        check("vaCreateConfig", status)?;
        if id == sys::VA_INVALID_ID {
            return Err(Error::Protocol(
                "vaCreateConfig succeeded but returned VA_INVALID_ID".to_string(),
            ));
        }
        Ok(Arc::new(Config {
            display: Arc::clone(display),
            id,
            profile,
            entrypoint,
        }))
    }

    /// Surface constraints for this config (`vaQuerySurfaceAttributes`).
    pub fn surface_caps(&self) -> Result<SurfaceCaps> {
        // Two-pass query: NULL first to learn the count, as documented at
        // va.h:1855 ("it is perfectly valid to pass NULL to attrib_list").
        let mut count: u32 = 0;
        // SAFETY: valid display and config id; NULL attrib_list with a valid
        // count out-parameter is the documented "how many?" form.
        let status = unsafe {
            sys::vaQuerySurfaceAttributes(
                self.display.handle(),
                self.id,
                std::ptr::null_mut(),
                &mut count,
            )
        };
        check("vaQuerySurfaceAttributes", status)?;

        let mut attrs = vec![
            sys::VASurfaceAttrib {
                type_: sys::VASurfaceAttribNone,
                flags: 0,
                value: sys::VAGenericValue {
                    type_: sys::VAGenericValueTypeInteger,
                    value: sys::VAGenericValueUnion { i: 0 },
                },
            };
            count as usize
        ];
        // SAFETY: `attrs` holds exactly `count` initialized elements, and
        // `count` is what the driver just asked for. It is passed by pointer
        // together with its length; libva rewrites `count` to the number
        // actually written, which cannot exceed the input.
        let status = unsafe {
            sys::vaQuerySurfaceAttributes(
                self.display.handle(),
                self.id,
                attrs.as_mut_ptr(),
                &mut count,
            )
        };
        check("vaQuerySurfaceAttributes", status)?;
        attrs.truncate(count as usize);

        let mut caps = SurfaceCaps::default();
        for attr in &attrs {
            if attr.flags & sys::VA_SURFACE_ATTRIB_GETTABLE == 0 {
                continue;
            }
            // Every attribute read below is documented as an integer
            // (va.h:1691-1730). A driver contradicting that is a protocol
            // error, not something to reinterpret: reading the wrong union arm
            // is precisely the class of bug this crate exists to avoid.
            if attr.value.type_ != sys::VAGenericValueTypeInteger {
                continue;
            }
            // SAFETY: the discriminant was just checked to be Integer, so the
            // `i` arm of the union is the initialized one.
            let value = unsafe { attr.value.value.i };
            match attr.type_ {
                sys::VASurfaceAttribPixelFormat => caps.pixel_formats.push(value as u32),
                sys::VASurfaceAttribMinWidth => caps.min_width = Some(value.max(0) as u32),
                sys::VASurfaceAttribMaxWidth => caps.max_width = Some(value.max(0) as u32),
                sys::VASurfaceAttribMinHeight => caps.min_height = Some(value.max(0) as u32),
                sys::VASurfaceAttribMaxHeight => caps.max_height = Some(value.max(0) as u32),
                sys::VASurfaceAttribMemoryType => caps.memory_types = value as u32,
                _ => {}
            }
        }
        Ok(caps)
    }

    /// The profile this config was created for.
    pub fn profile(&self) -> Profile {
        self.profile
    }

    /// The entrypoint this config was created for.
    pub fn entrypoint(&self) -> Entrypoint {
        self.entrypoint
    }

    /// The display this config belongs to.
    pub fn display(&self) -> &Arc<Display> {
        &self.display
    }

    pub(crate) fn id(&self) -> sys::VAConfigID {
        self.id
    }
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("id", &self.id)
            .field("profile", &self.profile)
            .field("entrypoint", &self.entrypoint)
            .finish()
    }
}

impl Drop for Config {
    fn drop(&mut self) {
        // SAFETY: `self.id` was returned by vaCreateConfig on `self.display`
        // and has not been destroyed. Contexts hold an `Arc<Config>`, so no
        // context can still reference it here.
        unsafe { sys::vaDestroyConfig(self.display.handle(), self.id) };
    }
}
