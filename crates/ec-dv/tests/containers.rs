//! DV in AVI and MOV: the containers hand over whole DV frames; the decode must match the raw
//! `.dv` decode, and the raw demuxer's sniffing/seeking must behave.
mod common;

use std::io::Cursor;

use common::*;
use ec_core::registry::{CodecId, Decoder, Demuxer, SeekMode};
use ec_core::timebase::{TimeBase, Timestamp};
use ec_dv::{DvDecoder, DvDemuxer, DvVideoDecoder, is_dv};
use ec_mp4::Mp4Demuxer;
use ec_riff::AviDemuxer;

fn dv_packets(d: &mut dyn Demuxer) -> Vec<Vec<u8>> {
    let vs = d.streams().iter().position(|s| s.params.codec == CodecId::DvVideo).expect("no DvVideo stream") as u32;
    let mut out = Vec::new();
    while let Ok(p) = d.next_packet() {
        if p.stream == vs {
            out.push(p.data.to_vec());
        }
    }
    out
}

fn native(frames: &[Vec<u8>]) -> Vec<u8> {
    let mut dec = DvVideoDecoder::new();
    let mut out = Vec::new();
    for f in frames {
        for p in &dec.decode(f).unwrap().planes {
            out.extend_from_slice(p);
        }
    }
    out
}

#[test]
fn avi_and_mov_carry_dv() {
    if !ffmpeg_or_skip() {
        return;
    }
    for f in [&FLAVOURS[0], &FLAVOURS[1], &FLAVOURS[3], &FLAVOURS[5]] {
        for ext in ["avi", "mov"] {
            let Some(path) = make(f, "c", ext, true, 8) else {
                // ffmpeg's avi/mov muxers do not take every DV flavour
                eprintln!("{} cannot be muxed as {ext}", f.name);
                continue;
            };
            let bytes = std::fs::read(&path).unwrap();
            let frames = if ext == "avi" {
                let mut d = AviDemuxer::new(Cursor::new(bytes)).unwrap();
                dv_packets(&mut d)
            } else {
                let mut d = Mp4Demuxer::new(Cursor::new(bytes)).unwrap();
                dv_packets(&mut d)
            };
            assert!(frames.len() >= 2, "{} {ext}: no DV packets", f.name);
            let got = native(&frames);
            let want = ffmpeg_raw_video(&path, &[]);
            assert_eq!(got.len(), want.len(), "{} in {ext}: size", f.name);
            let diff = got.iter().zip(&want).position(|(a, b)| a != b);
            assert!(diff.is_none(), "{} in {ext} differs at byte {diff:?}", f.name);
        }
    }
}

#[test]
fn decoder_trait_output_formats() {
    if !ffmpeg_or_skip() {
        return;
    }
    use ec_core::frame::{Frame, Interlace, PixelFormat};
    use ec_core::registry::CodecParameters;
    let cases = [(0usize, PixelFormat::I422), (1, PixelFormat::I420), (3, PixelFormat::I422), (5, PixelFormat::I422)];
    for (fi, fmt) in cases {
        let f = &FLAVOURS[fi];
        let path = make(f, "t", "dv", true, 8).unwrap();
        let mut dm = DvDemuxer::new(Cursor::new(std::fs::read(&path).unwrap())).unwrap();
        let mut dec = DvDecoder::new(CodecParameters::new(CodecId::DvVideo)).unwrap();
        let mut native_dec = DvVideoDecoder::new();
        let mut n = 0;
        while let Ok(p) = dm.next_packet() {
            if p.stream != 0 {
                continue;
            }
            dec.send_packet(&p).unwrap();
            let Frame::Video(v) = dec.receive_frame().unwrap() else { panic!() };
            assert_eq!(v.format, fmt, "{}", f.name);
            assert_eq!(v.width as usize, f.size.split('x').next().unwrap().parse::<usize>().unwrap());
            assert!(v.interlace.is_interlaced() || f.name == "hd720p50", "{} interlace", f.name);
            let nat = native_dec.decode(&p.data).unwrap();
            assert_eq!(v.interlace, nat.interlace);
            assert_eq!(v.planes[0].data[..], nat.planes[0][..]);
            if f.name == "ntsc411" {
                // 4:1:1 -> 4:2:2 is exact duplication
                let cw = nat.chroma_width();
                for y in [0usize, 100, 479] {
                    for x in [0usize, 7, cw - 1] {
                        let s = nat.planes[1][y * cw + x];
                        assert_eq!(v.planes[1].data[y * v.planes[1].stride + 2 * x], s);
                        assert_eq!(v.planes[1].data[y * v.planes[1].stride + 2 * x + 1], s);
                    }
                }
            }
            n += 1;
        }
        assert!(n >= 2);
        assert_eq!(Interlace::Progressive, Interlace::default());
    }
}

#[test]
fn field_order_matches_ffprobe_view() {
    if !ffmpeg_or_skip() {
        return;
    }
    use ec_core::frame::Interlace;
    // ffmpeg's encoder: +ilme marks the stream interlaced; SD output is bottom-field-first
    // unless the (top-field-first) flag is requested.
    let f = &FLAVOURS[0];
    let ilme = make(f, "il", "dv", true, 4).unwrap();
    let prog = make(f, "pr", "dv", false, 4).unwrap();
    let first = |p: &std::path::Path| {
        let data = std::fs::read(p).unwrap();
        DvVideoDecoder::new().decode(&data).unwrap().interlace
    };
    let a = first(&ilme);
    let b = first(&prog);
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_frames", "-read_intervals", "%+#1", "-show_entries", "frame=interlaced_frame,top_field_first,flags", "-of", "default=nw=1"])
        .arg(&ilme)
        .output();
    if let Ok(o) = out {
        let txt = String::from_utf8_lossy(&o.stdout).to_string();
        let interlaced = txt.contains("interlaced_frame=1") || txt.contains("interlaced");
        let tff = txt.contains("top_field_first=1") || txt.contains("top_first");
        assert_eq!(a.is_interlaced(), interlaced, "ffprobe says: {txt}");
        if interlaced {
            assert_eq!(a == Interlace::TopFieldFirst, tff, "ffprobe says: {txt}");
        }
    }
    assert!(!b.is_interlaced() || a.is_interlaced());
}

#[test]
fn raw_demuxer_sniff_seek_and_junk() {
    if !ffmpeg_or_skip() {
        return;
    }
    let f = &FLAVOURS[0];
    let path = make(f, "s", "dv", true, 8).unwrap();
    let data = std::fs::read(&path).unwrap();
    assert!(is_dv(&data[..16384]));
    assert!(!is_dv(&vec![0x55u8; 16384]));
    assert!(!is_dv(&[]));
    // leading junk is skipped
    let mut junk = vec![0xAAu8; 777];
    junk.extend_from_slice(&data);
    let mut d = DvDemuxer::new(Cursor::new(junk)).unwrap();
    assert_eq!(d.profile().frame_size, 120000);
    assert_eq!(d.streams()[0].params.codec, CodecId::DvVideo);
    assert_eq!(d.streams().len(), 2);
    let mut vids = 0;
    let mut last_pts = -1;
    while let Ok(p) = d.next_packet() {
        if p.stream == 0 {
            assert_eq!(p.pts, Some(last_pts + 1));
            last_pts = p.pts.unwrap();
            vids += 1;
        }
    }
    assert!(vids >= 2);
    // seek to frame 3 by time: 3 * 1001/30000 s
    d.seek(0, Timestamp::new(3, TimeBase::NTSC_VIDEO), SeekMode::SyncBefore).unwrap();
    let p = d.next_packet().unwrap();
    assert_eq!((p.stream, p.pts), (0, Some(3)));
    assert!(p.data[..] == data[3 * 120000..4 * 120000]);
}

#[test]
fn audio_rates_and_12bit_match_ffmpeg() {
    if !ffmpeg_or_skip() {
        return;
    }
    let f = &FLAVOURS[1]; // PAL
    for ar in ["44100", "32000"] {
        let dir = scratch("ar");
        let p = dir.join(format!("a{ar}.dv"));
        let vsrc = "testsrc2=size=720x576:rate=25:duration=2,noise=alls=8:allf=t";
        let asrc = format!("sine=frequency=1000:sample_rate={ar}:duration=2");
        if !run(&["-f", "lavfi", "-i", vsrc, "-f", "lavfi", "-i", &asrc, "-ac", "2", "-frames:v", "6", "-c:v", "dvvideo", "-pix_fmt", f.pix_fmt, "-c:a", "pcm_s16le", "-ar", ar, "-shortest", "-f", "dv", p.to_str().unwrap()]) {
            assert!(!require(), "ffmpeg could not write {ar} Hz DV");
            continue;
        }
        compare_audio(&p, ar.parse().unwrap());
        // flip the audio source pack to 12-bit non-linear and compare again
        let mut data = std::fs::read(&p).unwrap();
        for frame in data.as_chunks_mut::<144000>().0.iter_mut() {
            for c in 0..10usize {
                let offs = if c & 1 == 1 { 80 * 6 + 3 + c * 12000 } else { 80 * 6 + 80 * 16 * 3 + 3 + c * 12000 };
                if frame[offs] == 0x50 {
                    frame[offs + 4] = (frame[offs + 4] & 0xf8) | 1;
                }
            }
        }
        let q = dir.join(format!("a{ar}_12.dv"));
        std::fs::write(&q, &data).unwrap();
        compare_audio(&q, ar.parse().unwrap());
    }
}

fn compare_audio(path: &std::path::Path, rate: u32) {
    let data = std::fs::read(path).unwrap();
    let mut dm = DvDemuxer::new(Cursor::new(data)).unwrap();
    let mut ours: Vec<Vec<u8>> = vec![Vec::new(); dm.streams().len() - 1];
    while let Ok(p) = dm.next_packet() {
        if p.stream > 0 {
            ours[p.stream as usize - 1].extend_from_slice(&p.data);
        }
    }
    assert!(!ours.is_empty());
    for (n, o) in ours.iter().enumerate() {
        assert_eq!(dm.streams()[n + 1].params.audio().unwrap().sample_rate, rate);
        let w = ffmpeg_pcm(path, n).expect("ffmpeg audio");
        let len = o.len().min(w.len());
        assert!(len > 4000);
        assert!(o[..len] == w[..len], "{} stream {n}", path.display());
    }
}
