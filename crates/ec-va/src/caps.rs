//! Capability probing: what this GPU can actually decode and encode.
//!
//! [`Profile`] and [`Entrypoint`] are safe mirrors of the C enums, each with an
//! `Unknown(i32)` arm: a newer libva can name a profile this build has never
//! heard of, and that must survive as data rather than become an invalid enum.

use std::rc::Rc;

use cros_libva::{VAConfigAttrib, VAConfigAttribType};

use crate::config::{Config, SurfaceCaps, caps_of};
use crate::display::Display;
use crate::error::{Result, va_ok};

/// Generates a safe enum mirroring a C enum, plus its raw conversions.
///
/// One table, three derived functions — so the transcription can be diffed
/// against the header in one place.
macro_rules! va_enum {
    (
        $(#[$meta:meta])*
        $name:ident : $raw:ty {
            $( $variant:ident = $value:expr , )*
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[non_exhaustive]
        pub enum $name {
            $( #[allow(missing_docs)] $variant, )*
            /// A value this build does not know. Carried, never guessed at.
            Unknown($raw),
        }

        impl $name {
            /// Interpret a raw C value. Never panics, never UB.
            pub fn from_raw(raw: $raw) -> Self {
                match raw {
                    $( $value => $name::$variant, )*
                    other => $name::Unknown(other),
                }
            }

            /// The raw C value.
            pub fn as_raw(self) -> $raw {
                match self {
                    $( $name::$variant => $value, )*
                    $name::Unknown(other) => other,
                }
            }

            /// The libva identifier, e.g. `"VAProfileH264High"`.
            pub fn name(self) -> &'static str {
                match self {
                    $( $name::$variant => concat!(stringify!($name), "::", stringify!($variant)), )*
                    $name::Unknown(_) => concat!(stringify!($name), "::Unknown"),
                }
            }
        }
    };
}

va_enum! {
    /// `VAProfile`, transcribed from `va.h:502-547` (libva 1.23).
    Profile: i32 {
        None = -1,
        MPEG2Simple = 0,
        MPEG2Main = 1,
        MPEG4Simple = 2,
        MPEG4AdvancedSimple = 3,
        MPEG4Main = 4,
        H264Baseline = 5,
        H264Main = 6,
        H264High = 7,
        VC1Simple = 8,
        VC1Main = 9,
        VC1Advanced = 10,
        H263Baseline = 11,
        JPEGBaseline = 12,
        H264ConstrainedBaseline = 13,
        VP8Version0_3 = 14,
        H264MultiviewHigh = 15,
        H264StereoHigh = 16,
        HEVCMain = 17,
        HEVCMain10 = 18,
        VP9Profile0 = 19,
        VP9Profile1 = 20,
        VP9Profile2 = 21,
        VP9Profile3 = 22,
        HEVCMain12 = 23,
        HEVCMain422_10 = 24,
        HEVCMain422_12 = 25,
        HEVCMain444 = 26,
        HEVCMain444_10 = 27,
        HEVCMain444_12 = 28,
        HEVCSccMain = 29,
        HEVCSccMain10 = 30,
        HEVCSccMain444 = 31,
        AV1Profile0 = 32,
        AV1Profile1 = 33,
        HEVCSccMain444_10 = 34,
        Protected = 35,
        H264High10 = 36,
        VVCMain10 = 37,
        VVCMultilayerMain10 = 38,
        AV1Profile2 = 39,
        H264High422 = 40,
    }
}

va_enum! {
    /// `VAEntrypoint`, transcribed from `va.h:552-616` (libva 1.23).
    Entrypoint: u32 {
        VLD = 1,
        IZZ = 2,
        IDCT = 3,
        MoComp = 4,
        Deblocking = 5,
        EncSlice = 6,
        EncPicture = 7,
        EncSliceLP = 8,
        VideoProc = 10,
        FEI = 11,
        Stats = 12,
        ProtectedTEEComm = 13,
        ProtectedContent = 14,
    }
}

impl Profile {
    /// True for the profiles the family decodes or encodes in hardware.
    pub fn is_family_codec(self) -> bool {
        matches!(
            self,
            Profile::H264ConstrainedBaseline
                | Profile::H264Main
                | Profile::H264High
                | Profile::H264High10
                | Profile::HEVCMain
                | Profile::HEVCMain10
                | Profile::VP9Profile0
                | Profile::VP9Profile2
                | Profile::AV1Profile0
                | Profile::AV1Profile2
        )
    }
}

/// What one (profile, entrypoint) pair supports.
/// One supported (profile, entrypoint) pair and what the driver says about it.
#[derive(Debug, Clone)]
pub struct CapEntry {
    /// The profile.
    pub profile: Profile,
    /// The entrypoint.
    pub entrypoint: Entrypoint,
    /// `VAConfigAttribRTFormat`: the `VA_RT_FORMAT_*` mask.
    pub rt_formats: u32,
    /// `VAConfigAttribEncPackedHeaders`.
    pub packed_headers: u32,
    /// `VAConfigAttribRateControl`.
    pub rate_control: u32,
    /// Surface constraints, when the config could be created to ask.
    pub surfaces: Option<SurfaceCaps>,
}

impl CapEntry {
    /// True if `rt_format` (a `VA_RT_FORMAT_*` bit) is in the mask.
    pub fn supports_rt_format(&self, rt_format: u32) -> bool {
        self.rt_formats & rt_format != 0
    }
}

/// Everything the driver told us about itself.
#[derive(Debug, Clone)]
pub struct CapReport {
    /// `vaQueryVendorString`.
    pub vendor: String,
    /// One entry per supported (profile, entrypoint) pair, in driver order.
    pub entries: Vec<CapEntry>,
    /// FourCCs from `vaQueryImageFormats` — the formats `vaGetImage` can produce.
    pub image_formats: Vec<u32>,
}

impl CapReport {
    /// Probe a display. One `vaCreateConfig`/`vaDestroyConfig` round trip per
    /// supported pair; ~15 pairs on a typical GPU, so this is cheap enough to
    /// do once at startup and cache.
    pub fn probe(display: &Rc<Display>) -> Result<CapReport> {
        let entries = probe_entries(display)?;
        Ok(CapReport {
            vendor: crate::display::vendor(display)?,
            entries,
            image_formats: query_image_formats(display)?,
        })
    }

    /// The entry for a pair, if the driver supports it.
    pub fn entry(&self, profile: Profile, entrypoint: Entrypoint) -> Option<&CapEntry> {
        self.entries
            .iter()
            .find(|e| e.profile == profile && e.entrypoint == entrypoint)
    }

    /// True if this GPU supports the pair at all.
    pub fn supports(&self, profile: Profile, entrypoint: Entrypoint) -> bool {
        self.entry(profile, entrypoint).is_some()
    }

    /// Profiles supporting `entrypoint`, deduplicated, in driver order.
    pub fn profiles_for(&self, entrypoint: Entrypoint) -> Vec<Profile> {
        let mut out = Vec::new();
        for e in self.entries.iter().filter(|e| e.entrypoint == entrypoint) {
            if !out.contains(&e.profile) {
                out.push(e.profile);
            }
        }
        out
    }
}

fn probe_entries(display: &Rc<Display>) -> Result<Vec<CapEntry>> {
    let mut entries = Vec::new();
    for profile in query_profiles(display)? {
        for entrypoint in query_entrypoints(display, profile)? {
            let [rt_formats, packed_headers, rate_control] =
                query_attribs(display, profile, entrypoint)?;
            // Creating the config is also how the legal surface sizes are
            // discovered: `vaQuerySurfaceAttributes` needs a config id. A
            // driver may refuse a pair it just advertised (radeonsi does this
            // for some encode entrypoints), which is data, not an error, so it
            // lands as `surfaces: None` instead of failing the whole probe.
            let surfaces = match Config::new(display, profile, entrypoint, &[]) {
                Ok(mut config) => caps_of(&mut config).ok(),
                Err(_) => None,
            };
            entries.push(CapEntry {
                profile,
                entrypoint,
                rt_formats,
                packed_headers,
                rate_control,
                surfaces,
            });
        }
    }
    Ok(entries)
}

fn query_profiles(display: &Rc<Display>) -> Result<Vec<Profile>> {
    let raw = va_ok("vaQueryConfigProfiles", display.query_config_profiles())?;
    Ok(raw.into_iter().map(Profile::from_raw).collect())
}

fn query_entrypoints(display: &Rc<Display>, profile: Profile) -> Result<Vec<Entrypoint>> {
    let raw = va_ok(
        "vaQueryConfigEntrypoints",
        display.query_config_entrypoints(profile.as_raw()),
    )?;
    Ok(raw.into_iter().map(Entrypoint::from_raw).collect())
}

fn query_attribs(
    display: &Rc<Display>,
    profile: Profile,
    entrypoint: Entrypoint,
) -> Result<[u32; 3]> {
    let mut attrs = [
        VAConfigAttrib {
            type_: VAConfigAttribType::VAConfigAttribRTFormat,
            value: 0,
        },
        VAConfigAttrib {
            type_: VAConfigAttribType::VAConfigAttribEncPackedHeaders,
            value: 0,
        },
        VAConfigAttrib {
            type_: VAConfigAttribType::VAConfigAttribRateControl,
            value: 0,
        },
    ];
    va_ok(
        "vaGetConfigAttributes",
        display.get_config_attributes(profile.as_raw(), entrypoint.as_raw(), &mut attrs),
    )?;
    // VA_ATTRIB_NOT_SUPPORTED is 0xffffffff; a decode entrypoint answers that
    // for the encode-only attributes, and "all bits set" is not a capability.
    let value = |a: &VAConfigAttrib| if a.value == u32::MAX { 0 } else { a.value };
    Ok([value(&attrs[0]), value(&attrs[1]), value(&attrs[2])])
}

fn query_image_formats(display: &Rc<Display>) -> Result<Vec<u32>> {
    let formats = va_ok("vaQueryImageFormats", display.query_image_formats())?;
    Ok(formats.into_iter().map(|f| f.fourcc).collect())
}

/// Render a FourCC as its four characters, `?` for non-printable bytes.
pub fn fourcc_str(fourcc: u32) -> String {
    fourcc
        .to_le_bytes()
        .iter()
        .map(|&b| if b.is_ascii_graphic() { b as char } else { '?' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_roundtrip_and_unknown_arm() {
        for raw in -1..=40 {
            let p = Profile::from_raw(raw);
            assert_eq!(p.as_raw(), raw, "roundtrip failed for {raw}");
            assert_ne!(p, Profile::Unknown(raw), "{raw} should be a known profile");
        }
        // A profile from a future libva must survive, not become UB or a panic.
        assert_eq!(Profile::from_raw(9_999), Profile::Unknown(9_999));
        assert_eq!(Profile::from_raw(9_999).as_raw(), 9_999);
        assert_eq!(Profile::Unknown(9_999).name(), "Profile::Unknown");
        assert_eq!(Profile::H264High.name(), "Profile::H264High");
    }

    #[test]
    fn entrypoint_roundtrip_and_unknown_arm() {
        for raw in [1, 2, 3, 4, 5, 6, 7, 8, 10, 11, 12, 13, 14] {
            assert_eq!(Entrypoint::from_raw(raw).as_raw(), raw);
        }
        // 9 is unassigned in libva 1.23 and 15 is beyond the table.
        assert_eq!(Entrypoint::from_raw(9), Entrypoint::Unknown(9));
        assert_eq!(Entrypoint::from_raw(15), Entrypoint::Unknown(15));
    }

    #[test]
    fn fourcc_rendering() {
        assert_eq!(fourcc_str(cros_libva::VA_FOURCC_NV12), "NV12");
        assert_eq!(fourcc_str(cros_libva::VA_FOURCC_P010), "P010");
        assert_eq!(fourcc_str(0), "????");
    }
}
