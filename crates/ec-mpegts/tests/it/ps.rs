//! Program streams, system streams and raw video: packet-for-packet parity with
//! ffmpeg's demux on ffmpeg-made files (bytes, sizes, timestamps, key flags),
//! duration, seek exactness, SCR resets, PTS wraps and damaged files.
//!
//! Fixtures are generated at test time from synthetic sources; the tests skip when
//! ffmpeg is missing and fail instead under `EC_REQUIRE_FIXTURES`.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use crate::common::*;
use ec_core::registry::{Demuxer, SeekMode};
use ec_core::{CodecId, TimeBase, Timestamp};
use ec_mpegts::{PsDemuxer, PsKind};

type D = PsDemuxer<BufReader<File>>;

fn open_ps(path: &Path) -> D {
    PsDemuxer::new(BufReader::new(File::open(path).unwrap())).unwrap()
}

fn read_ps(d: &mut D) -> Vec<Pkt> {
    let mut v = Vec::new();
    while let Ok(p) = d.next_packet() {
        v.push(Pkt {
            stream: p.stream,
            pts: p.pts,
            dts: p.dts,
            key: p.flags.keyframe,
            off: d.last_packet_offset(),
            data: p.data.to_vec(),
        });
    }
    v
}

/// ffprobe's stream `id` for our (stream_id, substream) key.
fn ffid(key: u16) -> u16 {
    match (key >> 8) as u8 {
        0xBD => key & 0xFF,
        id => 0x100 | u16::from(id),
    }
}

const V2: &[&str] = &[
    "-c:v",
    "mpeg2video",
    "-g",
    "12",
    "-bf",
    "2",
    "-b:v",
    "1500k",
];

fn src(extra_audio: &[&str]) -> Vec<&'static str> {
    let mut a: Vec<&str> = VIDEO_SRC.to_vec();
    a.extend_from_slice(AUDIO_SRC);
    let _ = extra_audio;
    a
}

fn mkps(name: &str, video: &[&str], audio: &[&str], mux: &[&str]) -> Option<PathBuf> {
    let mut a = src(audio);
    a.extend_from_slice(video);
    a.extend_from_slice(audio);
    a.extend_from_slice(mux);
    fixture(name, &a)
}

/// Parity of every exposed stream with ffprobe/ffmpeg. `raw` files (no container
/// timestamps) compare sizes, flags and bytes only.
fn parity(path: &Path, slack: i64) {
    let mut d = open_ps(path);
    let raw = d.kind() == PsKind::VideoElementary;
    let pkts = read_ps(&mut d);
    let mut rows = ffprobe_rows(path);
    if raw {
        // A raw stream has no ids: its one stream is stream 0.
        rows.insert(ffid(d.stream_key(0).unwrap()), (0, raw_rows(path)));
    }
    assert!(!d.streams().is_empty(), "{path:?}: no streams");
    for s in d.streams() {
        let key = d.stream_key(s.index).unwrap();
        let (si, rows) = rows
            .get(&ffid(key))
            .unwrap_or_else(|| panic!("{path:?}: ffprobe lacks id {:#x}", ffid(key)));
        let mine: Vec<&Pkt> = pkts.iter().filter(|p| p.stream == s.index).collect();
        let name = s.params.codec.name();
        let pcm = matches!(s.params.codec, CodecId::PcmS16Be | CodecId::PcmS24Le);
        if !pcm {
            assert_eq!(mine.len(), rows.len(), "{path:?} {name} packet count");
        }
        if !pcm && !raw {
            let md0 = mine[0].dts.or(mine[0].pts).unwrap();
            let rd0 = rows[0].dts.or(rows[0].pts).unwrap();
            for (i, (m, r)) in mine.iter().zip(rows).enumerate() {
                assert_eq!(m.data.len(), r.size, "{path:?} {name} #{i} size");
                // ffmpeg leaves the pts of a picture without a timestamp unknown; the
                // demuxer derives it from the temporal reference.
                let near = |a: Option<i64>, b: Option<i64>| match (a, b) {
                    (Some(a), Some(b)) => (a - b).abs() <= if m.dts.is_none() { slack } else { 0 },
                    (Some(_), None) => true,
                    (a, b) => a == b,
                };
                let (mp, rp) = (m.pts.map(|v| v - md0), r.pts.map(|v| v - rd0));
                assert!(near(mp, rp), "{path:?} {name} #{i} pts {mp:?} vs {rp:?}");
                let (mdt, rdt) = (
                    m.dts.or(m.pts).map(|v| v - md0),
                    r.dts.or(r.pts).map(|v| v - rd0),
                );
                assert!(
                    near(mdt, rdt),
                    "{path:?} {name} #{i} dts {mdt:?} vs {rdt:?}"
                );
                assert_eq!(m.key, r.key, "{path:?} {name} #{i} keyframe");
            }
        } else if raw {
            for (i, (m, r)) in mine.iter().zip(rows).enumerate() {
                assert_eq!(m.data.len(), r.size, "{path:?} {name} #{i} size");
                assert_eq!(m.key, r.key, "{path:?} {name} #{i} keyframe");
            }
        }
        let bytes: Vec<u8> = mine.iter().flat_map(|p| p.data.iter().copied()).collect();
        let theirs = match s.params.codec {
            CodecId::PcmS16Be => ffmpeg_pcm_s16be(path, *si),
            CodecId::PcmS24Le => ffmpeg_pcm(path, *si, "s24le"),
            _ => ffmpeg_bytes(path, *si),
        };
        eprintln!(
            "{} {name}: {} packets, {} bytes",
            path.file_name().unwrap().to_string_lossy(),
            mine.len(),
            bytes.len()
        );
        // DVD LPCM: ffmpeg rounds the last packet's partial block up.
        let theirs = if s.params.codec == CodecId::PcmS24Le
            && theirs.len() >= bytes.len()
            && theirs.len() - bytes.len() <= 12
        {
            theirs[..bytes.len()].to_vec()
        } else {
            theirs
        };
        assert!(
            bytes == theirs,
            "{path:?} {name}: bytes differ ({} vs {})",
            bytes.len(),
            theirs.len()
        );
    }
}

fn raw_rows(path: &Path) -> Vec<Row> {
    let o = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "packet=size,flags",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(|l| {
            let f: Vec<&str> = l.split(',').collect();
            Row {
                pid: 0,
                pts: None,
                dts: None,
                dur: None,
                size: f[0].parse().unwrap(),
                key: f[1].starts_with('K'),
            }
        })
        .collect()
}

fn ffmpeg_pcm(path: &Path, index: u32, fmt: &str) -> Vec<u8> {
    std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", &format!("0:{index}"), "-f", fmt, "-"])
        .output()
        .unwrap()
        .stdout
}

/// Duration against ffprobe's packet extents, within one video frame.
fn duration_ok(path: &Path) {
    let d = open_ps(path);
    let got = d.duration_secs().expect("duration");
    let rows = ffprobe_rows(path);
    let mut want = 0f64;
    for s in d.streams() {
        let Some((_, r)) = rows.get(&ffid(d.stream_key(s.index).unwrap())) else {
            continue;
        };
        let first = r.iter().filter_map(|x| x.pts).min();
        let end = r
            .iter()
            .filter_map(|x| x.pts.map(|p| p + x.dur.unwrap_or(0)))
            .max();
        if let (Some(f), Some(e)) = (first, end) {
            want = want.max((e - f) as f64 / 90_000.0);
        }
    }
    if d.kind() == PsKind::VideoElementary {
        return;
    }
    assert!(
        (got - want).abs() <= 0.045,
        "{path:?}: duration {got} vs {want}"
    );
}

/// Seek (every mode, several targets) then read equals the sequential read from the landing offset.
fn seek_exact(path: &Path) {
    let mut d = open_ps(path);
    let all = read_ps(&mut d);
    let vid = d
        .streams()
        .iter()
        .find(|s| s.params.video().is_some())
        .map(|s| s.index);
    let base = d
        .streams()
        .iter()
        .filter_map(|s| s.start_time)
        .min()
        .unwrap();
    let dur = d.duration_secs().unwrap_or(3.0);
    let tb = TimeBase::new(1, 90_000);
    for frac in [0.0, 0.1, 0.33, 0.5, 0.77, 0.97, 1.5] {
        let target = base + (frac * dur * 90_000.0) as i64;
        for mode in [SeekMode::SyncBefore, SeekMode::Exact, SeekMode::SyncAfter] {
            let mut d = open_ps(path);
            let sk = vid.unwrap_or(0);
            match d.seek(sk, Timestamp::new(target, tb), mode) {
                Ok(()) => {}
                Err(ec_core::Error::Eof) if mode == SeekMode::SyncAfter => continue,
                Err(e) => panic!("{path:?} seek: {e}"),
            }
            let landing = d.last_packet_offset();
            let got = read_ps(&mut d);
            let mut want: Vec<&Pkt> = all.iter().filter(|p| p.off >= landing).collect();
            // Pictures that share the landing packet but precede the keyframe are dropped.
            if let Some(v) = vid
                && let Some(f) = got.iter().find(|p| p.stream == v)
            {
                let at = want
                    .iter()
                    .position(|p| {
                        p.stream == v && p.pts == f.pts && p.dts == f.dts && p.data == f.data
                    })
                    .expect("first video packet after the seek is in the sequential read");
                let mut i = 0;
                want.retain(|p| {
                    let drop = i < at && p.stream == v && p.off == landing;
                    i += 1;
                    !drop
                });
            }
            assert_eq!(
                got.len(),
                want.len(),
                "{path:?} seek {frac} {mode:?}: count"
            );
            for (g, w) in got.iter().zip(&want) {
                assert!(
                    g.stream == w.stream
                        && g.pts == w.pts
                        && g.dts == w.dts
                        && g.key == w.key
                        && g.data == w.data,
                    "{path:?} seek {frac} {mode:?}: packet differs at off {}: s{} pts {:?}/{:?} dts {:?}/{:?} key {}/{} len {}/{}",
                    w.off,
                    w.stream,
                    g.pts,
                    w.pts,
                    g.dts,
                    w.dts,
                    g.key,
                    w.key,
                    g.data.len(),
                    w.data.len()
                );
            }
            if let (Some(v), SeekMode::SyncBefore) = (vid, mode) {
                let first = got.iter().find(|p| p.stream == v).unwrap();
                let key_before = all
                    .iter()
                    .any(|p| p.stream == v && p.key && p.pts.is_some_and(|t| t <= target));
                assert!(
                    first.key || !key_before,
                    "{path:?} seek {frac}: lands on a keyframe"
                );
                let fp = first.pts.unwrap();
                assert!(
                    fp <= target.max(base) || frac == 0.0 || !key_before,
                    "{path:?} seek {frac}: {fp} > {target}"
                );
                let better = all.iter().any(|p| {
                    p.stream == v && p.key && p.pts.is_some_and(|x| x > fp && x <= target)
                });
                assert!(!better, "{path:?} seek {frac}: a closer keyframe exists");
            }
        }
    }
}

/// The fixture cases that get the full check set (parity, duration, seek
/// exactness). One test over a table: every case runs and all failures are named.
#[test]
fn ps_parity_cases() {
    type Case = (&'static str, fn() -> Option<std::path::PathBuf>);
    const CASES: &[Case] = &[
        ("mpeg1_system_mp2", || {
            mkps(
                "m1.mpg",
                &["-c:v", "mpeg1video", "-g", "12", "-bf", "2"],
                &["-c:a", "mp2"],
                &["-f", "mpeg"],
            )
        }),
        ("vob_mpeg2_ac3", || {
            mkps("v_ac3.vob", V2, &["-c:a", "ac3"], &["-f", "vob"])
        }),
        ("vob_mpeg2_mp2", || {
            mkps("v_mp2.vob", V2, &["-c:a", "mp2"], &["-f", "vob"])
        }),
        ("mpeg2_ps_mpeg_muxer", || {
            mkps("m2.mpg", V2, &["-c:a", "mp2"], &["-f", "mpeg"])
        }),
        ("dvd_with_nav_packets", || {
            mkps(
                "dvd.vob",
                &[
                    "-vf",
                    "scale=720:576",
                    "-c:v",
                    "mpeg2video",
                    "-g",
                    "12",
                    "-bf",
                    "2",
                    "-b:v",
                    "3000k",
                ],
                &["-c:a", "ac3", "-ar", "48000"],
                &["-f", "dvd"],
            )
        }),
        ("interlaced_mpeg2", || {
            mkps(
                "ilace.vob",
                &[
                    "-c:v",
                    "mpeg2video",
                    "-g",
                    "12",
                    "-bf",
                    "2",
                    "-flags",
                    "+ildct+ilme",
                    "-top",
                    "1",
                ],
                &["-c:a", "ac3"],
                &["-f", "vob"],
            )
        }),
        ("pcm_dvd_16", || {
            mkps("lpcm16.vob", V2, &["-c:a", "pcm_dvd"], &["-f", "vob"])
        }),
        ("raw_m2v", || {
            fixture("a.m2v", &[VIDEO_SRC, V2, &["-f", "mpeg2video"]].concat())
        }),
        ("pts_wrap", || {
            mkps(
                "wrap.vob",
                V2,
                &["-c:a", "ac3"],
                &["-output_ts_offset", "95442", "-f", "vob"],
            )
        }),
        ("h264_mp3_mpeg_muxer", || {
            mkps(
                "h264.mpg",
                &[
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-g",
                    "25",
                    "-bf",
                    "2",
                ],
                &["-c:a", "mp3"],
                &["-f", "mpeg"],
            )
        }),
        ("pcm_dvd_96k_16", || {
            mkps(
                "lpcm96.vob",
                V2,
                &["-c:a", "pcm_dvd", "-ar", "96000"],
                &["-f", "vob"],
            )
        }),
        ("mpeg1_video_only", || {
            fixture(
                "v1.mpg",
                &[
                    VIDEO_SRC,
                    &["-c:v", "mpeg1video", "-g", "9", "-bf", "2", "-f", "mpeg"],
                ]
                .concat(),
            )
        }),
        ("raw_m2v_without_b_frames", || {
            fixture(
                "nb.m2v",
                &[
                    VIDEO_SRC,
                    &[
                        "-c:v",
                        "mpeg2video",
                        "-g",
                        "10",
                        "-bf",
                        "0",
                        "-f",
                        "mpeg2video",
                    ],
                ]
                .concat(),
            )
        }),
        ("raw_mpeg1_video", || {
            fixture(
                "a1.m1v",
                &[
                    VIDEO_SRC,
                    &[
                        "-c:v",
                        "mpeg1video",
                        "-g",
                        "9",
                        "-bf",
                        "2",
                        "-f",
                        "mpeg1video",
                    ],
                ]
                .concat(),
            )
        }),
    ];
    let mut failures = Vec::new();
    for &(name, make) in CASES {
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let Some(p) = make() else { return };
            parity(&p, 0);
            duration_ok(&p);
            seek_exact(&p);
        }));
        if ran.is_err() {
            failures.push(name);
        }
    }
    assert!(failures.is_empty(), "failed cases: {failures:?}");
}

// 20 and 24 bit DVD LPCM groups straddle ffmpeg's packet cuts, so a read that starts
// mid-stream cannot know the phase: no seek-exactness check for it.
#[test]
fn pcm_dvd_24() {
    let Some(p) = mkps(
        "lpcm24.vob",
        V2,
        &["-c:a", "pcm_dvd", "-sample_fmt", "s32"],
        &["-f", "vob"],
    ) else {
        return;
    };
    parity(&p, 0);
    duration_ok(&p);
}

#[test]
fn dts_is_refused_by_name() {
    let Some(p) = mkps(
        "dts.vob",
        V2,
        &["-c:a", "dca", "-strict", "-2"],
        &["-f", "vob"],
    ) else {
        return;
    };
    let d = open_ps(&p);
    assert_eq!(d.streams().len(), 1);
    let t = d.tracks();
    let dts = t
        .iter()
        .find(|t| t.stream.is_none())
        .expect("a refused track");
    assert!(
        dts.refusal.as_deref().unwrap().starts_with("dts"),
        "{dts:?}"
    );
}

// ---------------------------------------------------------------------------
// Committed fixtures: SCR resets, damaged files, injected streams
// ---------------------------------------------------------------------------

fn committed(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data")
            .join(name),
    )
    .unwrap()
}

/// A file name no other call in this process uses: tests run as threads of one
/// process, so the pid alone does not separate them.
fn unique_name(name: &str) -> String {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{}-{n}-{name}", std::process::id())
}

fn tmpfile(name: &str, bytes: &[u8]) -> PathBuf {
    let p = fixture_dir().join(unique_name(name));
    std::fs::write(&p, bytes).unwrap();
    p
}

fn open_bytes(b: Vec<u8>) -> ec_core::Result<PsDemuxer<std::io::Cursor<Vec<u8>>>> {
    PsDemuxer::new(std::io::Cursor::new(b))
}

fn per_stream(p: &[Pkt]) -> Vec<Vec<&Pkt>> {
    let n = p
        .iter()
        .map(|x| x.stream)
        .max()
        .map_or(0, |m| m as usize + 1);
    (0..n)
        .map(|s| p.iter().filter(|x| x.stream as usize == s).collect())
        .collect()
}

/// `copies` of a file back to back: the SCR (and the PTS) restart in each.
fn check_concat(bytes: &[u8], copies: usize, walk_limit: Option<u64>) {
    let cat: Vec<u8> = (0..copies).flat_map(|_| bytes.iter().copied()).collect();
    let path = tmpfile("concat.vob", &cat);
    let open = |p: &Path| {
        let f = BufReader::new(File::open(p).unwrap());
        match walk_limit {
            Some(l) => PsDemuxer::with_walk_limit(f, l).unwrap(),
            None => PsDemuxer::new(f).unwrap(),
        }
    };
    let single = {
        let mut d = open_bytes(bytes.to_vec()).unwrap();
        let mut v = Vec::new();
        while let Ok(p) = d.next_packet() {
            v.push((p.stream, p.pts, p.dts, p.data.to_vec()));
        }
        v
    };
    let mut d = open(&path);
    assert_eq!(d.epoch_count(), copies, "SCR resets found at open");
    let all = read_ps(&mut d);
    assert_eq!(d.stats().discontinuities, copies as u64 - 1);
    assert_eq!(all.len(), single.len() * copies);
    let ps = per_stream(&all);
    let mut shift = None;
    for (s, list) in ps.iter().enumerate() {
        let k = list.len() / copies;
        assert!(k > 0);
        for c in 1..copies {
            for i in 0..k {
                let (a, b) = (list[i + (c - 1) * k], list[i + c * k]);
                assert_eq!(a.data, b.data, "stream {s} copy {c} #{i}");
                let dp = b.pts.unwrap() - a.pts.unwrap();
                let sh = *shift.get_or_insert(dp);
                assert_eq!(
                    dp, sh,
                    "stream {s} copy {c} #{i}: one shift for every stream"
                );
                if let (Some(x), Some(y)) = (a.dts, b.dts) {
                    assert_eq!(y - x, sh);
                }
            }
        }
        // Decode (or presentation) time never goes backwards across the joins.
        let t: Vec<i64> = list.iter().map(|p| p.dts.or(p.pts).unwrap()).collect();
        assert!(
            t.windows(2).all(|w| w[1] >= w[0]),
            "stream {s}: time steps back"
        );
    }
    // The shift is about one copy's length of SCR.
    let sh = shift.unwrap();
    let first_dur = open_bytes(bytes.to_vec()).unwrap().duration_secs().unwrap();
    assert!(
        sh > 0 && (sh as f64 / 90_000.0) < first_dur + 1.0,
        "shift {sh} vs {first_dur}"
    );
    let dur = d.duration_secs().unwrap();
    assert!(
        dur > first_dur * copies as f64 * 0.8 && dur < (first_dur + 1.0) * copies as f64,
        "{dur}"
    );
    seek_exact(&path);
}

#[test]
fn concatenated_vobs_keep_time_monotonic() {
    let b = committed("ps_mpeg2_ac3.vob");
    check_concat(&b, 3, None);
    // The same through the sampling check used for big files.
    check_concat(&b, 3, Some(1));
    check_concat(&committed("ps_mpeg1_mp2.mpg"), 2, None);
}

#[test]
fn concatenated_dvd_files_with_nav_packets() {
    let Some(p) = mkps(
        "dvd2.vob",
        &[
            "-vf",
            "scale=720:576",
            "-c:v",
            "mpeg2video",
            "-g",
            "12",
            "-bf",
            "2",
            "-b:v",
            "3000k",
        ],
        &["-c:a", "ac3", "-ar", "48000"],
        &["-f", "dvd"],
    ) else {
        return;
    };
    check_concat(&std::fs::read(p).unwrap(), 2, None);
}

#[test]
fn starts_mid_gop() {
    let Some(p) = mkps("v_ac3.vob", V2, &["-c:a", "ac3"], &["-f", "vob"]) else {
        return;
    };
    let orig_bytes = std::fs::read(&p).unwrap();
    let mut o = open_ps(&p);
    let orig = read_ps(&mut o);
    // Start at a pack boundary a few GOPs in and not on a keyframe.
    let cut = 2048 * 90;
    let path = tmpfile("midgop.vob", &orig_bytes[cut..]);
    let mut d = open_ps(&path);
    assert_eq!(d.streams().len(), 2);
    assert_eq!(d.streams()[0].params.codec, CodecId::Mpeg2Video);
    let got = read_ps(&mut d);
    let (gv, ov) = (
        got.iter().filter(|p| p.stream == 0).collect::<Vec<_>>(),
        orig.iter().filter(|p| p.stream == 0).collect::<Vec<_>>(),
    );
    // Pictures before the first keyframe are delivered (not keys); from the first key on
    // every picture is ffmpeg-identical to the full file's.
    let k = gv.iter().position(|p| p.key).expect("a keyframe");
    let at = ov
        .iter()
        .position(|p| p.key && p.data == gv[k].data)
        .expect("same keyframe");
    assert!(k < 12, "{k}");
    for (g, w) in gv[k..].iter().zip(&ov[at..]) {
        assert_eq!((g.data.len(), g.pts, g.key), (w.data.len(), w.pts, w.key));
        assert!(g.data == w.data);
    }
    assert_eq!(gv.len() - k, ov.len() - at);
    // Audio: a suffix of the full file's, same timestamps.
    let (ga, oa) = (
        got.iter().filter(|p| p.stream == 1).collect::<Vec<_>>(),
        orig.iter().filter(|p| p.stream == 1).collect::<Vec<_>>(),
    );
    let tail = &oa[oa.len() - ga.len()..];
    for (g, w) in ga.iter().zip(tail) {
        assert!(g.data == w.data && g.pts == w.pts);
    }
    seek_exact(&path);
}

/// A VobSub PES injected after the first pack: a refused track named for what it is, and
/// the other streams unchanged. Also private_stream_2 (nav) and padding in the mix.
#[test]
fn dvd_subpictures_nav_and_padding() {
    let b = committed("ps_mpeg2_ac3.vob");
    let mut d0 = open_bytes(b.clone()).unwrap();
    let mut base = Vec::new();
    while let Ok(p) = d0.next_packet() {
        base.push((p.stream, p.pts, p.data.to_vec()));
    }
    // After the first pack header + system header.
    let at = b.windows(4).position(|w| w == [0, 0, 1, 0xBB]).unwrap();
    let at = at + 6 + usize::from(u16::from_be_bytes([b[at + 4], b[at + 5]]));
    let pes = |id: u8, body: &[u8]| {
        let mut v = vec![0, 0, 1, id];
        v.extend_from_slice(&(body.len() as u16).to_be_bytes());
        v.extend_from_slice(body);
        v
    };
    let mut inj = pes(0xBF, &[0u8; 100]);
    inj.extend(pes(0xBE, &[0xFF; 40]));
    let mut sub = vec![0x81, 0x80, 5, 0x21, 0, 1, 0, 1];
    sub.extend_from_slice(&[0x20, 1, 2, 3, 4]);
    inj.extend(pes(0xBD, &sub));
    let mut dts = vec![0x88, 0, 0, 0]; // DTS substream
    dts.extend_from_slice(&[0x7F, 0xFE, 0x80, 0x01, 0, 0]);
    let mut dtspes = vec![0x81, 0x00, 0];
    dtspes.extend_from_slice(&dts);
    inj.extend(pes(0xBD, &dtspes));
    let mut m = b[..at].to_vec();
    m.extend_from_slice(&inj);
    m.extend_from_slice(&b[at..]);
    let mut d = open_bytes(m).unwrap();
    let t = d.tracks();
    let sp = t
        .iter()
        .find(|t| t.pid == 0xBD20)
        .expect("subpicture track");
    // The subpicture is a DvdSub stream now; this unit never completes, so it yields no packet.
    assert!(sp.stream.is_some() && sp.refusal.is_none(), "{sp:?}");
    let dtst = t.iter().find(|t| t.pid == 0xBD88).expect("dts track");
    assert!(dtst.refusal.as_deref().unwrap().starts_with("dts"));
    assert_eq!(d.streams().len(), 3);
    let mut got = Vec::new();
    while let Ok(p) = d.next_packet() {
        got.push((p.stream, p.pts, p.data.to_vec()));
    }
    // The new subpicture stream may shift later stream numbers; the packets themselves are the same.
    let strip = |v: &[(u32, Option<i64>, Vec<u8>)]| -> Vec<(Option<i64>, Vec<u8>)> {
        v.iter().map(|x| (x.1, x.2.clone())).collect()
    };
    assert!(
        strip(&got) == strip(&base),
        "refused and ignored elements change nothing"
    );
}

#[test]
fn truncated_files_give_a_prefix() {
    for name in [
        "ps_mpeg2_ac3.vob",
        "ps_mpeg1_mp2.mpg",
        "ps_pcm_dvd.vob",
        "ps_mpeg2.m2v",
    ] {
        let b = committed(name);
        let mut d = open_bytes(b.clone()).unwrap();
        let mut full: Vec<Vec<Vec<u8>>> = vec![Vec::new(); 2];
        while let Ok(p) = d.next_packet() {
            full[p.stream as usize].push(p.data.to_vec());
        }
        let step = (b.len() / 61).max(1);
        for len in (0..64).chain((64..b.len()).step_by(step)) {
            let Ok(mut d) = open_bytes(b[..len].to_vec()) else {
                continue;
            };
            let mut got: Vec<Vec<(Vec<u8>, bool)>> = vec![Vec::new(); 8];
            while let Ok(p) = d.next_packet() {
                got[p.stream as usize].push((p.data.to_vec(), p.flags.corrupt));
            }
            for (s, g) in got.iter().enumerate() {
                if g.is_empty() {
                    continue;
                }
                assert!(
                    g.len() <= full[s].len(),
                    "{name} cut {len}: stream {s} has more packets"
                );
                // All but the last are whole packets of the full read.
                for (i, (data, bad)) in g.iter().enumerate().take(g.len().saturating_sub(1)) {
                    if *bad {
                        continue;
                    }
                    assert!(*data == full[s][i], "{name} cut {len}: stream {s} #{i}");
                }
            }
        }
    }
}

fn exercise(bytes: Vec<u8>) {
    let Ok(mut d) = open_bytes(bytes) else {
        return;
    };
    let _ = (d.tracks(), d.duration_secs(), d.stats(), d.epoch_count());
    for round in 0..2 {
        if round == 1 {
            d.set_video_framing(ec_mpegts::VideoFraming::LengthPrefixed);
        }
        while d.next_packet().is_ok() {}
        for s in 0..d.streams().len() as u32 {
            for t in [0i64, 20_000, 1 << 33, -1] {
                for mode in [SeekMode::SyncBefore, SeekMode::SyncAfter, SeekMode::Exact] {
                    let _ = d.seek(s, Timestamp::new(t, TimeBase::new(1, 90_000)), mode);
                    for _ in 0..20 {
                        if d.next_packet().is_err() {
                            break;
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn mutated_files_never_panic() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for name in [
        "ps_mpeg2_ac3.vob",
        "ps_mpeg1_mp2.mpg",
        "ps_pcm_dvd.vob",
        "ps_mpeg2.m2v",
    ] {
        let base = committed(name);
        for round in 0..60 {
            let mut b = base.clone();
            for _ in 0..1 + rng.below(6) {
                let at = rng.below(b.len());
                match rng.below(6) {
                    0 => b[at] ^= 1 << rng.below(8),
                    1 => b.truncate(at.max(1)),
                    2 => {
                        let n = rng.below(300).min(b.len() - at);
                        b.drain(at..at + n);
                    }
                    3 => {
                        let n = rng.below(200);
                        let junk: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
                        b.splice(at..at, junk);
                    }
                    4 => {
                        let n = rng.below(2000).min(b.len() - at);
                        b[at..at + n].fill(if round % 2 == 0 { 0 } else { 0xFF });
                    }
                    _ => {
                        let n = rng.below(4000).min(b.len() - at);
                        let chunk = b[at..at + n].to_vec();
                        b.splice(at..at, chunk);
                    }
                }
                if b.is_empty() {
                    break;
                }
            }
            exercise(b);
        }
        // Start codes with absurd lengths everywhere.
        let mut b = base.clone();
        for w in 0..b.len().saturating_sub(6) {
            if b[w..w + 3] == [0, 0, 1] && b[w + 3] >= 0xB9 && w % 3 == 0 {
                b[w + 4] = 0xFF;
                b[w + 5] = 0xFF;
            }
        }
        exercise(b);
    }
}

#[test]
fn pts_wrap_is_unwrapped() {
    let Some(p) = mkps(
        "wrap.vob",
        V2,
        &["-c:a", "ac3"],
        &["-output_ts_offset", "95442", "-f", "vob"],
    ) else {
        return;
    };
    let mut d = open_ps(&p);
    let v = read_ps(&mut d);
    let pts: Vec<i64> = v
        .iter()
        .filter(|x| x.stream == 0)
        .filter_map(|x| x.pts)
        .collect();
    let m = 1i64 << 33;
    assert!(
        pts.iter().any(|&x| x < m) && pts.iter().any(|&x| x > m),
        "the clock crosses 2^33"
    );
    let mut sorted = pts.clone();
    sorted.sort_unstable();
    assert!(
        sorted.windows(2).all(|w| w[1] - w[0] == 3600),
        "uniform after unwrapping"
    );
    let dur = d.duration_secs().unwrap();
    assert!((dur - 3.0).abs() < 0.1, "{dur}");
}

#[test]
fn raw_stream_timestamps_follow_the_frame_rate() {
    let b = committed("ps_mpeg2.m2v");
    let mut d = open_bytes(b).unwrap();
    assert_eq!(d.kind(), PsKind::VideoElementary);
    let mut pkts = Vec::new();
    while let Ok(p) = d.next_packet() {
        pkts.push((p.pts.unwrap(), p.dts.unwrap(), p.flags.keyframe));
    }
    assert_eq!(pkts.len(), 13);
    let mut pts: Vec<i64> = pkts.iter().map(|p| p.0).collect();
    pts.sort_unstable();
    assert!(pts.windows(2).all(|w| w[1] - w[0] == 3600), "{pts:?}");
    assert_eq!(pts[0], 0);
    assert!(pkts.iter().all(|p| p.1 <= p.0), "dts never after pts");
    assert!(
        pkts.windows(2).all(|w| w[1].1 - w[0].1 == 3600),
        "dts one frame apart"
    );
    assert!((d.duration_secs().unwrap() - 0.52).abs() < 1e-9);
}

// ---------------------------------------------------------------------------
// Tail picture without a PES timestamp; byte-joined VOBs
// ---------------------------------------------------------------------------

const FLASH_SRC: &str = "color=c=black:s=352x288:r=25:d=2.4,drawbox=x=0:y=0:w=352:h=288:color=white:t=fill:enable='between(t,1,1.2)'";

fn video_index(d: &D) -> u32 {
    d.streams()
        .iter()
        .find(|s| s.params.video().is_some())
        .unwrap()
        .index
}

fn frames_by_ffprobe(path: &Path) -> usize {
    let o = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-count_frames", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=nb_read_frames", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(|l| l.trim().trim_end_matches(','))
        .find(|l| !l.is_empty())
        .unwrap()
        .parse()
        .unwrap()
}

/// The last picture of an MPEG-1 `-f mpeg` file carries no PES timestamp: it is still
/// emitted with a derived time and counted in the stream length.
#[test]
fn tail_picture_without_a_timestamp_is_counted() {
    let mut cases = Vec::new();
    for (name, video) in [
        ("pstail-flash.mpg", FLASH_SRC),
        (
            "pstail-card.mpg",
            "testsrc2=size=352x288:rate=25:duration=2.4",
        ),
    ] {
        let mut a: Vec<&str> = vec!["-f", "lavfi", "-i", video, "-f", "lavfi", "-i"];
        a.push("sine=frequency=440:sample_rate=48000:duration=2.4");
        a.extend_from_slice(&[
            "-map",
            "0:v",
            "-c:v",
            "mpeg1video",
            "-b:v",
            "2M",
            "-g",
            "10",
            "-bf",
            "2",
        ]);
        a.extend_from_slice(&[
            "-pix_fmt", "yuv420p", "-map", "1:a", "-c:a", "mp2", "-f", "mpeg",
        ]);
        if let Some(p) = fixture(name, &a) {
            cases.push(p);
        }
    }
    for path in cases {
        let mut d = open_ps(&path);
        let v = video_index(&d);
        let pkts = read_ps(&mut d);
        let mine: Vec<&Pkt> = pkts.iter().filter(|p| p.stream == v).collect();
        let want = frames_by_ffprobe(&path);
        assert_eq!(mine.len(), want, "{path:?}: packets");
        assert_eq!(want, 60);
        // Every picture has a presentation time; they tile the timeline one frame apart.
        let mut pts: Vec<i64> = mine
            .iter()
            .map(|p| p.pts.expect("a pts on every picture"))
            .collect();
        pts.sort_unstable();
        assert!(
            pts.windows(2).all(|w| w[1] - w[0] == 3600),
            "{path:?}: {pts:?}"
        );
        let s = &d.streams()[v as usize];
        assert_eq!(
            s.duration,
            Some(60 * 3600),
            "{path:?}: video duration is 60 frames"
        );
    }
}

/// Two VOBs of 2.4 s joined by bytes: SCR and PTS restart at the join.
fn joined_vob() -> Option<(PathBuf, u64)> {
    let mk = |name: &str, vf: &str, hz: &str| {
        let video = format!("testsrc2=size=352x288:rate=25:duration=2.4{vf}");
        let audio = format!("sine=frequency={hz}:sample_rate=48000:duration=2.4");
        let a = [
            "-f",
            "lavfi",
            "-i",
            &video,
            "-f",
            "lavfi",
            "-i",
            &audio,
            "-map",
            "0:v",
            "-c:v",
            "mpeg2video",
            "-b:v",
            "4M",
            "-g",
            "10",
            "-bf",
            "2",
            "-pix_fmt",
            "yuv420p",
            "-map",
            "1:a",
            "-c:a",
            "mp2",
            "-b:a",
            "192k",
            "-f",
            "vob",
        ];
        fixture(name, &a)
    };
    let a = mk("pstail-a.vob", "", "440")?;
    let b = mk("pstail-b.vob", ",negate", "880")?;
    let out = fixture_dir().join("pstail-join.vob");
    let alen = std::fs::metadata(&a).unwrap().len();
    if !out.exists() {
        let mut bytes = std::fs::read(&a).unwrap();
        bytes.extend(std::fs::read(&b).unwrap());
        // Three tests build this file in parallel: each writes its own temporary file.
        let tmp = fixture_dir().join(unique_name("pstail-join.tmp"));
        std::fs::write(&tmp, bytes).unwrap();
        std::fs::rename(&tmp, &out).unwrap();
    }
    Some((out, alen))
}

#[test]
fn joined_vobs_have_the_summed_duration() {
    let Some((path, _)) = joined_vob() else {
        return;
    };
    let d = open_ps(&path);
    let single = open_ps(&fixture_dir().join("pstail-a.vob"));
    let v = video_index(&d) as usize;
    let one = single.streams()[video_index(&single) as usize]
        .duration
        .unwrap();
    assert_eq!(
        d.streams()[v].duration,
        Some(2 * one),
        "video stream duration"
    );
    assert!(
        (d.duration_secs().unwrap() - 4.8).abs() < 0.03,
        "{:?}",
        d.duration_secs()
    );
    let mut d = d;
    let pkts = read_ps(&mut d);
    let mut pts: Vec<i64> = pkts
        .iter()
        .filter(|p| p.stream == v as u32)
        .filter_map(|p| p.pts)
        .collect();
    assert_eq!(pts.len(), 120);
    pts.sort_unstable();
    assert!(
        pts.windows(2).all(|w| w[1] - w[0] == 3600),
        "continuous display times"
    );
}

#[test]
fn seeks_on_both_sides_of_a_join_equal_the_sequential_decode() {
    let Some((path, _)) = joined_vob() else {
        return;
    };
    let mut d = open_ps(&path);
    let v = video_index(&d);
    let all = read_ps(&mut d);
    let tb = TimeBase::new(1, 90_000);
    let vid: Vec<&Pkt> = all.iter().filter(|p| p.stream == v).collect();
    let mut shown: Vec<i64> = vid.iter().map(|p| p.pts.unwrap()).collect();
    shown.sort_unstable();
    // Frames well before, at, and after the join (frame 60), and the last.
    for n in [3usize, 29, 57, 58, 59, 60, 61, 62, 65, 90, 119] {
        let target = shown[n];
        for mode in [SeekMode::Exact, SeekMode::SyncBefore] {
            let mut d = open_ps(&path);
            d.seek(v, Timestamp::new(target, tb), mode).unwrap();
            let got = read_ps(&mut d);
            let first = got.iter().find(|p| p.stream == v).unwrap();
            // The expected landing from the sequential read.
            let want = match mode {
                SeekMode::Exact => vid.iter().rfind(|p| p.dts.or(p.pts).unwrap() <= target),
                _ => vid.iter().rfind(|p| {
                    p.key && p.pts.unwrap() <= target && p.dts.or(p.pts).unwrap() <= target
                }),
            }
            .unwrap();
            assert_eq!(
                (first.pts, first.dts, &first.data),
                (want.pts, want.dts, &want.data),
                "frame {n} {mode:?}: landing"
            );
            // And the rest of the video equals the sequential read from there.
            let at = vid
                .iter()
                .position(|p| p.off == want.off && p.pts == want.pts)
                .unwrap();
            let rest: Vec<_> = vid[at..].iter().map(|p| (p.pts, p.dts, &p.data)).collect();
            let mine: Vec<_> = got
                .iter()
                .filter(|p| p.stream == v)
                .map(|p| (p.pts, p.dts, &p.data))
                .collect();
            assert_eq!(mine, rest, "frame {n} {mode:?}: packets after the seek");
        }
    }
    // The generic check over many fractions, as for any other file.
    seek_exact(&path);
}

#[test]
fn audio_keeps_its_offset_to_the_video_after_a_join() {
    let Some((path, alen)) = joined_vob() else {
        return;
    };
    let mut d = open_ps(&path);
    let v = video_index(&d);
    let a = d
        .streams()
        .iter()
        .find(|s| s.params.audio().is_some())
        .unwrap()
        .index;
    let all = read_ps(&mut d);
    let first = |stream: u32, after: bool| {
        all.iter()
            .filter(|p| p.stream == stream && (p.off >= alen) == after)
            .filter_map(|p| p.pts)
            .min()
            .unwrap()
    };
    let before = first(a, false) - first(v, false);
    let after = first(a, true) - first(v, true);
    // One MP2 frame is 1152 samples (2160 ticks).
    assert!(
        (after - before).abs() <= 2160,
        "offset {before} before, {after} after the join"
    );
    // The audio of the second half starts where the first half's ends.
    let mut apts: Vec<i64> = all
        .iter()
        .filter(|p| p.stream == a)
        .filter_map(|p| p.pts)
        .collect();
    apts.sort_unstable();
    assert!(
        apts.windows(2).all(|w| (w[1] - w[0] - 2160).abs() <= 2160),
        "audio is continuous"
    );
}

/// The TS path emits every picture too (its PES packets carry one picture each).
#[test]
fn ts_tail_picture_is_counted() {
    for (name, codec) in [
        ("pstail-flash1.ts", "mpeg1video"),
        ("pstail-flash2.ts", "mpeg2video"),
    ] {
        let a = [
            "-f", "lavfi", "-i", FLASH_SRC, "-c:v", codec, "-b:v", "2M", "-g", "10", "-bf", "2",
            "-pix_fmt", "yuv420p", "-f", "mpegts",
        ];
        let Some(path) = fixture(name, &a) else {
            continue;
        };
        let mut d = open(&path);
        let v = d
            .streams()
            .iter()
            .find(|s| s.params.video().is_some())
            .unwrap()
            .index;
        let pkts = read_all(&mut d);
        let mine: Vec<&Pkt> = pkts.iter().filter(|p| p.stream == v).collect();
        assert_eq!(mine.len(), frames_by_ffprobe(&path), "{name}");
        assert!(
            mine.iter().all(|p| p.pts.is_some()),
            "{name}: every picture has a pts"
        );
        assert_eq!(d.streams()[v as usize].duration, Some(60 * 3600), "{name}");
    }
}
