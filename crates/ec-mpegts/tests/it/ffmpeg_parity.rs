//! Packet-for-packet parity with ffmpeg's demux on ffmpeg-made transport streams:
//! bytes, sizes, timestamps, keyframe flags, duration, plus seek exactness.
//!
//! Fixtures are generated at test time from synthetic sources (testsrc2, sine). The
//! tests skip, saying so, when ffmpeg is absent and fail instead when
//! `EC_REQUIRE_FIXTURES` is set.

use crate::common::*;
use ec_core::registry::{Demuxer, SeekMode};
use ec_core::{CodecId, TimeBase, Timestamp};

fn v264(name: &str, extra: &[&str], mux: &[&str]) -> Option<std::path::PathBuf> {
    let mut a: Vec<&str> = VIDEO_SRC.to_vec();
    a.extend_from_slice(AUDIO_SRC);
    a.extend_from_slice(&[
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-g",
        "25",
        "-bf",
        "2",
    ]);
    a.extend_from_slice(extra);
    a.extend_from_slice(mux);
    fixture(name, &a)
}

/// Compares our packets with ffprobe/ffmpeg for every stream we expose.
fn parity(path: &std::path::Path) {
    parity_slack(path, 0);
}

/// As [`parity`], allowing audio timestamps to differ by `slack` ticks (ffmpeg rounds
/// the frames after the first of a PES its own way when the rate does not divide 90 kHz).
fn parity_slack(path: &std::path::Path, slack: i64) {
    let mut d = open(path);
    let pkts = read_all(&mut d);
    let rows = ffprobe_rows(path);
    assert!(!d.streams().is_empty(), "{path:?}: no streams");
    for s in d.streams() {
        let pid = d.stream_pid(s.index).unwrap();
        let (si, rows) = rows
            .get(&pid)
            .unwrap_or_else(|| panic!("ffprobe lacks pid {pid:#x}"));
        let mine: Vec<&Pkt> = pkts.iter().filter(|p| p.stream == s.index).collect();
        let name = s.params.codec.name();
        let lpcm = s.params.codec == CodecId::PcmS16Be;
        assert_eq!(mine.len(), rows.len(), "{path:?} {name} packet count");
        let (b0, r0) = (
            mine[0].pts.or(mine[0].dts).unwrap(),
            rows[0].pts.or(rows[0].dts).unwrap(),
        );
        let (md0, rd0) = (
            mine[0].dts.or(mine[0].pts).unwrap(),
            rows[0].dts.or(rows[0].pts).unwrap(),
        );
        let _ = (b0, r0);
        for (i, (m, r)) in mine.iter().zip(rows).enumerate() {
            if !lpcm {
                assert_eq!(m.data.len(), r.size, "{path:?} {name} #{i} size");
            }
            let mpts = m.pts.map(|v| v - md0);
            let rpts = r.pts.map(|v| v - rd0);
            let near = |a: Option<i64>, b: Option<i64>| match (a, b) {
                (Some(a), Some(b)) => (a - b).abs() <= if m.dts.is_none() { slack } else { 0 },
                (a, b) => a == b,
            };
            assert!(
                near(mpts, rpts),
                "{path:?} {name} #{i} pts {mpts:?} vs {rpts:?}"
            );
            let mdts = m.dts.or(m.pts).map(|v| v - md0);
            let rdts = r.dts.or(r.pts).map(|v| v - rd0);
            assert!(near(mdts, rdts), "{path:?} {name} #{i} dts");
            assert_eq!(m.key, r.key, "{path:?} {name} #{i} keyframe");
        }
        let bytes: Vec<u8> = mine.iter().flat_map(|p| p.data.iter().copied()).collect();
        let theirs = if lpcm {
            ffmpeg_pcm_s16be(path, *si)
        } else {
            ffmpeg_bytes(path, *si)
        };
        eprintln!(
            "{} {name}: {} packets, {} bytes match",
            path.file_name().unwrap().to_string_lossy(),
            mine.len(),
            bytes.len()
        );
        assert!(
            bytes == theirs,
            "{path:?} {name}: bytes differ ({} vs {})",
            bytes.len(),
            theirs.len()
        );
    }
}

/// Duration against what ffprobe's packets say: last end minus first start, over
/// every stream, within one frame (ffprobe's own `format=duration` estimate is off
/// by up to two audio frames for AC-3 at 44.1 kHz, so it is not the reference).
fn duration_ok(path: &std::path::Path) {
    let d = open(path);
    let got = d.duration_secs().expect("duration");
    let rows = ffprobe_rows(path);
    let mut want = 0f64;
    for s in d.streams() {
        let (_, r) = &rows[&d.stream_pid(s.index).unwrap()];
        let first = r.iter().filter_map(|x| x.pts).min().unwrap();
        let end = r
            .iter()
            .filter_map(|x| x.pts.map(|p| p + x.dur.unwrap_or(0)))
            .max()
            .unwrap();
        want = want.max((end - first) as f64 / 90_000.0);
    }
    assert!(
        (got - want).abs() <= 0.045,
        "{path:?}: duration {got} vs {want}"
    );
}

/// Seeks and compares the packets that follow with a sequential read.
fn seek_exact(path: &std::path::Path) {
    let mut d = open(path);
    let all = read_all(&mut d);
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
    let tb = TimeBase::new(1, 90_000);
    for secs in [0.0, 0.3, 0.97, 1.5, 2.2, 2.9, 10.0] {
        let target = base + (secs * 90_000.0) as i64;
        for mode in [SeekMode::SyncBefore, SeekMode::Exact] {
            let mut d = open(path);
            let sk = vid.unwrap_or(0);
            d.seek(sk, Timestamp::new(target, tb), mode).unwrap();
            let landing = d.last_packet_offset();
            let got = read_all(&mut d);
            let want: Vec<&Pkt> = all.iter().filter(|p| p.off >= landing).collect();
            assert_eq!(
                got.len(),
                want.len(),
                "{path:?} seek {secs} {mode:?}: count"
            );
            for (g, w) in got.iter().zip(&want) {
                assert!(
                    g.stream == w.stream
                        && g.pts == w.pts
                        && g.dts == w.dts
                        && g.key == w.key
                        && g.data == w.data,
                    "{path:?} seek {secs} {mode:?}: packet differs at off {}",
                    w.off
                );
            }
            if mode == SeekMode::SyncBefore
                && let Some(v) = vid
            {
                {
                    let first = got.iter().find(|p| p.stream == v).unwrap();
                    assert!(first.key, "{path:?} seek {secs}: lands on a keyframe");
                    let fp = first.pts.unwrap();
                    assert!(
                        fp <= target.max(base) || secs == 0.0,
                        "{path:?} seek {secs}: {fp} > {target}"
                    );
                    // No later keyframe at or before the target.
                    let better = all.iter().any(|p| {
                        p.stream == v && p.key && p.pts.is_some_and(|x| x > fp && x <= target)
                    });
                    assert!(!better, "{path:?} seek {secs}: a closer keyframe exists");
                }
            }
        }
    }
    cleanup_none();
}

fn cleanup_none() {}

/// Every ffmpeg-made transport stream case: parity, duration and seek exactness.
/// One test over a table so every case runs and all failures are named.
#[test]
fn parity_cases() {
    type Case = (&'static str, fn() -> Option<std::path::PathBuf>);
    const CASES: &[Case] = &[
        ("h264_aac_ts", || {
            v264("h264_aac.ts", &["-c:a", "aac"], &["-f", "mpegts"])
        }),
        ("h264_ac3_m2ts", || {
            v264(
                "h264_ac3.m2ts",
                &["-c:a", "ac3"],
                &["-f", "mpegts", "-mpegts_m2ts_mode", "1"],
            )
        }),
        ("h264_interlaced", || {
            v264(
                "h264_ildct.ts",
                &[
                    "-c:a",
                    "aac",
                    "-flags",
                    "+ildct+ilme",
                    "-top",
                    "1",
                    "-x264-params",
                    "interlaced=1:tff=1",
                ],
                &["-f", "mpegts"],
            )
        }),
        ("h264_ac3", || {
            v264("h264_ac3.ts", &["-c:a", "ac3"], &["-f", "mpegts"])
        }),
        ("h264_eac3", || {
            v264("h264_eac3.ts", &["-c:a", "eac3"], &["-f", "mpegts"])
        }),
        ("h264_mp2", || {
            v264("h264_mp2.ts", &["-c:a", "mp2"], &["-f", "mpegts"])
        }),
        ("h264_lpcm_m2ts", || {
            v264(
                "h264_lpcm.m2ts",
                &["-c:a", "pcm_bluray"],
                &["-f", "mpegts", "-mpegts_m2ts_mode", "1"],
            )
        }),
        ("h264_mp3", || {
            v264("h264_mp3.ts", &["-c:a", "libmp3lame"], &["-f", "mpegts"])
        }),
        ("hevc_aac", || hevc("hevc_aac.ts", "aac")),
    ];
    let mut failures = Vec::new();
    for &(name, make) in CASES {
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let Some(p) = make() else { return };
            parity(&p);
            duration_ok(&p);
            seek_exact(&p);
        }));
        if ran.is_err() {
            failures.push(name);
        }
    }
    assert!(failures.is_empty(), "failed cases: {failures:?}");
}

#[test]
fn mp2_in_ts_is_exposed_as_mp2() {
    let Some(p) = v264("h264_mp2.ts", &["-c:a", "mp2"], &["-f", "mpegts"]) else {
        return;
    };
    let d = open(&p);
    let audio: Vec<_> = d
        .streams()
        .iter()
        .filter(|s| s.params.codec == CodecId::Mp2)
        .collect();
    assert_eq!(
        audio.len(),
        1,
        "an MP2 stream must be playable, not refused"
    );
    assert!(d.tracks().iter().all(|t| t.refusal.is_none()));
}
fn audio_only(name: &str, codec: &str, rate: &str, extra: &[&str]) -> Option<std::path::PathBuf> {
    let mut a: Vec<&str> = vec![
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=48000:duration=3",
        "-ac",
        "2",
        "-ar",
        rate,
        "-c:a",
        codec,
    ];
    a.extend_from_slice(extra);
    a.extend_from_slice(&["-f", "mpegts"]);
    fixture(name, &a)
}

#[test]
fn audio_only_aac_44100() {
    let Some(p) = audio_only("audio_aac.ts", "aac", "44100", &[]) else {
        return;
    };
    parity_slack(&p, 8);
    seek_exact(&p);
    duration_ok(&p);
}

/// Frame-size tables: AC-3 and E-AC-3 at every sample rate and several bit rates.
#[test]
fn ac3_family_sizes() {
    for (codec, rate, br) in [
        ("ac3", "32000", "192k"),
        ("ac3", "44100", "192k"),
        ("ac3", "44100", "448k"),
        ("ac3", "48000", "640k"),
        ("ac3", "48000", "96k"),
        ("eac3", "32000", "192k"),
        ("eac3", "44100", "256k"),
        ("eac3", "48000", "384k"),
    ] {
        let name = format!("audio_{codec}_{rate}_{br}.ts");
        let Some(p) = audio_only(&name, codec, rate, &["-b:a", br]) else {
            return;
        };
        parity_slack(&p, 8);
        seek_exact(&p);
        duration_ok(&p);
    }
}

fn hevc(name: &str, audio: &str) -> Option<std::path::PathBuf> {
    let mut a: Vec<&str> = VIDEO_SRC.to_vec();
    a.extend_from_slice(AUDIO_SRC);
    a.extend_from_slice(&[
        "-c:v",
        "libx265",
        "-preset",
        "ultrafast",
        "-g",
        "25",
        "-x265-params",
        "log-level=none",
        "-c:a",
        audio,
        "-f",
        "mpegts",
    ]);
    fixture(name, &a)
}

#[test]
fn codecs_and_extradata() {
    let Some(p) = v264("h264_aac.ts", &["-c:a", "aac"], &["-f", "mpegts"]) else {
        return;
    };
    let d = open(&p);
    let s = d.streams();
    assert_eq!(s[0].params.codec, CodecId::H264);
    assert_eq!(s[0].params.video().unwrap().width, 320);
    assert_eq!(
        s[0].params.video().unwrap().frame_rate,
        Some(TimeBase::new(25, 1))
    );
    assert_eq!(s[0].params.extradata.as_ref().unwrap()[0], 1);
    assert_eq!(s[1].params.codec, CodecId::Aac);
    assert_eq!(s[1].params.extradata.as_ref().unwrap().len(), 2);
    assert_eq!(s[1].params.audio().unwrap().sample_rate, 48000);
}

fn two_programs() -> Option<std::path::PathBuf> {
    let mut a: Vec<&str> = VIDEO_SRC.to_vec();
    a.extend_from_slice(AUDIO_SRC);
    a.extend_from_slice(&[
        "-map",
        "0:v",
        "-map",
        "1:a",
        "-map",
        "0:v",
        "-map",
        "1:a",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-g",
        "25",
        "-c:a:0",
        "aac",
        "-c:a:1",
        "ac3",
        "-program",
        "title=one:program_num=1:st=0:st=1",
        "-program",
        "title=two:program_num=2:st=2:st=3",
        "-f",
        "mpegts",
    ]);
    fixture("two_programs.ts", &a)
}

#[test]
fn multiple_programs() {
    let Some(p) = two_programs() else { return };
    let d = open(&p);
    let progs = d.programs();
    assert_eq!(progs.len(), 2, "{progs:?}");
    assert_eq!(d.program_number(), Some(progs[0].number));
    let first: Vec<_> = d.streams().iter().map(|s| s.params.codec).collect();
    assert_eq!(first, [CodecId::H264, CodecId::Aac]);
    parity(&p);
    seek_exact(&p);
    let f = std::fs::File::open(&p).unwrap();
    let d2 =
        ec_mpegts::TsDemuxer::with_program(std::io::BufReader::new(f), progs[1].number).unwrap();
    let second: Vec<_> = d2.streams().iter().map(|s| s.params.codec).collect();
    assert_eq!(second, [CodecId::H264, CodecId::Ac3]);
    assert_ne!(d.stream_pid(0), d2.stream_pid(0));
    let f = std::fs::File::open(&p).unwrap();
    assert!(ec_mpegts::TsDemuxer::with_program(std::io::BufReader::new(f), 99).is_err());
}

/// 33-bit PTS wrap: the clock starts 1.7 s before 2^33 / 90000.
#[test]
fn pts_wrap_is_unwrapped() {
    let mut a: Vec<&str> = VIDEO_SRC.to_vec();
    a.extend_from_slice(AUDIO_SRC);
    a.extend_from_slice(&[
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-g",
        "25",
        "-bf",
        "2",
        "-c:a",
        "aac",
        "-output_ts_offset",
        "95442",
        "-f",
        "mpegts",
    ]);
    let Some(p) = fixture("wrap.ts", &a) else {
        return;
    };
    let mut d = open(&p);
    let pkts = read_all(&mut d);
    for s in d.streams() {
        let v: Vec<i64> = pkts
            .iter()
            .filter(|p| p.stream == s.index)
            .filter_map(|p| p.dts.or(p.pts))
            .collect();
        assert!(
            v.windows(2).all(|w| w[1] > w[0]),
            "stream {} not monotonic",
            s.index
        );
        let first = v[0];
        let last = *v.last().unwrap();
        assert!(
            first < (1 << 33) && last >= (1 << 33),
            "{first} .. {last} should straddle 2^33"
        );
        assert_eq!(s.start_time.unwrap() / 90_000, first / 90_000);
    }
    assert!((d.duration_secs().unwrap() - 3.0).abs() < 0.1);
    parity(&p);
    seek_exact(&p);
}

/// Truncation and in-place damage: no panic, the damage is counted, and every
/// packet not flagged corrupt is one the intact file has.
#[test]
fn corrupted_and_truncated() {
    let Some(p) = v264("h264_aac.ts", &["-c:a", "aac"], &["-f", "mpegts"]) else {
        return;
    };
    let good = std::fs::read(&p).unwrap();
    let mut d = open(&p);
    let want = read_all(&mut d);
    let known: std::collections::HashSet<(u32, Option<i64>, Vec<u8>)> = want
        .iter()
        .map(|p| (p.stream, p.pts, p.data.clone()))
        .collect();

    let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
    cases.push((
        "truncated mid-packet",
        good[..good.len() * 6 / 10 + 77].to_vec(),
    ));
    let mut hole = good.clone();
    for b in &mut hole[100_000..103_000] {
        *b = 0xAA;
    }
    cases.push(("overwritten", hole));
    let mut cut = good.clone();
    cut.drain(150_000..150_100);
    cases.push(("bytes removed", cut));
    let mut head = vec![0x55u8; 1000];
    head.extend_from_slice(&good);
    cases.push(("garbage in front", head));

    for (name, bytes) in cases {
        let mut d = ec_mpegts::TsDemuxer::new(std::io::Cursor::new(bytes)).expect(name);
        let mut n = 0;
        let mut flagged = 0;
        while let Ok(p) = d.next_packet() {
            n += 1;
            if p.flags.corrupt {
                flagged += 1;
            } else {
                assert!(
                    known.contains(&(p.stream, p.pts, p.data.to_vec())),
                    "{name}: an unflagged packet differs from the intact file"
                );
            }
        }
        assert!(
            n >= want.len() / 2,
            "{name}: only {n} of {} packets",
            want.len()
        );
        let st = d.stats();
        if name == "overwritten" || name == "bytes removed" {
            assert!(
                st.resyncs > 0 || st.cc_errors > 0,
                "{name}: damage unnoticed {st:?}"
            );
        }
        eprintln!("{name}: {n} packets, {flagged} flagged, {st:?}");
    }
}
