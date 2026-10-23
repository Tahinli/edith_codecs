//! Reproducers from the `avi_read` and `avi_seek` fuzz targets, kept as hand-minimised inline AVI
//! files. Each test names the crash it guards.

use std::io::Cursor;

use ec_riff::AviReader;
use crate::common::{chunk, list};

fn riff(body: &[u8]) -> Vec<u8> {
    let mut b = b"AVI ".to_vec();
    b.extend(body);
    let mut v = b"RIFF".to_vec();
    v.extend((b.len() as u32).to_le_bytes());
    v.extend(b);
    v
}

/// A 56-byte `strh` for one audio stream with `length` time units.
fn strh_audio(length: u32) -> Vec<u8> {
    let mut b = vec![0u8; 56];
    b[0..4].copy_from_slice(b"auds");
    b[20..24].copy_from_slice(&1u32.to_le_bytes());
    b[24..28].copy_from_slice(&48_000u32.to_le_bytes());
    b[32..36].copy_from_slice(&length.to_le_bytes());
    b[44..48].copy_from_slice(&4u32.to_le_bytes());
    chunk(b"strh", &b)
}

/// A 16-byte PCM `strf`: stereo, 48 kHz, 16-bit.
fn strf_pcm() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend(1u16.to_le_bytes());
    b.extend(2u16.to_le_bytes());
    b.extend(48_000u32.to_le_bytes());
    b.extend((48_000u32 * 4).to_le_bytes());
    b.extend(4u16.to_le_bytes());
    b.extend(16u16.to_le_bytes());
    chunk(b"strf", &b)
}

fn strl(extra: &[u8]) -> Vec<u8> {
    list(b"strl", &[strh_audio(4), strf_pcm(), extra.to_vec()].concat())
}

fn hdrl(strls: &[u8]) -> Vec<u8> {
    list(b"hdrl", &[chunk(b"avih", &[0u8; 56]), strls.to_vec()].concat())
}

/// `idx1` declares a 0xC8000060-byte body in a file a few hundred bytes long. Parsing must fail
/// as corrupt; the body buffer may not be sized from the declared length.
#[test]
fn idx1_size_past_end_of_file_is_refused() {
    let movi = list(b"movi", &chunk(b"00wb", &[0u8; 16]));
    let mut idx = b"idx1".to_vec();
    idx.extend(0xC800_0060u32.to_le_bytes());
    idx.extend([0u8; 16]);
    let file = riff(&[hdrl(&strl(&[])), movi, idx].concat());
    assert!(AviReader::new(Cursor::new(&file)).is_err());
}

/// A `movi` list whose body is only its list type (size 4) made `seek_to_stream_time` call
/// `clamp` with min > max and panic.
#[test]
fn seek_in_empty_movi_does_not_panic() {
    let file = riff(&[hdrl(&strl(&[])), list(b"movi", &[])].concat());
    let mut reader = AviReader::new(Cursor::new(&file)).expect("headers parse");
    assert!(reader.seek_to_stream_time(0, 5, false).is_ok());
}

/// Nested `rec ` lists recursed once per level. 100 000 nested levels overflowed the stack; the
/// reader now refuses anything past its depth cap.
#[test]
fn deeply_nested_rec_lists_are_refused_not_recursed() {
    const N: usize = 100_000;
    let mut rec = Vec::with_capacity(12 * N);
    for i in 0..N {
        // Each level is a LIST header, its size, and the `rec ` type: 12 bytes, plus the levels
        // nested inside it.
        let size = 4 + 12 * (N - 1 - i) as u32;
        rec.extend(b"LIST");
        rec.extend(size.to_le_bytes());
        rec.extend(b"rec ");
    }
    let file = riff(&[hdrl(&strl(&[])), list(b"movi", &rec)].concat());
    let mut reader = AviReader::new(Cursor::new(&file)).expect("headers parse");
    assert!(reader.next_packet().is_err());
}

/// An odml super-index entry at offset `u64::MAX` made `offset + 8` overflow (a panic in
/// overflow-checked builds, a wrap in release).
#[test]
fn odml_index_offset_at_u64_max_does_not_overflow() {
    let mut indx = vec![0u8; 24 + 16];
    indx[0..2].copy_from_slice(&4u16.to_le_bytes());
    indx[3] = 0; // super index
    indx[4..8].copy_from_slice(&1u32.to_le_bytes());
    indx[8..12].copy_from_slice(b"00wb");
    indx[24..32].copy_from_slice(&u64::MAX.to_le_bytes());
    indx[32..36].copy_from_slice(&24u32.to_le_bytes());
    let file = riff(&[hdrl(&strl(&chunk(b"indx", &indx))), list(b"movi", &[])].concat());
    assert!(AviReader::new(Cursor::new(&file)).is_ok());
}

/// A seek target far past the stream's length made `movi_start + estimate` overflow u64 (a panic in
/// overflow-checked builds). The estimate must saturate instead.
#[test]
fn seek_to_huge_target_does_not_overflow() {
    let movi = list(b"movi", &chunk(b"00wb", &[0u8; 16]));
    let file = riff(&[hdrl(&list(b"strl", &[strh_audio(1), strf_pcm()].concat())), movi].concat());
    let mut reader = AviReader::new(Cursor::new(&file)).expect("headers parse");
    let _ = reader.seek_to_stream_time(0, u64::MAX, false);
}
