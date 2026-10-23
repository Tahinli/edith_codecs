//! Atmos backward compatibility, tested on a synthetic 4-substream stream.
//!
//! No real Atmos TrueHD stream is available to this build, so the stream is
//! built from the real 7.1 Blu-ray remux's own access units (the first
//! [`AUS`] of its TrueHD track, 3 substreams each). For every access unit,
//! [`with_object_substream`] rebuilds it with a 4th directory entry: the
//! object-audio substream, here a run of [`FILLER_WORDS`] stand-in words
//! appended after substream 2's coded data. The rebuilt access unit is:
//!
//! - header: length word (new word count, parity nibble recomputed) and the
//!   original input timing;
//! - major sync, when the original has one: `num_substreams` (top nibble of
//!   byte 16) set to 4, and the major sync's own CRC-16 (bytes 26..28)
//!   recomputed;
//! - directory: the original three entries unchanged (their cumulative end
//!   pointers are relative to the directory end, so the coded data they name
//!   does not move relative to it), then a fourth entry whose end pointer is
//!   the old last pointer plus the filler length;
//! - substream data: the original substreams 0..2 bytes verbatim, then the
//!   filler.
//!
//! Decoding the synthetic stream must therefore equal decoding the original
//! bit-exactly: the 8-channel (7.1) presentation is the one decoded and
//! output, substream 3 is skipped via its end pointer, and the parity and
//! checksum of substreams 0..2 are still checked. Object audio itself is out
//! of scope and never decoded, so the filler's contents must not matter; a
//! second test checks that.
//!
//! Optional cross-check: with `EC_ATMOS_SYNTH_OUT=<path>` set, the synthetic
//! access units are also written there as a raw TrueHD elementary stream, so
//! `ffmpeg -f truehd -i <path> -f s32le` can be compared against this
//! decoder's output (ffmpeg decodes the same 8-channel presentation).
//!
//! Needs the 7.1 remux under `~/Downloads`. Absent, the tests skip loudly; with
//! `EC_REQUIRE_FIXTURES=1` an absent remux is a hard failure.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use ec_core::CodecId;
use ec_core::Demuxer;
use ec_core::frame::ChannelLayout;
use ec_matroska::MatroskaDemuxer;
use ec_truehd::TrueHdDecoder;
use ec_truehd::sync::AccessUnitHeader;

/// Access units taken from the start of the remux: 1 s of 48 kHz audio, one
/// major sync among them.
const AUS: usize = 1200;
/// Length of the object-audio stand-in, in 16-bit words.
const FILLER_WORDS: u16 = 4;
/// The 12-bit access-unit length word's maximum, in 16-bit words.
const MAX_AU_WORDS: usize = 0x0FFF;
const MAJOR_SYNC_LEN: usize = 28;

fn find_file() -> Option<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.push(p);
            }
        }
    }
    let home = std::env::var("HOME").ok()?;
    let mut all = Vec::new();
    walk(&PathBuf::from(home).join("Downloads"), &mut all);
    all.into_iter().find(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.contains("ReelR") && n.ends_with(".mkv"))
    })
}

/// The first [`AUS`] access units of the remux's TrueHD track, or `None`
/// (after a loud skip, or a hard failure under `EC_REQUIRE_FIXTURES`).
fn truehd_access_units() -> Option<Vec<Vec<u8>>> {
    let Some(path) = find_file() else {
        assert!(
            std::env::var_os("EC_REQUIRE_FIXTURES").is_none(),
            "EC_REQUIRE_FIXTURES=1 but the 7.1 TrueHD remux is absent under ~/Downloads"
        );
        eprintln!("skip: no 'ReelR ... TrueHD ...mkv' under ~/Downloads");
        return None;
    };
    let file = File::open(&path).expect("open the remux");
    let mut demux = MatroskaDemuxer::new(BufReader::new(file)).expect("open the container");
    let stream_index = demux
        .streams()
        .iter()
        .find(|s| s.params.codec == CodecId::TrueHd)
        .expect("a TrueHD track")
        .index;
    let mut aus = Vec::with_capacity(AUS);
    while aus.len() < AUS {
        let packet = match demux.next_packet() {
            Ok(p) => p,
            Err(e) if e.is_eof() => break,
            Err(e) => panic!("demux error: {e}"),
        };
        if packet.stream == stream_index {
            aus.push(packet.data.as_ref().to_vec());
        }
    }
    assert_eq!(
        aus.len(),
        AUS,
        "the remux has at least {AUS} TrueHD access units"
    );
    Some(aus)
}

/// The major sync's CRC-16 (poly `0x002D`, MSB-first table form), the same
/// one the crate's encoder writes and ffmpeg checks.
fn crc16_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    for (i, entry) in table.iter_mut().enumerate() {
        let mut c = (i as i32) << 24;
        for _ in 0..8 {
            let overflow = c >> 31;
            c = (c << 1) ^ ((0x002D_i32 << 16) & overflow);
        }
        *entry = (c as u32).swap_bytes();
    }
    table
}

/// The major sync's checksum field value: CRC-16 over bytes `0..24`, XORed
/// with the little-endian word at `24..26`, stored little-endian at `26..28`.
fn major_sync_checksum(sync: &[u8]) -> u16 {
    let table = crc16_table();
    let mut crc: u32 = 0;
    for &b in &sync[..24] {
        crc = table[usize::from((crc as u8) ^ b)] ^ (crc >> 8);
    }
    (crc as u16) ^ u16::from_le_bytes([sync[24], sync[25]])
}

/// The access-unit header's parity fold: the 4 header bytes and every
/// directory byte, XORed together, must leave a nibble pair that is bitwise
/// complementary (`((p >> 4) ^ p) & 0xF == 0xF`).
fn parity_ok(header: &[u8], directory: &[u8]) -> bool {
    let p = header.iter().chain(directory).fold(0u8, |a, &b| a ^ b);
    ((p >> 4) ^ p) & 0xF == 0xF
}

/// Rebuilds one access unit of the 7.1 remux with a 4th (object-audio)
/// directory entry whose coded data is `FILLER_WORDS` words of `filler`.
fn with_object_substream(au: &[u8], filler: u8) -> Vec<u8> {
    let header = AccessUnitHeader::parse(au).expect("the original access unit parses");
    assert_eq!(
        header.substreams.len(),
        3,
        "the remux is a 3-substream 7.1 stream"
    );
    let sync_len = if header.major_sync.is_some() {
        MAJOR_SYNC_LEN
    } else {
        0
    };
    let dir_start = 4 + sync_len;
    let old_dir = &au[dir_start..header.data_start];
    let old_last_end = header.substreams[2].end_offset_words;
    let new_last_end = old_last_end + FILLER_WORDS;

    // The original directory bytes are reused verbatim. The new entry is a
    // plain 2-byte word: no extra word, no check data, and the non-restart bit
    // (0x4) copied from substream 2. That bit is set exactly on access units
    // without a major sync, and ffmpeg rejects an entry that disagrees with
    // the access unit's major sync. Its end pointer is cumulative.
    let nonrestart = header.substreams[2].flags & 0x4;
    let mut dir = old_dir.to_vec();
    let entry = (u16::from(nonrestart) << 12) | new_last_end;
    dir.extend_from_slice(&entry.to_be_bytes());

    let coded = &au[header.data_start..header.length];
    let filler_bytes = usize::from(FILLER_WORDS) * 2;

    let mut body = Vec::new();
    if header.major_sync.is_some() {
        let mut sync = au[4..4 + MAJOR_SYNC_LEN].to_vec();
        assert_eq!(
            u16::from_le_bytes([sync[26], sync[27]]),
            major_sync_checksum(&sync),
            "the remux's major sync checksum matches the formula"
        );
        sync[16] = (sync[16] & 0x0F) | (4 << 4); // num_substreams = 4
        let checksum = major_sync_checksum(&sync);
        sync[26..28].copy_from_slice(&checksum.to_le_bytes());
        body.extend_from_slice(&sync);
    }
    body.extend_from_slice(&dir);
    body.extend_from_slice(coded);
    body.extend(std::iter::repeat_n(filler, filler_bytes));

    let total = 4 + body.len();
    let words = total / 2;
    assert!(words <= MAX_AU_WORDS, "synthetic access unit too long");
    let timing = [au[2], au[3]];
    // The parity nibble is the one value that makes the fold complementary.
    let nibble = (0u8..16)
        .find(|&cn| {
            let len_hi = (words >> 8) as u8 & 0x0F;
            let len_lo = (words & 0xFF) as u8;
            let head = [(cn << 4) | len_hi, len_lo, timing[0], timing[1]];
            parity_ok(&head, &dir)
        })
        .expect("one of the 16 nibble values satisfies the fold");
    let word = (u16::from(nibble) << 12) | words as u16;

    // Sanity: the same fold rule holds for the original access unit.
    assert!(
        parity_ok(&au[..4], old_dir),
        "the remux's own parity fold holds"
    );

    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&word.to_be_bytes());
    out.extend_from_slice(&timing);
    out.extend_from_slice(&body);
    out
}

/// One decoded frame: its layout and its planes.
type Decoded = Vec<(ChannelLayout, Vec<Vec<u8>>)>;

/// Decodes `aus` with a fresh decoder and returns every output frame's planes
/// plus the decoder's self-check tallies.
fn decode_all(aus: &[Vec<u8>]) -> (Decoded, ec_truehd::CheckStats) {
    let mut decoder = TrueHdDecoder::new();
    let mut frames = Vec::new();
    for au in aus {
        if let Some(frame) = decoder.push(au).expect("access unit decodes") {
            let planes = frame.data.iter().map(|b| b.as_ref().to_vec()).collect();
            frames.push((frame.layout, planes));
        }
    }
    (frames, decoder.check_stats())
}

#[test]
fn synthetic_atmos_decodes_bit_exact_to_the_original_7_1() {
    let Some(originals) = truehd_access_units() else {
        return;
    };
    let synthetic: Vec<Vec<u8>> = originals
        .iter()
        .map(|au| with_object_substream(au, 0xA5))
        .collect();

    for au in &synthetic {
        let header = AccessUnitHeader::parse(au).expect("synthetic access unit parses");
        assert_eq!(header.substreams.len(), 4, "a 4-entry directory");
        assert_eq!(
            header.channel_layout().expect("a layout"),
            ChannelLayout::Surround7_1,
            "the 8-ch presentation is the top PCM one"
        );
    }

    if let Some(out) = std::env::var_os("EC_ATMOS_SYNTH_OUT") {
        let raw: Vec<u8> = synthetic.iter().flatten().copied().collect();
        std::fs::write(&out, raw).expect("write the synthetic TrueHD stream");
        eprintln!("wrote synthetic stream to {}", Path::new(&out).display());
    }

    let (ours, _) = decode_all(&originals);
    let (synth, stats) = decode_all(&synthetic);
    assert!(!ours.is_empty(), "the original stream decodes to frames");
    assert_eq!(synth.len(), ours.len(), "same number of frames");
    for (i, ((la, pa), (lb, pb))) in ours.iter().zip(&synth).enumerate() {
        assert_eq!(*la, ChannelLayout::Surround7_1, "frame {i} is 7.1");
        assert_eq!(*lb, ChannelLayout::Surround7_1, "frame {i} is 7.1");
        assert_eq!(pa, pb, "frame {i} differs from the original");
    }
    eprintln!(
        "{} frames, 8-channel, bit-exact vs the original",
        ours.len()
    );

    // The PCM substreams' own checks still run on the synthetic stream.
    assert_eq!(stats.parity_failures, 0, "substream parity");
    assert_eq!(stats.restart_crc_failures, 0, "restart header CRC");
    assert_eq!(stats.lossless_check_failures, 0, "lossless check");
    assert_eq!(stats.length_mismatches, 0, "substream lengths");
}

#[test]
fn object_substream_contents_do_not_change_the_pcm_output() {
    let Some(originals) = truehd_access_units() else {
        return;
    };
    // Filler bytes are never read, so any value must decode identically.
    let zeros: Vec<Vec<u8>> = originals
        .iter()
        .map(|au| with_object_substream(au, 0x00))
        .collect();
    let ones: Vec<Vec<u8>> = originals
        .iter()
        .map(|au| with_object_substream(au, 0xFF))
        .collect();
    let (a, _) = decode_all(&zeros);
    let (b, _) = decode_all(&ones);
    assert!(!a.is_empty());
    assert_eq!(
        a, b,
        "the object substream's bytes are skipped, not decoded"
    );
}
