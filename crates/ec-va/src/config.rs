//! VA configs: one `(profile, entrypoint)` pair plus driver attributes.

use std::rc::Rc;

use cros_libva::{GenericValue, QuerySurfaceAttributesError, VAConfigAttrib, VASurfaceAttribType};

use crate::caps::{Entrypoint, Profile};
use crate::display::Display;
use crate::error::{Error, Result, va_ok};

/// One `VAConfigAttrib`: a type and its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigAttrib {
    /// The `VAConfigAttribType` this value answers.
    pub type_: u32,
    /// The value, interpreted per attribute type.
    pub value: u32,
}

impl ConfigAttrib {
    /// `VAConfigAttribRTFormat` with the given `VA_RT_FORMAT_*` mask.
    pub fn rt_format(mask: u32) -> Self {
        ConfigAttrib {
            type_: cros_libva::VAConfigAttribType::VAConfigAttribRTFormat,
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

/// A live `VAConfigID`, remembering the profile and entrypoint it was made for.
pub struct Config {
    inner: cros_libva::Config,
    profile: Profile,
    entrypoint: Entrypoint,
}

impl Config {
    /// `vaCreateConfig` for `profile` and `entrypoint` with the given attributes.
    pub fn new(
        display: &Rc<Display>,
        profile: Profile,
        entrypoint: Entrypoint,
        attribs: &[ConfigAttrib],
    ) -> Result<Config> {
        let attrs = attribs
            .iter()
            .map(|a| VAConfigAttrib {
                type_: a.type_,
                value: a.value,
            })
            .collect();
        let inner = va_ok(
            "vaCreateConfig",
            display.create_config(attrs, profile.as_raw(), entrypoint.as_raw()),
        )?;
        Ok(Config {
            inner,
            profile,
            entrypoint,
        })
    }

    /// The profile this config decodes or encodes.
    pub fn profile(&self) -> Profile {
        self.profile
    }

    /// The entrypoint this config was created for.
    pub fn entrypoint(&self) -> Entrypoint {
        self.entrypoint
    }

    /// The cros-libva handle, for the calls that take one.
    pub fn raw(&self) -> &cros_libva::Config {
        &self.inner
    }

    /// Mutable cros-libva handle; surface attribute queries need one.
    pub fn raw_mut(&mut self) -> &mut cros_libva::Config {
        &mut self.inner
    }
}

/// Query the surface constraints of `config` (`vaQuerySurfaceAttributes`).
///
/// A config's surfaces are described by attributes the driver may or may not
/// report; each one that is missing stays `None`.
pub fn caps_of(config: &mut Config) -> Result<SurfaceCaps> {
    let first = |v: Vec<u32>| v.first().copied();
    Ok(SurfaceCaps {
        min_width: first(attribute_ints(
            config,
            VASurfaceAttribType::VASurfaceAttribMinWidth,
        )?),
        max_width: first(attribute_ints(
            config,
            VASurfaceAttribType::VASurfaceAttribMaxWidth,
        )?),
        min_height: first(attribute_ints(
            config,
            VASurfaceAttribType::VASurfaceAttribMinHeight,
        )?),
        max_height: first(attribute_ints(
            config,
            VASurfaceAttribType::VASurfaceAttribMaxHeight,
        )?),
        pixel_formats: attribute_ints(config, VASurfaceAttribType::VASurfaceAttribPixelFormat)?,
        memory_types: attribute_ints(config, VASurfaceAttribType::VASurfaceAttribMemoryType)?
            .into_iter()
            .fold(0, |acc, m| acc | m),
    })
}

fn attribute_ints(config: &mut Config, ty: VASurfaceAttribType::Type) -> Result<Vec<u32>> {
    let values = config
        .raw_mut()
        .query_surface_attributes_by_type(ty)
        .map_err(|e| match e {
            QuerySurfaceAttributesError::VaError(v) => Error::va("vaQuerySurfaceAttributes", &v),
            other => Error::Protocol(other.to_string()),
        })?;
    Ok(values
        .into_iter()
        .filter_map(|v| match v {
            GenericValue::Integer(i) => Some(i as u32),
            _ => None,
        })
        .collect())
}
