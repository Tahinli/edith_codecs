use ec_cc::ccdata::*;
use ec_cc::synth::*;
use ec_cc::{Cea608Decoder, Cea708Decoder};
use ec_subs::{Segment, plain_text};

fn run(script: &Script608, field: u8) -> Vec<ec_cc::CaptionCue> {
    let mut d = Cea608Decoder::new();
    for (i, p) in script.pairs.iter().enumerate() {
        d.push_pair((i as i64 + 1) * 1000, field, p[0], p[1]);
    }
    d.finish((script.pairs.len() as i64 + 1) * 1000)
}

#[test]
fn pop_on_basic() {
    let mut s = Script608::new();
    s.rcl()
        .enm()
        .pac(15, 4, false)
        .text("HELLO WORLD")
        .eoc()
        .idle(3)
        .edm();
    let cues = run(&s, 0);
    assert_eq!(cues.len(), 1);
    let c = &cues[0];
    assert_eq!(c.channel, 1);
    assert_eq!(plain_text(&c.cue.segments), "HELLO WORLD");
    assert_eq!((c.lines[0].row, c.lines[0].col), (14, 4));
    // EOC is the 13th pair (RCL x2, ENM x2, PAC x2, 6 text pairs) and its first copy sits at idx 12.
    assert_eq!(c.cue.start_us, 13_000);
    assert!(c.cue.end_us > c.cue.start_us);
}

#[test]
fn pop_on_two_captions_and_two_rows() {
    let mut s = Script608::new();
    s.rcl()
        .enm()
        .pac(14, 0, false)
        .text("LINE A")
        .pac(15, 0, false)
        .text("LINE B")
        .eoc();
    s.idle(5);
    s.enm().pac(15, 8, false).text("NEXT").eoc().idle(5).edm();
    let cues = run(&s, 0);
    assert_eq!(cues.len(), 2);
    assert_eq!(plain_text(&cues[0].cue.segments), "LINE A\nLINE B");
    assert_eq!(plain_text(&cues[1].cue.segments), "NEXT");
    assert_eq!(cues[0].cue.end_us, cues[1].cue.start_us);
    assert_eq!(cues[1].lines[0].col, 8);
}

#[test]
fn styles_and_char_sets() {
    let mut s = Script608::new();
    s.rcl().enm().pac_style(15, 0, 4, false, false).text("RED ");
    s.midrow(0, true, false)
        .text("ITA ")
        .midrow(0, false, true)
        .text("UND");
    s.special(7).extended(0, 0).eoc().idle(2).edm();
    let cues = run(&s, 0);
    assert_eq!(cues.len(), 1);
    let t = plain_text(&cues[0].cue.segments);
    assert!(t.starts_with("RED"), "{t}");
    assert!(t.contains('\u{266A}'), "{t}");
    // extended char replaces the filler '?'
    assert!(t.contains('Á') && !t.contains('?'), "{t}");
    let has = |f: &dyn Fn(&Segment) -> bool| cues[0].cue.segments.iter().any(f);
    assert!(has(&|g| matches!(
        g,
        Segment::Color {
            rgb: (255, 0, 0),
            ..
        }
    )));
    assert!(has(
        &|g| matches!(g, Segment::Italic(_)) || matches!(g, Segment::Underline(_))
    ));
}

#[test]
fn roll_up_cr_makes_new_cue_and_doubled_cr_rolls_once() {
    let mut s = Script608::new();
    s.roll_up(2)
        .pac(15, 0, false)
        .text("ONE")
        .idle(2)
        .cr()
        .text("TWO")
        .idle(2)
        .cr()
        .text("THREE")
        .idle(2)
        .edm();
    let cues = run(&s, 0);
    let texts: Vec<String> = cues.iter().map(|c| plain_text(&c.cue.segments)).collect();
    assert_eq!(texts, vec!["ONE", "ONE\nTWO", "TWO\nTHREE"]);
    assert_eq!(cues[0].cue.end_us, cues[1].cue.start_us);
}

#[test]
fn paint_on_and_channel_two() {
    let mut s = Script608::new();
    s.rdc().pac(10, 0, false).text("PAINTED").idle(3).edm();
    let c = run(&s, 0);
    assert_eq!(plain_text(&c[0].cue.segments), "PAINTED");
    // Channel 2 (CC2): the channel bit lives in the first byte of every control.
    let mut s2 = Script608::new();
    s2.ctrl(0x1C, 0x20)
        .ctrl(0x1C, 0x2E)
        .ctrl(0x1C, 0x60)
        .text("SECOND")
        .ctrl(0x1C, 0x2F)
        .idle(2)
        .ctrl(0x1C, 0x2C);
    let c2 = run(&s2, 0);
    assert_eq!(
        (c2[0].channel, plain_text(&c2[0].cue.segments)),
        (2, "SECOND".to_string())
    );
}

#[test]
fn text_mode_is_not_captioned() {
    let mut s = Script608::new();
    s.ctrl(0x14, 0x2A).text("TEXT MODE DATA").ctrl(0x14, 0x2C);
    assert!(run(&s, 0).is_empty());
}

#[test]
fn field_two_is_cc3() {
    let mut s = Script608::new();
    s.rcl()
        .enm()
        .pac(15, 0, false)
        .text("F2")
        .eoc()
        .idle(2)
        .edm();
    let c = run(&s, 1);
    assert_eq!(c[0].channel, 3);
}

#[test]
fn cc_data_and_wrappers() {
    let t = [CcTriplet {
        valid: true,
        cc_type: 0,
        data: [0x94, 0x2C],
    }];
    let cc = cc_data_bytes_for_test(&t);
    assert_eq!(parse_cc_data(&cc).unwrap(), t);
    let mut ud = b"GA94\x03".to_vec();
    ud.extend(&cc);
    assert_eq!(parse_mpeg2_user_data(&ud).unwrap(), t);
    // DVD form
    let dvd = [b'C', b'C', 0x01, 0xF8, 0xC2, 0xFF, 0x94, 0x2C];
    assert_eq!(parse_mpeg2_user_data(&dvd).unwrap(), t);
    // c608 sample: cdat + cdt2
    let mut sample = Vec::new();
    sample.extend_from_slice(&10u32.to_be_bytes());
    sample.extend_from_slice(b"cdat\x94\x2c");
    sample.extend_from_slice(&10u32.to_be_bytes());
    sample.extend_from_slice(b"cdt2\x41\x42");
    let tr = parse_c608_sample(&sample);
    assert_eq!(tr.len(), 2);
    assert_eq!((tr[1].cc_type, tr[1].data), (1, [0x41, 0x42]));
    // CDP
    let mut cdp = vec![
        0x96, 0x69, 0, 0x4F, 0x43, 0, 0, 0x72, 0xE1, 0xFC, 0x94, 0x2C, 0x74, 0, 0, 0,
    ];
    cdp[2] = cdp.len() as u8;
    assert_eq!(parse_cdp(&cdp).unwrap(), t);
    let mut s708 = Vec::new();
    s708.extend_from_slice(&(8 + cdp.len() as u32).to_be_bytes());
    s708.extend_from_slice(b"ccdp");
    s708.extend(&cdp);
    assert_eq!(parse_c708_sample(&s708), t);
}

fn cc_data_bytes_for_test(t: &[CcTriplet]) -> Vec<u8> {
    ec_cc::synth::cc_data_bytes(t)
}

fn dtvcc_packet(seq: u8, svc_blocks: &[u8]) -> Vec<(u8, u8, u8)> {
    // Header byte then data; total bytes padded to even.
    let mut data = svc_blocks.to_vec();
    if !(data.len() + 1).is_multiple_of(2) {
        data.push(0);
    }
    let size_code = data.len().div_ceil(2);
    let mut all = vec![(seq << 6) | size_code as u8];
    all.extend(data);
    let mut out = Vec::new();
    for (k, ch) in all.chunks(2).enumerate() {
        out.push((if k == 0 { 3 } else { 2 }, ch[0], ch[1]));
    }
    out
}

#[test]
fn cea708_service_one_window_text() {
    let mut blk = vec![0x98, 0x20, 0x4A, 0x10, 0x0A, 0x1F, 0x00]; // DF0 visible, row_count 11? rows=(0x0A&15)+1
    blk.extend_from_slice(b"HI THERE");
    blk.push(0x03);
    let mut block = vec![(1 << 5) | blk.len() as u8];
    block.extend(&blk);
    let mut d = Cea708Decoder::new();
    for (ty, a, b) in dtvcc_packet(0, &block) {
        d.push_triplet(1000, ty, a, b);
    }
    // service 2 block must be refused by name
    let blk2 = [0x20, b'X'];
    let mut b2 = vec![(2 << 5) | blk2.len() as u8];
    b2.extend(blk2);
    for (ty, a, b) in dtvcc_packet(1, &b2) {
        d.push_triplet(2000, ty, a, b);
    }
    // hide window 0
    let mut hide = vec![(1 << 5) | 2u8, 0x8A, 0x01];
    hide.push(0);
    for (ty, a, b) in dtvcc_packet(2, &hide) {
        d.push_triplet(5000, ty, a, b);
    }
    assert_eq!(d.unsupported_services(), vec![2]);
    let cues = d.finish(6000);
    assert_eq!(cues.len(), 1);
    assert_eq!(plain_text(&cues[0].cue.segments), "HI THERE");
    assert_eq!((cues[0].cue.start_us, cues[0].cue.end_us), (1000, 5000));
}
