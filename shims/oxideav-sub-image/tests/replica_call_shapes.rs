//! The replica's own call shapes, transcribed, so a swap is proven at compile
//! time and not at the first PGS track a user opens.
//!
//! Every block below is the engine's code with its surroundings removed:
//! `subtitle.rs:33` (`plain_text`), `:90-125` (`CueImage::rgba` — the `.sup`
//! framing rebuilt around a Matroska block and decoded), `:518` (`srt::parse`
//! around a synthetic timing line), `:555` (`oxideav_ass::parse`) and
//! `:603-607` (the four parsers as one function-pointer type). If any of them
//! stops compiling against these shims, the swap has broken, whatever the
//! crates' own tests say.

use oxideav_core::{CodecId, CodecParameters, Frame, Packet, TimeBase};
use oxideav_subtitle::ir::plain_text;

/// `subtitle.rs:604-607` — the extension-to-parser table, as one `fn` type.
type Parse = fn(&[u8]) -> oxideav_core::Result<oxideav_subtitle::ir::SubtitleTrack>;

fn parser_for(ext: &str) -> Option<Parse> {
    match ext {
        "srt" => Some(oxideav_subtitle::srt::parse),
        "vtt" | "webvtt" => Some(oxideav_subtitle::webvtt::parse),
        "ass" | "ssa" => Some(oxideav_ass::parse),
        _ => None,
    }
}

#[test]
fn the_parser_table_and_the_cue_fields_the_replica_reads() {
    for (ext, doc) in [
        ("srt", "1\n00:00:01,000 --> 00:00:02,000\n<i>hi</i>\n"),
        (
            "vtt",
            "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\n<i>hi</i>\n",
        ),
        (
            "ass",
            "[Events]\nFormat: Layer,Start,End,Style,Name,MarginL,MarginR,MarginV,Effect,Text\n\
             Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,{\\i1}hi\n",
        ),
    ] {
        let parse = parser_for(ext).expect("the replica routes these four extensions");
        let track = parse(doc.as_bytes()).unwrap();
        // `subtitle.rs:623-632`: cues mapped onto the engine's own `Cue`.
        let cues: Vec<(i64, i64, String)> = track
            .cues
            .iter()
            .map(|c| (c.start_us, c.end_us, plain_text(&c.segments)))
            .collect();
        assert_eq!(cues, vec![(1_000_000, 2_000_000, "hi".to_owned())], "{ext}");
    }
    assert!(parser_for("txt").is_none());
}

/// `subtitle.rs:516-527` — an `S_TEXT/UTF8` block's markup resolved by wrapping
/// it in a synthetic SRT.
#[test]
fn a_matroska_text_block_is_resolved_through_the_srt_parser() {
    let text = "<i>tilted</i> and plain";
    let doc = format!("1\n00:00:00,000 --> 00:00:01,000\n{}\n", text.trim_end());
    let resolved = match oxideav_subtitle::srt::parse(doc.as_bytes()) {
        Ok(track) => track
            .cues
            .first()
            .map(|c| plain_text(&c.segments))
            .unwrap_or_default(),
        Err(_) => text.trim_end().to_owned(),
    };
    assert_eq!(resolved, "tilted and plain");
}

/// `subtitle.rs:89-125` — a Matroska PGS block, framed back into a `.sup` and
/// decoded to straight RGBA the size of the disc's own canvas.
#[test]
fn a_matroska_pgs_block_decodes_the_way_the_replica_decodes_one() {
    let set = matroska_block();
    let params = CodecParameters::subtitle(CodecId::new(oxideav_sub_image::PGS_CODEC_ID));
    let mut decoder = oxideav_sub_image::pgs::make_decoder(&params).unwrap();
    let mut sup = Vec::with_capacity(set.len() + 64);
    let mut shows = false;
    for (kind, body) in segments(&set).unwrap() {
        sup.extend_from_slice(b"PG");
        sup.extend_from_slice(&[0; 8]);
        sup.push(kind);
        sup.extend_from_slice(&(body.len() as u16).to_be_bytes());
        sup.extend_from_slice(body);
        match kind {
            oxideav_sub_image::pgs::SEG_PCS => {
                shows = body.get(10).is_some_and(|&n| n > 0);
            }
            oxideav_sub_image::pgs::SEG_END if shows => break,
            _ => {}
        }
    }
    decoder
        .send_packet(&Packet::new(0, TimeBase::new(1, 90_000), sup))
        .unwrap();
    let rgba = match decoder.receive_frame().unwrap() {
        Frame::Video(frame) => frame.planes.into_iter().next().map(|p| p.data),
        _ => None,
    }
    .expect("a display set is a picture");
    // The whole canvas, transparent where the cue paints nothing.
    assert_eq!(rgba.len(), 4 * 2 * 4);
    assert_eq!(&rgba[0..4], &[0, 0, 0, 0]);
    assert_eq!(&rgba[4..12], &[255, 255, 255, 255, 255, 255, 255, 255]);

    // The erase set after the composition must not be what gets decoded: the
    // loop above stops at the `END` that closes the showing set.
    let all: Vec<u8> = set.clone();
    assert!(
        segments(&all).unwrap().len() > 5,
        "the block holds both sets"
    );
}

/// `subtitle.rs:131-140` — the segments of a display set as a Matroska block
/// holds them: type, big-endian size, body.
fn segments(set: &[u8]) -> Option<Vec<(u8, &[u8])>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < set.len() {
        let size = u16::from_be_bytes([*set.get(at + 1)?, *set.get(at + 2)?]) as usize;
        out.push((set[at], set.get(at + 3..at + 3 + size)?));
        at += 3 + size;
    }
    Some(out)
}

/// A block holding a composition *and* the erase that follows it — the packing
/// the replica's comment warns about.
fn matroska_block() -> Vec<u8> {
    let block = |kind: u8, body: &[u8]| {
        let mut out = vec![kind];
        out.extend_from_slice(&(body.len() as u16).to_be_bytes());
        out.extend_from_slice(body);
        out
    };
    let mut pcs = vec![0, 4, 0, 2, 0x10, 0, 0, 0x80, 0, 0, 1];
    pcs.extend_from_slice(&[0, 0, 0, 0, 0, 1, 0, 0]);
    let pds = vec![0, 0, 1, 235, 128, 128, 255];
    let rle = vec![0x00, 0x82, 0x01, 0x00, 0x00];
    let mut ods = vec![0, 0, 0, 0xC0];
    ods.extend_from_slice(&((rle.len() + 4) as u32).to_be_bytes()[1..]);
    ods.extend_from_slice(&[0, 2, 0, 1]);
    ods.extend_from_slice(&rle);
    // The erase: a composition with no objects at all.
    let erase = vec![0, 4, 0, 2, 0x10, 0, 1, 0, 0, 0, 0];
    [
        block(oxideav_sub_image::pgs::SEG_PCS, &pcs),
        block(oxideav_sub_image::pgs::SEG_PDS, &pds),
        block(oxideav_sub_image::pgs::SEG_ODS, &ods),
        block(oxideav_sub_image::pgs::SEG_END, &[]),
        block(oxideav_sub_image::pgs::SEG_PCS, &erase),
        block(oxideav_sub_image::pgs::SEG_END, &[]),
    ]
    .concat()
}
