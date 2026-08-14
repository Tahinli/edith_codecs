//! Decoded units: pictures and audio blocks.

use crate::error::{Error, Result};
use crate::packet::Buf;
use crate::timebase::Timestamp;

/// Pixel layout of a [`VideoFrame`].
///
/// Covers what the family actually moves: planar YUV 8/10/16-bit (software
/// codecs), the semi-planar pairs hardware hands back (NV12, P010) and packed
/// RGB for the display sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    /// Planar Y/U/V 4:2:0, 8-bit.
    I420,
    /// Planar Y/U/V 4:2:2, 8-bit.
    I422,
    /// Planar Y/U/V 4:4:4, 8-bit.
    I444,
    /// Planar Y/U/V 4:2:0, 10-bit in 16-bit little-endian samples.
    I010,
    /// Planar Y/U/V 4:2:2, 10-bit in 16-bit little-endian samples.
    I210,
    /// Planar Y/U/V 4:4:4, 10-bit in 16-bit little-endian samples.
    I410,
    /// Y plane + interleaved U/V, 4:2:0, 8-bit.
    Nv12,
    /// Y plane + interleaved V/U, 4:2:0, 8-bit.
    Nv21,
    /// Y plane + interleaved U/V, 4:2:0, 10-bit in the high bits of 16.
    P010,
    /// Y plane + interleaved U/V, 4:2:0, 16-bit.
    P016,
    /// Single 8-bit luma plane.
    Gray8,
    /// Single 16-bit luma plane.
    Gray16,
    /// Packed 8-bit R,G,B.
    Rgb8,
    /// Packed 8-bit B,G,R.
    Bgr8,
    /// Packed 8-bit R,G,B,A.
    Rgba8,
    /// Packed 8-bit B,G,R,A — edith's display currency.
    Bgra8,
}

impl PixelFormat {
    /// Number of planes the format stores.
    pub fn plane_count(&self) -> usize {
        use PixelFormat::*;
        match self {
            I420 | I422 | I444 | I010 | I210 | I410 => 3,
            Nv12 | Nv21 | P010 | P016 => 2,
            Gray8 | Gray16 | Rgb8 | Bgr8 | Rgba8 | Bgra8 => 1,
        }
    }

    /// True when chroma lives in its own plane(s) rather than packed with luma.
    pub fn is_planar(&self) -> bool {
        self.plane_count() > 1
    }

    /// True for the YUV families (planar and semi-planar).
    pub fn is_yuv(&self) -> bool {
        use PixelFormat::*;
        matches!(
            self,
            I420 | I422 | I444 | I010 | I210 | I410 | Nv12 | Nv21 | P010 | P016
        )
    }

    /// True when the format carries an alpha channel.
    pub fn has_alpha(&self) -> bool {
        matches!(self, PixelFormat::Rgba8 | PixelFormat::Bgra8)
    }

    /// Significant bits per component (10 for P010 even though it stores 16).
    pub fn bits_per_component(&self) -> u32 {
        use PixelFormat::*;
        match self {
            I010 | I210 | I410 | P010 => 10,
            Gray16 | P016 => 16,
            _ => 8,
        }
    }

    /// Storage bytes per component (2 for every 10- and 16-bit format).
    pub fn bytes_per_component(&self) -> usize {
        if self.bits_per_component() > 8 { 2 } else { 1 }
    }

    /// `(log2 horizontal, log2 vertical)` chroma subsampling; `(0, 0)` for
    /// non-subsampled and non-YUV formats.
    pub fn chroma_shift(&self) -> (u32, u32) {
        use PixelFormat::*;
        match self {
            I420 | I010 | Nv12 | Nv21 | P010 | P016 => (1, 1),
            I422 | I210 => (1, 0),
            _ => (0, 0),
        }
    }

    /// Per plane: `(minimum bytes per row, number of rows)` for `width x height`.
    pub fn plane_geometry(&self, width: u32, height: u32) -> Vec<(usize, usize)> {
        use PixelFormat::*;
        let (w, h) = (width as usize, height as usize);
        let bpc = self.bytes_per_component();
        let (sx, sy) = self.chroma_shift();
        let cw = w.div_ceil(1 << sx);
        let ch = h.div_ceil(1 << sy);
        match self {
            I420 | I422 | I444 | I010 | I210 | I410 => {
                vec![(w * bpc, h), (cw * bpc, ch), (cw * bpc, ch)]
            }
            Nv12 | Nv21 | P010 | P016 => vec![(w * bpc, h), (cw * 2 * bpc, ch)],
            Gray8 | Gray16 => vec![(w * bpc, h)],
            Rgb8 | Bgr8 => vec![(w * 3, h)],
            Rgba8 | Bgra8 => vec![(w * 4, h)],
        }
    }
}

/// One image plane: bytes plus the distance between row starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plane {
    /// Plane bytes; may be a shared slice of a larger buffer.
    pub data: Buf,
    /// Bytes from the start of one row to the start of the next.
    pub stride: usize,
}

impl Plane {
    /// A plane over `data` with row pitch `stride`.
    pub fn new(data: impl Into<Buf>, stride: usize) -> Plane {
        Plane {
            data: data.into(),
            stride,
        }
    }

    /// Row `y` of the plane, `len` bytes wide, or `None` when out of range.
    pub fn row(&self, y: usize, len: usize) -> Option<&[u8]> {
        let start = y.checked_mul(self.stride)?;
        self.data.get(start..start.checked_add(len)?)
    }
}

/// CICP (ITU-T H.273) colour code points, carried verbatim.
///
/// Kept as raw code points here so the IR never loses a value it cannot name;
/// the named tables (BT.709/2020, PQ, HLG, ...) live in the colour module built
/// on top of this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColorInfo {
    /// `colour_primaries` code point (1 = BT.709, 9 = BT.2020, 2 = unspecified).
    pub primaries: u8,
    /// `transfer_characteristics` code point (16 = PQ, 18 = HLG).
    pub transfer: u8,
    /// `matrix_coefficients` code point (1 = BT.709, 9 = BT.2020 NCL).
    pub matrix: u8,
    /// `video_full_range_flag`: false = limited (16-235), true = full.
    pub full_range: bool,
}

impl Default for ColorInfo {
    /// All three code points unspecified (2), limited range.
    fn default() -> Self {
        ColorInfo {
            primaries: 2,
            transfer: 2,
            matrix: 2,
            full_range: false,
        }
    }
}

/// A decoded picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFrame {
    /// Pixel layout of `planes`.
    pub format: PixelFormat,
    /// Visible width in pixels.
    pub width: u32,
    /// Visible height in pixels.
    pub height: u32,
    /// One entry per [`PixelFormat::plane_count`].
    pub planes: Vec<Plane>,
    /// Colour description as signalled by the bitstream or container.
    pub color: ColorInfo,
    /// Presentation time, when the source provided one.
    pub pts: Option<Timestamp>,
}

impl VideoFrame {
    /// A frame over existing planes, validating plane count and plane sizes.
    ///
    /// This is the trust boundary between a decoder and everything that indexes
    /// planes afterwards; a short plane fails here rather than in a converter.
    pub fn try_new(
        format: PixelFormat,
        width: u32,
        height: u32,
        planes: Vec<Plane>,
    ) -> Result<VideoFrame> {
        let geometry = format.plane_geometry(width, height);
        if planes.len() != geometry.len() {
            return Err(Error::corrupt(format!(
                "{format:?} needs {} planes, got {}",
                geometry.len(),
                planes.len()
            )));
        }
        for (i, (plane, (row_bytes, rows))) in planes.iter().zip(geometry).enumerate() {
            if plane.stride < row_bytes {
                return Err(Error::corrupt(format!(
                    "plane {i}: stride {} < {row_bytes} bytes per row",
                    plane.stride
                )));
            }
            // The last row need only hold its visible bytes, not a full stride.
            let needed = rows.saturating_sub(1) * plane.stride + row_bytes;
            if plane.data.len() < needed {
                return Err(Error::corrupt(format!(
                    "plane {i}: {} bytes for {rows} rows of stride {}",
                    plane.data.len(),
                    plane.stride
                )));
            }
        }
        Ok(VideoFrame {
            format,
            width,
            height,
            planes,
            color: ColorInfo::default(),
            pts: None,
        })
    }

    /// A zeroed frame with tightly packed planes.
    pub fn alloc(format: PixelFormat, width: u32, height: u32) -> VideoFrame {
        let planes = format
            .plane_geometry(width, height)
            .into_iter()
            .map(|(row_bytes, rows)| Plane::new(vec![0u8; row_bytes * rows], row_bytes))
            .collect();
        VideoFrame {
            format,
            width,
            height,
            planes,
            color: ColorInfo::default(),
            pts: None,
        }
    }
}

/// Sample storage of an [`AudioFrame`]. 24-bit sources decode into [`SampleFormat::S32`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SampleFormat {
    /// Unsigned 8-bit.
    U8,
    /// Signed 16-bit native endian.
    S16,
    /// Signed 32-bit native endian.
    S32,
    /// 32-bit float, nominal range -1.0..=1.0 — the family's mixing currency.
    F32,
    /// 64-bit float.
    F64,
}

impl SampleFormat {
    /// Bytes one sample of one channel occupies.
    pub fn bytes_per_sample(&self) -> usize {
        match self {
            SampleFormat::U8 => 1,
            SampleFormat::S16 => 2,
            SampleFormat::S32 | SampleFormat::F32 => 4,
            SampleFormat::F64 => 8,
        }
    }

    /// True for the float formats.
    pub fn is_float(&self) -> bool {
        matches!(self, SampleFormat::F32 | SampleFormat::F64)
    }
}

/// A named speaker position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelPosition {
    /// Front left.
    FrontLeft,
    /// Front right.
    FrontRight,
    /// Front centre.
    FrontCenter,
    /// Low frequency effects.
    Lfe,
    /// Back (surround) left.
    BackLeft,
    /// Back (surround) right.
    BackRight,
    /// Side left.
    SideLeft,
    /// Side right.
    SideRight,
    /// Back centre.
    BackCenter,
    /// Front left of centre.
    FrontLeftOfCenter,
    /// Front right of centre.
    FrontRightOfCenter,
}

use ChannelPosition::*;

const MONO: &[ChannelPosition] = &[FrontCenter];
const STEREO: &[ChannelPosition] = &[FrontLeft, FrontRight];
const SURROUND_5_1: &[ChannelPosition] =
    &[FrontLeft, FrontRight, FrontCenter, Lfe, BackLeft, BackRight];
const SURROUND_7_1: &[ChannelPosition] = &[
    FrontLeft,
    FrontRight,
    FrontCenter,
    Lfe,
    BackLeft,
    BackRight,
    SideLeft,
    SideRight,
];

/// Channel count plus the meaning and order of each channel.
///
/// The named layouts fix the interleaving order the family decodes and encodes
/// in; 5.1 is FL, FR, FC, LFE, BL, BR.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ChannelLayout {
    /// One channel, front centre.
    Mono,
    /// FL, FR.
    Stereo,
    /// FL, FR, FC, LFE, BL, BR.
    Surround5_1,
    /// FL, FR, FC, LFE, BL, BR, SL, SR.
    Surround7_1,
    /// Any other ordering a container states explicitly.
    Custom(Vec<ChannelPosition>),
}

impl ChannelLayout {
    /// Channel order, as interleaved and as planes are indexed.
    pub fn positions(&self) -> &[ChannelPosition] {
        match self {
            ChannelLayout::Mono => MONO,
            ChannelLayout::Stereo => STEREO,
            ChannelLayout::Surround5_1 => SURROUND_5_1,
            ChannelLayout::Surround7_1 => SURROUND_7_1,
            ChannelLayout::Custom(v) => v,
        }
    }

    /// Number of channels.
    pub fn channel_count(&self) -> usize {
        self.positions().len()
    }

    /// The default layout for a bare channel count, as containers that state
    /// only a count intend it.
    pub fn from_count(n: usize) -> ChannelLayout {
        match n {
            1 => ChannelLayout::Mono,
            2 => ChannelLayout::Stereo,
            6 => ChannelLayout::Surround5_1,
            8 => ChannelLayout::Surround7_1,
            _ => ChannelLayout::Custom(vec![FrontCenter; n]),
        }
    }
}

/// A decoded block of audio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioFrame {
    /// Sample storage format.
    pub format: SampleFormat,
    /// True when `data` holds one plane per channel, false when interleaved.
    pub planar: bool,
    /// Channel count, order and meaning.
    pub layout: ChannelLayout,
    /// Sample rate in Hz.
    pub rate: u32,
    /// Samples per channel in this frame.
    pub samples: usize,
    /// One plane when interleaved, one per channel when planar.
    pub data: Vec<Buf>,
    /// Presentation time, when the source provided one.
    pub pts: Option<Timestamp>,
}

impl AudioFrame {
    /// An audio frame over existing planes, validating plane count and length.
    pub fn try_new(
        format: SampleFormat,
        planar: bool,
        layout: ChannelLayout,
        rate: u32,
        samples: usize,
        data: Vec<Buf>,
    ) -> Result<AudioFrame> {
        let channels = layout.channel_count();
        let wanted_planes = if planar { channels } else { 1 };
        if data.len() != wanted_planes {
            return Err(Error::corrupt(format!(
                "{layout:?} {} needs {wanted_planes} planes, got {}",
                if planar { "planar" } else { "interleaved" },
                data.len()
            )));
        }
        let per_plane = if planar { 1 } else { channels };
        let needed = samples * per_plane * format.bytes_per_sample();
        for (i, plane) in data.iter().enumerate() {
            if plane.len() < needed {
                return Err(Error::corrupt(format!(
                    "audio plane {i}: {} bytes, {needed} needed for {samples} samples",
                    plane.len()
                )));
            }
        }
        Ok(AudioFrame {
            format,
            planar,
            layout,
            rate,
            samples,
            data,
            pts: None,
        })
    }

    /// Channel count of the layout.
    pub fn channels(&self) -> usize {
        self.layout.channel_count()
    }
}

/// What a decoder produces.
///
/// Subtitles are deliberately absent: a decoded subtitle is a cue list or a
/// bitmap region set, not a sample grid, so subtitle streams travel as
/// [`crate::packet::Packet`]s and are parsed by the subtitle crate into its own
/// IR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// A decoded picture.
    Video(VideoFrame),
    /// A decoded audio block.
    Audio(AudioFrame),
}

impl Frame {
    /// Presentation time of either variant.
    pub fn pts(&self) -> Option<Timestamp> {
        match self {
            Frame::Video(v) => v.pts,
            Frame::Audio(a) => a.pts,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timebase::TimeBase;

    #[test]
    fn plane_geometry_matches_formats() {
        // Odd dimensions round chroma up, never down.
        assert_eq!(
            PixelFormat::I420.plane_geometry(1921, 1081),
            vec![(1921, 1081), (961, 541), (961, 541)]
        );
        assert_eq!(
            PixelFormat::Nv12.plane_geometry(1920, 1080),
            vec![(1920, 1080), (1920, 540)]
        );
        // P010: 2 bytes per component, interleaved chroma pair.
        assert_eq!(
            PixelFormat::P010.plane_geometry(1920, 1080),
            vec![(3840, 1080), (3840, 540)]
        );
        assert_eq!(PixelFormat::Bgra8.plane_geometry(64, 4), vec![(256, 4)]);
        assert_eq!(PixelFormat::I444.plane_geometry(16, 16).len(), 3);
        assert_eq!(PixelFormat::P010.bits_per_component(), 10);
        assert_eq!(PixelFormat::P010.bytes_per_component(), 2);
        assert!(PixelFormat::Bgra8.has_alpha());
        assert!(!PixelFormat::Nv12.has_alpha());
        assert!(PixelFormat::Nv12.is_yuv());
    }

    #[test]
    fn video_frame_validates_planes() {
        let f = VideoFrame::alloc(PixelFormat::I420, 64, 32);
        assert_eq!(f.planes.len(), 3);
        assert_eq!(f.planes[0].stride, 64);
        assert_eq!(f.planes[1].data.len(), 32 * 16);
        assert_eq!(f.planes[0].row(31, 64).unwrap().len(), 64);
        assert!(f.planes[0].row(32, 64).is_none());

        // Padded strides are legal; short planes and short strides are not.
        let padded = VideoFrame::try_new(
            PixelFormat::Gray8,
            4,
            2,
            vec![Plane::new(vec![0u8; 8 + 4], 8)],
        );
        assert!(padded.is_ok());
        assert!(
            VideoFrame::try_new(PixelFormat::Gray8, 4, 2, vec![Plane::new(vec![0u8; 8], 2)])
                .is_err()
        );
        assert!(
            VideoFrame::try_new(PixelFormat::I420, 4, 2, vec![Plane::new(vec![0u8; 8], 4)])
                .is_err()
        );
    }

    #[test]
    fn channel_layouts_are_named_and_ordered() {
        assert_eq!(
            ChannelLayout::Surround5_1.positions(),
            &[
                ChannelPosition::FrontLeft,
                ChannelPosition::FrontRight,
                ChannelPosition::FrontCenter,
                ChannelPosition::Lfe,
                ChannelPosition::BackLeft,
                ChannelPosition::BackRight
            ]
        );
        assert_eq!(ChannelLayout::Surround7_1.channel_count(), 8);
        assert_eq!(ChannelLayout::from_count(6), ChannelLayout::Surround5_1);
        assert_eq!(ChannelLayout::from_count(3).channel_count(), 3);
    }

    #[test]
    fn audio_frame_validates_planes() {
        let interleaved = AudioFrame::try_new(
            SampleFormat::F32,
            false,
            ChannelLayout::Surround5_1,
            48_000,
            1024,
            vec![Buf::from_vec(vec![0u8; 1024 * 6 * 4])],
        )
        .unwrap();
        assert_eq!(interleaved.channels(), 6);

        let planar = AudioFrame::try_new(
            SampleFormat::S16,
            true,
            ChannelLayout::Stereo,
            44_100,
            512,
            vec![Buf::from_vec(vec![0u8; 1024]); 2],
        )
        .unwrap();
        assert_eq!(planar.data.len(), 2);

        // Wrong plane count and short planes are rejected, not truncated.
        assert!(
            AudioFrame::try_new(
                SampleFormat::F32,
                true,
                ChannelLayout::Stereo,
                48_000,
                64,
                vec![Buf::from_vec(vec![0u8; 256])]
            )
            .is_err()
        );
        assert!(
            AudioFrame::try_new(
                SampleFormat::F32,
                false,
                ChannelLayout::Stereo,
                48_000,
                64,
                vec![Buf::from_vec(vec![0u8; 4])]
            )
            .is_err()
        );
    }

    #[test]
    fn frame_pts_passthrough() {
        let mut v = VideoFrame::alloc(PixelFormat::Nv12, 16, 16);
        v.pts = Some(Timestamp::new(1001, TimeBase::new(1, 24_000)));
        let f = Frame::Video(v);
        assert_eq!(f.pts().unwrap().ticks, 1001);
    }
}
