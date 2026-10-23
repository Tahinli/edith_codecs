//! Differential tests: every frame decoded here must equal, byte for byte, what ffmpeg's `mjpeg`
//! decoder writes in its native pixel format (yuvj420p, yuvj422p, yuvj444p, gray).
//!
//! All sources are synthetic: lavfi test patterns encoded by ffmpeg, and the Pillow-made JPEGs in
//! `tests/data` (restart markers, progressive). Expected pixels are never stored; ffmpeg is the
//! oracle at run time. Without ffmpeg the tests skip, unless `EC_REQUIRE_FIXTURES` is set.

use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::frame::{Interlace, PixelFormat, VideoFrame};
use ec_core::registry::{CodecId, CodecParameters, Demuxer, MediaParameters};
use ec_image::Limits;
use ec_mjpeg::MjpegDecoder;

fn require() -> bool {
    std::env::var_os("EC_REQUIRE_FIXTURES").is_some()
}

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// False (after saying so) when the oracle is missing and fixtures are optional.
fn oracle() -> bool {
    if have_ffmpeg() {
        return true;
    }
    assert!(!require(), "ffmpeg missing and EC_REQUIRE_FIXTURES is set");
    eprintln!("SKIP: no ffmpeg");
    false
}

fn scratch(tag: &str) -> PathBuf {
    let base = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("ec-mjpeg-{}-{}", std::process::id(), tag));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn ffmpeg(args: &[&str]) {
    let ok = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-nostdin"])
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(ok, "ffmpeg {args:?} failed");
}

/// ffmpeg's own decode of `input` (anything the mjpeg decoder reads), native pixel format.
fn oracle_decode(input: &Path, extra: &[&str]) -> Vec<u8> {
    let out = input.with_extension("oracle.yuv");
    let mut args: Vec<&str> = vec!["-c:v", "mjpeg"];
    args.extend_from_slice(extra);
    args.extend([
        "-i",
        input.to_str().unwrap(),
        "-f",
        "rawvideo",
        out.to_str().unwrap(),
    ]);
    ffmpeg(&args);
    std::fs::read(out).unwrap()
}

fn frame_bytes(f: &VideoFrame) -> Vec<u8> {
    let mut v = Vec::new();
    for (i, p) in f.planes.iter().enumerate() {
        let (row, rows) = f.format.plane_geometry(f.width, f.height)[i];
        for y in 0..rows {
            v.extend_from_slice(&p.data[y * p.stride..y * p.stride + row]);
        }
    }
    v
}

struct Opened {
    params: CodecParameters,
    field_order: Option<Interlace>,
    packets: Vec<Vec<u8>>,
}

/// Demux the first video track with our own container readers.
fn open(path: &Path) -> Opened {
    let file = std::fs::File::open(path).unwrap();
    let mut reader = BufReader::new(file);
    let mut head = [0u8; 12];
    std::io::Read::read(&mut reader, &mut head).unwrap();
    std::io::Seek::rewind(&mut reader).unwrap();
    let (mut demux, field_order): (Box<dyn Demuxer>, Option<Interlace>) =
        if ec_matroska::is_matroska(&head) {
            let d = ec_matroska::MatroskaDemuxer::new(reader).expect("matroska opens");
            let fo = d.field_order(0);
            (Box::new(d), fo)
        } else {
            let d = ec_mp4::Mp4Demuxer::new(reader).expect("mp4 opens");
            let fo = d.field_order(0);
            (Box::new(d), fo)
        };
    let params = demux.streams()[0].params.clone();
    assert_eq!(
        params.codec,
        CodecId::Mjpeg,
        "{}: container mapping",
        path.display()
    );
    let mut packets = Vec::new();
    while let Ok(p) = demux.next_packet() {
        packets.push(p.data.to_vec());
    }
    Opened {
        params,
        field_order,
        packets,
    }
}

fn decode_all(o: &Opened, order: Interlace) -> Vec<VideoFrame> {
    let mut dec = MjpegDecoder::new(o.params.clone()).unwrap();
    dec.set_field_order(order);
    o.packets
        .iter()
        .map(|p| dec.decode(p, None).expect("packet decodes"))
        .collect()
}

fn assert_equal(tag: &str, ours: &[VideoFrame], theirs: &[u8]) {
    let mut mine = Vec::new();
    for f in ours {
        mine.extend(frame_bytes(f));
    }
    assert_eq!(mine.len(), theirs.len(), "{tag}: output size");
    if let Some(at) = mine.iter().zip(theirs).position(|(a, b)| a != b) {
        panic!("{tag}: first difference at byte {at} of {}", mine.len());
    }
}

fn format_name(f: PixelFormat) -> &'static str {
    match f {
        PixelFormat::I420 => "yuvj420p",
        PixelFormat::I422 => "yuvj422p",
        PixelFormat::I444 => "yuvj444p",
        PixelFormat::Gray8 => "gray",
        other => panic!("{other:?}"),
    }
}

#[test]
fn still_fixtures_match_ffmpeg() {
    if !oracle() {
        return;
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let work = scratch("stills");
    let mut n = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("jpg") {
            continue;
        }
        let data = std::fs::read(&path).unwrap();
        let img = ec_image::jpeg::decode_yuv(&data, Limits::default()).unwrap();
        let name = path.file_stem().unwrap().to_str().unwrap();
        let copy = work.join(format!("{name}.jpg"));
        std::fs::copy(&path, &copy).unwrap();
        let theirs = oracle_decode(&copy, &[]);
        let mut mine = Vec::new();
        for p in &img.planes {
            mine.extend_from_slice(&p.data);
        }
        assert_eq!(mine, theirs, "{name} (progressive={})", img.progressive);
        n += 1;
    }
    assert!(n >= 15, "fixtures found: {n}");
}

/// ffmpeg's mjpeg encoder has no gray format, so gray frames come from Pillow's JPEG, wrapped
/// into mov and mkv by a stream copy.
#[test]
fn gray_video_matches_ffmpeg() {
    if !oracle() {
        return;
    }
    let work = scratch("gray");
    for name in ["basegray", "rstgray", "proggray"] {
        let jpg = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/data/{name}.jpg"));
        for ext in ["mov", "mkv"] {
            let path = work.join(format!("{name}.{ext}"));
            ffmpeg(&[
                "-i",
                jpg.to_str().unwrap(),
                "-c",
                "copy",
                path.to_str().unwrap(),
            ]);
            let o = open(&path);
            let frames = decode_all(&o, Interlace::TopFieldFirst);
            assert_eq!(frames[0].format, PixelFormat::Gray8);
            assert_equal(
                &format!("{name} {ext}"),
                &frames,
                &oracle_decode(&path, &[]),
            );
        }
    }
}

#[test]
fn video_matrix_matches_ffmpeg() {
    if !oracle() {
        return;
    }
    let work = scratch("matrix");
    let mut cases = 0;
    for (pix, expect) in [
        ("yuvj420p", PixelFormat::I420),
        ("yuvj422p", PixelFormat::I422),
        ("yuvj444p", PixelFormat::I444),
    ] {
        for (size, q, huff, ext) in [
            ("64x48", "2", "optimal", "mov"),
            ("64x48", "12", "default", "mkv"),
            ("50x38", "5", "optimal", "mkv"),
            ("51x37", "31", "default", "mov"),
            ("160x96", "8", "optimal", "mov"),
        ] {
            let path = work.join(format!("{pix}-{size}-q{q}-{huff}.{ext}"));
            let vf = format!("noise=alls=18:allf=t,format={pix}");
            ffmpeg(&[
                "-f",
                "lavfi",
                "-i",
                &format!("testsrc2=size={size}:rate=10"),
                "-frames:v",
                "6",
                "-vf",
                &vf,
                "-c:v",
                "mjpeg",
                "-pix_fmt",
                pix,
                "-q:v",
                q,
                "-huffman",
                huff,
                path.to_str().unwrap(),
            ]);
            let o = open(&path);
            let frames = decode_all(&o, Interlace::TopFieldFirst);
            assert_eq!(frames.len(), 6);
            for f in &frames {
                assert_eq!(f.format, expect, "{pix}");
                assert!(f.color.full_range, "MJPEG is full range");
                assert_eq!(f.interlace, Interlace::Progressive);
            }
            assert_equal(
                &format!("{pix} {size} q{q} {huff} {ext}"),
                &frames,
                &oracle_decode(&path, &[]),
            );
            assert_eq!(format_name(frames[0].format), pix);
            cases += 1;
        }
    }
    eprintln!("{cases} matrix cases identical");
}

/// Remove every DHT segment, as AVI1/MJPG writers do.
fn strip_dht(jpeg: &[u8]) -> Vec<u8> {
    let mut out = vec![0xff, 0xd8];
    let mut at = 2;
    while at + 4 <= jpeg.len() {
        assert_eq!(jpeg[at], 0xff);
        let marker = jpeg[at + 1];
        let len = usize::from(u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]));
        if marker == 0xda {
            out.extend_from_slice(&jpeg[at..]);
            return out;
        }
        if marker != 0xc4 {
            out.extend_from_slice(&jpeg[at..at + 2 + len]);
        }
        at += 2 + len;
    }
    panic!("no SOS");
}

fn chunk(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = kind.to_vec();
    v.extend((body.len() as u32).to_le_bytes());
    v.extend_from_slice(body);
    if body.len() % 2 == 1 {
        v.push(0);
    }
    v
}

fn list(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = kind.to_vec();
    b.extend_from_slice(body);
    chunk(b"LIST", &b)
}

/// A minimal AVI with one video stream tagged `MJPG`, one packet per `00dc` chunk.
fn write_avi(path: &Path, w: u32, h: u32, packets: &[Vec<u8>]) {
    let le = |v: &[u32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let avih = le(&[
        100_000,
        0,
        0,
        0x10,
        packets.len() as u32,
        0,
        1,
        1 << 20,
        w,
        h,
        0,
        0,
        0,
        0,
    ]);
    let mut strh = b"vidsMJPG".to_vec();
    strh.extend(le(&[
        0,
        0,
        0,
        1,
        10,
        0,
        packets.len() as u32,
        1 << 20,
        0xffff_ffff,
        0,
    ]));
    for v in [0u16, 0, w as u16, h as u16] {
        strh.extend(v.to_le_bytes());
    }
    let mut strf = le(&[40, w, h]);
    strf.extend(1u16.to_le_bytes());
    strf.extend(24u16.to_le_bytes());
    strf.extend(b"MJPG");
    strf.extend(le(&[w * h * 3, 0, 0, 0, 0]));
    let mut strl = chunk(b"strh", &strh);
    strl.extend(chunk(b"strf", &strf));
    let mut hdrl = chunk(b"avih", &avih);
    hdrl.extend(list(b"strl", &strl));
    let mut movi = b"movi".to_vec();
    let mut idx = Vec::new();
    for p in packets {
        idx.extend(b"00dc");
        idx.extend(le(&[0x10, movi.len() as u32 - 4, p.len() as u32]));
        movi.extend(chunk(b"00dc", p));
    }
    let mut body = b"AVI ".to_vec();
    body.extend(list(b"hdrl", &hdrl));
    body.extend(chunk(b"LIST", &movi));
    body.extend(chunk(b"idx1", &idx));
    let mut file = b"RIFF".to_vec();
    file.extend((body.len() as u32).to_le_bytes());
    file.extend(body);
    std::fs::write(path, file).unwrap();
}

#[test]
fn avi1_packets_without_dht_use_the_default_tables() {
    if !oracle() {
        return;
    }
    let work = scratch("nodht");
    for (pix, size) in [
        ("yuvj420p", "64x48"),
        ("yuvj422p", "48x32"),
        ("yuvj444p", "40x40"),
        ("gray", "33x17"),
    ] {
        let src = work.join(format!("{pix}.mkv"));
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc2=size={size}:rate=10"),
            "-frames:v",
            "4",
            "-c:v",
            "mjpeg",
            "-pix_fmt",
            pix,
            "-huffman",
            "default",
            src.to_str().unwrap(),
        ]);
        let o = open(&src);
        let stripped: Vec<Vec<u8>> = o.packets.iter().map(|p| strip_dht(p)).collect();
        assert!(
            stripped
                .iter()
                .zip(&o.packets)
                .all(|(s, p)| s.len() < p.len())
        );
        let (w, h) = size.split_once('x').unwrap();
        let avi = work.join(format!("{pix}.avi"));
        write_avi(&avi, w.parse().unwrap(), h.parse().unwrap(), &stripped);
        let theirs = oracle_decode(&avi, &[]);
        let with_dht = decode_all(&o, Interlace::TopFieldFirst);
        let no_dht = Opened {
            params: o.params.clone(),
            field_order: None,
            packets: stripped,
        };
        let ours = decode_all(&no_dht, Interlace::TopFieldFirst);
        assert_equal(&format!("{pix} no DHT vs ffmpeg"), &ours, &theirs);
        assert_equal(&format!("{pix} no DHT vs with DHT"), &ours, &{
            let mut v = Vec::new();
            for f in &with_dht {
                v.extend(frame_bytes(f));
            }
            v
        });
    }
}

#[test]
fn two_field_packets_weave_like_ffmpeg() {
    if !oracle() {
        return;
    }
    let work = scratch("fields");
    // ffmpeg's mjpeg encoder refuses `-flags +ildct` ("interlacing not supported by codec"),
    // so the interlaced material is built from separated fields, one JPEG per field.
    for (pix, size) in [
        ("yuvj420p", "64x48"),
        ("yuvj422p", "64x32"),
        ("yuvj444p", "48x48"),
        ("yuvj420p", "34x26"),
    ] {
        let fields = work.join(format!("{pix}-{size}-fields.mkv"));
        let mut args = vec![
            "-f",
            "lavfi",
            "-i",
            leak(format!("testsrc2=size={size}:rate=5")),
            "-frames:v",
            "6",
            "-vf",
            "separatefields",
            "-c:v",
            "mjpeg",
            "-pix_fmt",
            pix,
        ];
        args.push(fields.to_str().unwrap());
        ffmpeg(&args);
        let f = open(&fields);
        assert_eq!(f.packets.len(), 6);
        let packets: Vec<Vec<u8>> = f
            .packets
            .chunks(2)
            .map(|c| [c[0].clone(), c[1].clone()].concat())
            .collect();
        let (w, h) = size.split_once('x').unwrap();
        let (w, h): (u32, u32) = (w.parse().unwrap(), h.parse().unwrap());

        // AVI (MJPG tag): ffmpeg takes the first image as the bottom field.
        let avi = work.join(format!("{pix}-{size}.avi"));
        write_avi(&avi, w, h, &packets);
        let mut params = f.params.clone();
        if let MediaParameters::Video(v) = &mut params.media {
            v.width = w;
            v.height = h;
        }
        let o = Opened {
            params: params.clone(),
            field_order: None,
            packets: packets.clone(),
        };
        let ours = decode_all(&o, Interlace::BottomFieldFirst);
        assert!(
            ours.iter()
                .all(|f| f.interlace == Interlace::BottomFieldFirst && f.height == h)
        );
        assert_equal(&format!("{pix} avi"), &ours, &oracle_decode(&avi, &[]));

        // MOV (mjpa) and Matroska: first image is the top field.
        for (ext, tag) in [("mov", Some("mjpa")), ("mkv", None)] {
            let out = work.join(format!("{pix}-{size}-w.{ext}"));
            let mut a = vec!["-i", avi.to_str().unwrap(), "-c", "copy"];
            if let Some(t) = tag {
                a.extend(["-tag:v", t]);
            }
            a.push(out.to_str().unwrap());
            ffmpeg(&a);
            let o = open(&out);
            assert_eq!(o.packets.len(), 3);
            let ours = decode_all(&o, o.field_order.unwrap_or(Interlace::TopFieldFirst));
            assert!(ours.iter().all(|f| f.interlace == Interlace::TopFieldFirst));
            assert_equal(&format!("{pix} {ext}"), &ours, &oracle_decode(&out, &[]));
        }
    }
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

#[test]
fn unmappable_files_are_refused_by_name() {
    let data =
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/odd444.jpg")).unwrap();
    // 12-bit precision: the SOF precision byte.
    let sof = data
        .windows(2)
        .position(|w| w == [0xff, 0xc0])
        .or_else(|| data.windows(2).position(|w| w == [0xff, 0xc1]))
        .unwrap();
    let mut twelve = data.clone();
    twelve[sof + 4] = 12;
    let err = ec_image::jpeg::decode_yuv(&twelve, Limits::default())
        .unwrap_err()
        .to_string();
    assert!(err.contains("12-bit"), "{err}");
    // 4:4:0: luma 1x2, chroma 1x1.
    let mut s440 = data.clone();
    s440[sof + 4 + 6 + 1] = 0x12;
    let err = ec_image::jpeg::decode_yuv(&s440, Limits::default())
        .unwrap_err()
        .to_string();
    assert!(err.contains("sampling"), "{err}");
    // 4:1:1: luma 4x1.
    let mut s411 = data.clone();
    s411[sof + 4 + 6 + 1] = 0x41;
    let err = ec_image::jpeg::decode_yuv(&s411, Limits::default())
        .unwrap_err()
        .to_string();
    assert!(err.contains("sampling"), "{err}");
    // RGB-coded: an Adobe APP14 segment with transform 0.
    let mut rgb = data[..2].to_vec();
    rgb.extend([0xff, 0xee, 0, 14]);
    rgb.extend(b"Adobe");
    rgb.extend([0, 100, 0, 0, 0, 0, 0]);
    rgb.extend(&data[2..]);
    let err = ec_image::jpeg::decode_yuv(&rgb, Limits::default())
        .unwrap_err()
        .to_string();
    assert!(err.contains("RGB"), "{err}");
}

// ---- MJPEG-B (QuickTime `mjpb`) -------------------------------------------------------------
// ffmpeg has no MJPEG-B encoder, so baseline JPEGs are rewrapped into the MJPEG-B field layout
// and written into a hand-built .mov; ffmpeg's `mjpegb` decoder is the oracle.

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

/// One baseline JPEG as an MJPEG-B field. `second` is the second-field offset to state.
fn jpeg_to_mjpegb_field(jpeg: &[u8], second: u32) -> Vec<u8> {
    let (mut dqt, mut dht, mut sof, mut sos) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut i = 2;
    let mut scan_at = 0;
    while i + 4 <= jpeg.len() {
        assert_eq!(jpeg[i], 0xff);
        let m = jpeg[i + 1];
        let len = usize::from(u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]));
        let body = &jpeg[i + 4..i + 2 + len];
        match m {
            0xdb => dqt.extend_from_slice(body),
            0xc4 => dht.extend_from_slice(body),
            0xc0 => sof = jpeg[i + 2..i + 2 + len].to_vec(),
            0xda => {
                sos = jpeg[i + 2..i + 2 + len].to_vec();
                scan_at = i + 2 + len;
                break;
            }
            _ => {}
        }
        i += 2 + len;
    }
    assert!(!sof.is_empty() && scan_at > 0, "baseline JPEG expected");
    let scan_end = jpeg.len() - 2; // drop EOI
    let wrap = |body: &[u8]| {
        let mut v = ((body.len() + 2) as u16).to_be_bytes().to_vec();
        v.extend_from_slice(body);
        v
    };
    let (dqt, dht) = (wrap(&dqt), wrap(&dht));
    let mut off = 40u32;
    let mut offs = [0u32; 4];
    for (k, b) in [&dqt, &dht, &sof, &sos].into_iter().enumerate() {
        offs[k] = off;
        off += b.len() as u32;
    }
    let sod = off;
    let size = sod + {
        let s = &jpeg[scan_at..scan_end];
        (s.len() - s.windows(2).filter(|w| w == &[0xff, 0]).count()) as u32
    };
    let mut out = Vec::new();
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(b"mjpg");
    out.extend_from_slice(&be32(size));
    out.extend_from_slice(&be32(size));
    out.extend_from_slice(&be32(second));
    for o in offs {
        out.extend_from_slice(&be32(o));
    }
    out.extend_from_slice(&be32(sod));
    for b in [&dqt, &dht, &sof, &sos] {
        out.extend_from_slice(b);
    }
    // MJPEG-B scan data is not byte-stuffed: FF 00 becomes FF.
    let scan = &jpeg[scan_at..scan_end];
    let mut k = 0;
    while k < scan.len() {
        out.push(scan[k]);
        k += if scan[k] == 0xff && scan.get(k + 1) == Some(&0) {
            2
        } else {
            1
        };
    }
    out
}

fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(kind);
    v.extend_from_slice(body);
    v
}

/// A one-track .mov with `mjpb` samples of the given sizes, `fiel` when `fields` is Some.
fn write_mjpb_mov(path: &Path, w: u16, h: u16, packets: &[Vec<u8>], fields: Option<(u8, u8)>) {
    let mut mdat = Vec::new();
    for p in packets {
        mdat.extend_from_slice(p);
    }
    let ftyp = bx(b"ftyp", b"qt  \0\0\0\0qt  ");
    let data_at = (ftyp.len() + 8) as u32;
    let n = packets.len() as u32;
    let mut mvhd = vec![0u8; 100];
    mvhd[12..16].copy_from_slice(&be32(25));
    mvhd[16..20].copy_from_slice(&be32(n));
    mvhd[20..24].copy_from_slice(&be32(0x10000));
    mvhd[24..26].copy_from_slice(&[1, 0]);
    for (i, v) in [0x10000u32, 0, 0, 0, 0x10000, 0, 0, 0, 0x40000000]
        .into_iter()
        .enumerate()
    {
        mvhd[36 + 4 * i..40 + 4 * i].copy_from_slice(&be32(v));
    }
    mvhd[96..100].copy_from_slice(&be32(2));
    let mut tkhd = vec![0u8; 84];
    tkhd[3] = 7;
    tkhd[12..16].copy_from_slice(&be32(1));
    tkhd[20..24].copy_from_slice(&be32(n));
    for (i, v) in [0x10000u32, 0, 0, 0, 0x10000, 0, 0, 0, 0x40000000]
        .into_iter()
        .enumerate()
    {
        tkhd[40 + 4 * i..44 + 4 * i].copy_from_slice(&be32(v));
    }
    tkhd[76..80].copy_from_slice(&be32(u32::from(w) << 16));
    tkhd[80..84].copy_from_slice(&be32(u32::from(h) << 16));
    let mut mdhd = vec![0u8; 24];
    mdhd[12..16].copy_from_slice(&be32(25));
    mdhd[16..20].copy_from_slice(&be32(n));
    let mut hdlr = vec![0u8; 24];
    hdlr[8..12].copy_from_slice(b"vide");
    hdlr.push(0);
    let mut entry = vec![0u8; 78];
    entry[6..8].copy_from_slice(&[0, 1]);
    entry[24..26].copy_from_slice(&w.to_be_bytes());
    entry[26..28].copy_from_slice(&h.to_be_bytes());
    entry[28..32].copy_from_slice(&be32(0x480000));
    entry[32..36].copy_from_slice(&be32(0x480000));
    entry[40..42].copy_from_slice(&[0, 1]);
    entry[74..76].copy_from_slice(&[0, 24]);
    entry[76..78].copy_from_slice(&[0xff, 0xff]);
    if let Some((count, detail)) = fields {
        entry.extend_from_slice(&bx(b"fiel", &[count, detail]));
    }
    let mut stsd = be32(0).to_vec();
    stsd.extend_from_slice(&be32(1));
    stsd.extend_from_slice(&bx(b"mjpb", &entry));
    let mut stts = vec![0u8; 4];
    stts.extend_from_slice(&be32(1));
    stts.extend_from_slice(&be32(n));
    stts.extend_from_slice(&be32(1));
    let mut stsc = vec![0u8; 4];
    stsc.extend_from_slice(&be32(1));
    stsc.extend_from_slice(&[be32(1), be32(n), be32(1)].concat());
    let mut stsz = vec![0u8; 4];
    stsz.extend_from_slice(&be32(0));
    stsz.extend_from_slice(&be32(n));
    for p in packets {
        stsz.extend_from_slice(&be32(p.len() as u32));
    }
    let mut stco = vec![0u8; 4];
    stco.extend_from_slice(&be32(1));
    stco.extend_from_slice(&be32(data_at));
    let stbl = bx(
        b"stbl",
        &[
            bx(b"stsd", &stsd),
            bx(b"stts", &stts),
            bx(b"stsc", &stsc),
            bx(b"stsz", &stsz),
            bx(b"stco", &stco),
        ]
        .concat(),
    );
    let minf = bx(
        b"minf",
        &[bx(b"vmhd", &[0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]), stbl].concat(),
    );
    let mdia = bx(
        b"mdia",
        &[bx(b"mdhd", &mdhd), bx(b"hdlr", &hdlr), minf].concat(),
    );
    let trak = bx(b"trak", &[bx(b"tkhd", &tkhd), mdia].concat());
    let moov = bx(b"moov", &[bx(b"mvhd", &mvhd), trak].concat());
    let mut file = ftyp;
    file.extend_from_slice(&bx(b"mdat", &mdat));
    file.extend_from_slice(&moov);
    std::fs::write(path, file).unwrap();
}

fn mjpegb_oracle(path: &Path) -> Vec<u8> {
    let out = path.with_extension("oracle.yuv");
    ffmpeg(&[
        "-c:v",
        "mjpegb",
        "-i",
        path.to_str().unwrap(),
        "-f",
        "rawvideo",
        out.to_str().unwrap(),
    ]);
    std::fs::read(out).unwrap()
}

fn mjpeg_stream_packets(path: &Path) -> Vec<Vec<u8>> {
    let o = open(path);
    o.packets
}

#[test]
fn mjpegb_matches_ffmpeg() {
    if !oracle() {
        return;
    }
    let work = scratch("mjpegb");
    // Progressive: one field per packet, all three layouts, odd sizes.
    for (pix, size, w, h, q) in [
        ("yuvj420p", "64x48", 64u16, 48u16, "3"),
        ("yuvj422p", "50x38", 50, 38, "9"),
        ("yuvj444p", "51x37", 51, 37, "20"),
        ("yuvj420p", "160x96", 160, 96, "5"),
    ] {
        let src = work.join(format!("{pix}-{size}.mov"));
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc2=size={size}:rate=10"),
            "-frames:v",
            "5",
            "-vf",
            &format!("noise=alls=18:allf=t,format={pix}"),
            "-c:v",
            "mjpeg",
            "-pix_fmt",
            pix,
            "-q:v",
            q,
            "-huffman",
            "optimal",
            src.to_str().unwrap(),
        ]);
        let packets: Vec<Vec<u8>> = mjpeg_stream_packets(&src)
            .iter()
            .map(|p| jpeg_to_mjpegb_field(p, 0))
            .collect();
        let path = work.join(format!("{pix}-{size}-b.mov"));
        write_mjpb_mov(&path, w, h, &packets, None);
        let o = open(&path);
        let frames = decode_all(&o, Interlace::TopFieldFirst);
        assert_eq!(frames.len(), 5);
        assert_equal(
            &format!("mjpb {pix} {size}"),
            &frames,
            &mjpegb_oracle(&path),
        );
    }
}

#[test]
fn mjpegb_two_fields_match_ffmpeg() {
    if !oracle() {
        return;
    }
    let work = scratch("mjpegb2");
    for (pix, size, w, h) in [
        ("yuvj420p", "64x48", 64u16, 48u16),
        ("yuvj422p", "48x32", 48, 32),
    ] {
        let fields = work.join(format!("{pix}-{size}-fields.mkv"));
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc2=size={size}:rate=5"),
            "-frames:v",
            "6",
            "-vf",
            "separatefields",
            "-c:v",
            "mjpeg",
            "-pix_fmt",
            pix,
            fields.to_str().unwrap(),
        ]);
        let f = mjpeg_stream_packets(&fields);
        assert_eq!(f.len(), 6);
        let packets: Vec<Vec<u8>> = f
            .chunks(2)
            .map(|c| {
                let a = jpeg_to_mjpegb_field(&c[0], 0);
                let b = jpeg_to_mjpegb_field(&c[1], 0);
                let mut a = jpeg_to_mjpegb_field(&c[0], a.len() as u32);
                a.extend_from_slice(&b);
                a
            })
            .collect();
        let want = {
            let path = work.join(format!("{pix}-{size}-b.mov"));
            write_mjpb_mov(&path, w, h, &packets, Some((2, 1)));
            (path.clone(), mjpegb_oracle(&path))
        };
        let o = open(&want.0);
        let top = decode_all(&o, Interlace::TopFieldFirst);
        let bottom = decode_all(&o, Interlace::BottomFieldFirst);
        let eq = |fs: &[VideoFrame]| fs.iter().flat_map(frame_bytes).collect::<Vec<u8>>() == want.1;
        assert!(
            eq(&top),
            "mjpb {pix} {size}: top-first weave differs from ffmpeg"
        );
        eprintln!("mjpb {pix} {size}: top={} bottom={}", eq(&top), eq(&bottom));
    }
}
