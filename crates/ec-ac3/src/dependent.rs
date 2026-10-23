//! E-AC-3 dependent substreams (Annex E, E.3.8): the channels a dependent
//! substream adds to the frame its independent substream 0 starts.
//!
//! A dependent substream is an Annex E syncframe in its own right, with its
//! own `audfrm()` and `audblk()`, so it is decoded by the same pipeline as an
//! independent one, with a [`Core`](crate::decoder) of its own. What makes it a
//! dependent is the custom channel map in its `bsi()`, which names the
//! loudspeaker positions its coded channels fill.
//!
//! Placement follows ffmpeg (`ff_eac3_custom_channel_map_locations` and the
//! custom-map code in `eac3dec.c`), which is the oracle the tests use:
//!
//! - The map is 16 bits, bit 15 (`0x8000`) first. Table E2.5, as a `u16`:
//!   `0x8000` L, `0x4000` C, `0x2000` R, `0x1000` Ls, `0x0800` Rs,
//!   `0x0400` Lc/Rc pair, `0x0200` Lrs/Rrs pair, `0x0100` Cs, `0x0080` Ts,
//!   `0x0040` Lsd/Rsd pair, `0x0020` Lw/Rw pair, `0x0010` Vhl/Vhr pair,
//!   `0x0008` Vhc, `0x0004` Lts/Rts pair, `0x0002` LFE2, `0x0001` LFE.
//! - A position the independent substream already has replaces that channel.
//!   Any other position is added.
//! - The frame's channels are output in the layout's canonical order, which is
//!   the bit order of the channel layout mask (see [`rank`]).
//!
//! Refused by name, never dropped in silence: a dependent substream without a
//! custom channel map, or whose map names a position this build has no
//! [`ChannelPosition`] for (Ts, Lsd/Rsd, Lw/Rw, Vhl/Vhr, Vhc, Lts/Rts, LFE2),
//! is [`Error::Unsupported`]. A map whose channel count disagrees with the
//! substream is [`Error::Corrupt`].

use ec_core::{BitReader, ChannelPosition, Error, Result};

use crate::eac3::{self, Eac3Bsi, StreamType};
use crate::syncinfo::SYNCWORD;

/// One channel of a frame: where it plays, and one sample per sample frame.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Channel {
    pub(crate) position: ChannelPosition,
    pub(crate) samples: Vec<f32>,
}

/// The header of the dependent substream that starts `data`, if one does.
///
/// Anything else (an independent syncframe, junk, a header that does not
/// parse) is `None`, so the caller decides what a tail of bytes means.
pub(crate) fn peek(data: &[u8]) -> Option<Eac3Bsi> {
    // Six bytes are needed for the sync word and bsid; AC-3 (bsid <= 10) has
    // no dependent substreams.
    if data.len() < 6 || u16::from_be_bytes([data[0], data[1]]) != SYNCWORD || data[5] >> 3 <= 10 {
        return None;
    }
    let hdr = eac3::parse_from(&mut BitReader::new(data)).ok()?;
    (hdr.strmtyp == StreamType::Dependent).then_some(hdr)
}

/// The loudspeaker positions a dependent substream's `channels` coded channels
/// fill, in coded order, from its custom channel map (Table E2.5).
pub(crate) fn positions(map: Option<u16>, channels: usize) -> Result<Vec<ChannelPosition>> {
    use ChannelPosition::*;
    let Some(map) = map else {
        return Err(Error::unsupported(
            "E-AC-3 dependent substream without a channel map",
            "its channels have to be placed by the custom channel map (Table E2.5); \
             without one this build cannot tell which loudspeakers they feed",
        ));
    };
    // Bit `i` counted from the top (0x8000 is i = 0) names entry `i` of the
    // table; a pair is two channels, in ascending canonical order.
    let mut out = Vec::new();
    for i in 0..16 {
        if map & (0x8000 >> i) == 0 {
            continue;
        }
        let names: &[ChannelPosition] = match i {
            0 => &[FrontLeft],
            1 => &[FrontCenter],
            2 => &[FrontRight],
            3 => &[SideLeft],
            4 => &[SideRight],
            5 => &[FrontLeftOfCenter, FrontRightOfCenter],
            6 => &[BackLeft, BackRight],
            7 => &[BackCenter],
            15 => &[Lfe],
            _ => {
                return Err(Error::unsupported(
                    format!(
                        "E-AC-3 dependent substream: channel map {map:#06x} names {}",
                        unsupported_name(i)
                    ),
                    "that channel has no ChannelPosition in this build",
                ));
            }
        };
        out.extend_from_slice(names);
    }
    if out.len() != channels {
        return Err(Error::corrupt(format!(
            "E-AC-3 dependent substream: channel map {map:#06x} names {} channels, \
             the substream carries {channels}",
            out.len()
        )));
    }
    Ok(out)
}

/// The Table E2.5 name of the map entry `i` that this build cannot place.
fn unsupported_name(i: usize) -> &'static str {
    match i {
        8 => "Ts (0x0080)",
        9 => "Lsd/Rsd (0x0040)",
        10 => "Lw/Rw (0x0020)",
        11 => "Vhl/Vhr (0x0010)",
        12 => "Vhc (0x0008)",
        13 => "Lts/Rts (0x0004)",
        _ => "LFE2 (0x0002)",
    }
}

/// Rank of a position in the canonical channel order (the bit order of the
/// layout mask: FL FR FC LFE BL BR FLC FRC BC SL SR).
fn rank(p: ChannelPosition) -> u8 {
    use ChannelPosition::*;
    match p {
        FrontLeft => 0,
        FrontRight => 1,
        FrontCenter => 2,
        Lfe => 3,
        BackLeft => 4,
        BackRight => 5,
        FrontLeftOfCenter => 6,
        FrontRightOfCenter => 7,
        BackCenter => 8,
        SideLeft => 9,
        SideRight => 10,
    }
}

/// Merge a dependent substream's channels into the frame's: a channel at a
/// position the frame already has replaces that channel, any other is added.
/// The result is in canonical channel order.
pub(crate) fn merge(mut frame: Vec<Channel>, dependent: Vec<Channel>) -> Vec<Channel> {
    for ch in dependent {
        match frame.iter_mut().find(|c| c.position == ch.position) {
            Some(slot) => slot.samples = ch.samples,
            None => frame.push(ch),
        }
    }
    frame.sort_by_key(|c| rank(c.position));
    frame
}

/// De-interleave `samples`, which hold one value per position per sample
/// frame, into one [`Channel`] per position.
pub(crate) fn split(samples: &[f32], positions: &[ChannelPosition]) -> Vec<Channel> {
    let n = positions.len();
    positions
        .iter()
        .enumerate()
        .map(|(c, &position)| Channel {
            position,
            samples: if n == 0 {
                Vec::new()
            } else {
                samples.iter().skip(c).step_by(n).copied().collect()
            },
        })
        .collect()
}

/// Interleave channels frame by frame. The frame count is the shortest
/// channel, so unequal channels cannot index past their end.
pub(crate) fn interleave(channels: &[Channel]) -> Vec<f32> {
    let frames = channels.iter().map(|c| c.samples.len()).min().unwrap_or(0);
    let mut out = Vec::with_capacity(frames * channels.len());
    for f in 0..frames {
        out.extend(channels.iter().map(|c| c.samples[f]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ChannelPosition::*;

    fn ch(position: ChannelPosition, v: f32) -> Channel {
        Channel { position, samples: vec![v, v] }
    }

    #[test]
    fn map_bits_name_positions_as_ffmpeg_does() {
        assert_eq!(positions(Some(0x8000), 1).unwrap(), vec![FrontLeft]);
        assert_eq!(positions(Some(0x0400), 2).unwrap(), vec![FrontLeftOfCenter, FrontRightOfCenter]);
        assert_eq!(positions(Some(0x0200), 2).unwrap(), vec![BackLeft, BackRight]);
        assert_eq!(positions(Some(0x1800), 2).unwrap(), vec![SideLeft, SideRight]);
        assert_eq!(positions(Some(0x0001), 1).unwrap(), vec![Lfe]);
        assert_eq!(positions(Some(0x8400), 3).unwrap(), vec![FrontLeft, FrontLeftOfCenter, FrontRightOfCenter]);
    }

    #[test]
    fn unnamed_and_missing_maps_are_refused_by_name() {
        for map in [None, Some(0x0080), Some(0x0040), Some(0x0010), Some(0x0004), Some(0x0002)] {
            let err = positions(map, 2).expect_err("refused");
            assert!(matches!(err, Error::Unsupported { .. }), "{map:?}: {err}");
        }
    }

    #[test]
    fn map_that_disagrees_with_the_channel_count_is_corrupt() {
        assert!(matches!(positions(Some(0x1800), 1), Err(Error::Corrupt { .. })));
        assert!(matches!(positions(Some(0x0400), 3), Err(Error::Corrupt { .. })));
    }

    #[test]
    fn merge_replaces_a_core_position_and_appends_the_rest_in_order() {
        let core = vec![
            ch(FrontLeft, 1.0),
            ch(FrontRight, 2.0),
            ch(FrontCenter, 3.0),
            ch(Lfe, 4.0),
            ch(BackLeft, 5.0),
            ch(BackRight, 6.0),
        ];
        // Replace BL/BR, add SL/SR (the dependent lists them out of order).
        let dep = vec![ch(BackLeft, 7.0), ch(BackRight, 8.0)];
        let merged = merge(core.clone(), dep);
        assert_eq!(merged.len(), 6);
        assert_eq!(merged[4].samples, vec![7.0, 7.0]);
        assert_eq!(merged[5].samples, vec![8.0, 8.0]);

        let side = vec![ch(SideRight, 10.0), ch(SideLeft, 9.0)];
        let merged = merge(core, side);
        let order: Vec<_> = merged.iter().map(|c| c.position).collect();
        assert_eq!(
            order,
            vec![FrontLeft, FrontRight, FrontCenter, Lfe, BackLeft, BackRight, SideLeft, SideRight]
        );
        assert_eq!(merged[6].samples, vec![9.0, 9.0]);
    }

    #[test]
    fn split_and_interleave_round_trip() {
        let interleaved = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]; // 3 frames of 2 channels
        let chans = split(&interleaved, &[FrontLeft, FrontRight]);
        assert_eq!(chans[0].samples, vec![1.0, 3.0, 5.0]);
        assert_eq!(interleave(&chans), interleaved.to_vec());
    }
}
