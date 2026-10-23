//! Shared helpers: ffmpeg/ffprobe fixtures, packet dumps and the skip convention
//! (`EC_REQUIRE_FIXTURES` turns a skip into a failure).
#![allow(dead_code)]

use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use ec_core::registry::Demuxer;
use ec_mpegts::TsDemuxer;

pub fn required() -> bool {
    std::env::var_os("EC_REQUIRE_FIXTURES").is_some_and(|v| !v.is_empty() && v != "0")
}

/// True when ffmpeg and ffprobe can be run; otherwise prints a skip note (or panics
/// when fixtures are required).
pub fn have_ffmpeg() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    let ok = *OK.get_or_init(|| {
        ["ffmpeg", "ffprobe"].iter().all(|t| {
            Command::new(t)
                .arg("-version")
                .output()
                .is_ok_and(|o| o.status.success())
        })
    });
    if !ok {
        assert!(
            !required(),
            "EC_REQUIRE_FIXTURES is set but ffmpeg/ffprobe are missing"
        );
        eprintln!("skip: ffmpeg/ffprobe not available");
    }
    ok
}

pub fn fixture_dir() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let d = std::env::temp_dir().join("ec-mpegts-fx");
        std::fs::create_dir_all(&d).unwrap();
        d
    })
    .clone()
}

pub const VIDEO_SRC: &[&str] = &[
    "-f",
    "lavfi",
    "-i",
    "testsrc2=size=320x240:rate=25:duration=3",
];
pub const AUDIO_SRC: &[&str] = &[
    "-f",
    "lavfi",
    "-i",
    "sine=frequency=440:sample_rate=48000:duration=3",
];

/// Runs ffmpeg; `Err` carries stderr. Unavailable encoders are a skip.
pub fn ffmpeg(args: &[&str], out: &Path) -> Result<(), String> {
    let o = Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(args)
        .arg(out)
        .output()
        .map_err(|e| e.to_string())?;
    if o.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).into_owned())
    }
}

/// Generates `name` with `args` (input options included) or returns `None` on skip.
pub fn fixture(name: &str, args: &[&str]) -> Option<PathBuf> {
    if !have_ffmpeg() {
        return None;
    }
    // Tests run on threads of one process: one generates a given file at a time.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let out = fixture_dir().join(name);
    if out.exists() {
        return Some(out);
    }
    let tmp = fixture_dir().join(format!("{name}.{}.tmp", std::process::id()));
    match ffmpeg(args, &tmp) {
        Ok(()) => {
            std::fs::rename(&tmp, &out).unwrap();
            Some(out)
        }
        Err(e) => {
            assert!(!required(), "ffmpeg failed for {name}: {e}");
            eprintln!("skip {name}: {e}");
            None
        }
    }
}

pub fn open(path: &Path) -> TsDemuxer<BufReader<File>> {
    TsDemuxer::new(BufReader::new(File::open(path).unwrap())).unwrap()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkt {
    pub stream: u32,
    pub pts: Option<i64>,
    pub dts: Option<i64>,
    pub key: bool,
    pub off: u64,
    pub data: Vec<u8>,
}

pub fn read_all(d: &mut TsDemuxer<BufReader<File>>) -> Vec<Pkt> {
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

/// One row of `ffprobe -show_packets`.
#[derive(Debug, Clone)]
pub struct Row {
    pub pid: u16,
    pub pts: Option<i64>,
    pub dts: Option<i64>,
    pub dur: Option<i64>,
    pub size: usize,
    pub key: bool,
}

pub fn ffprobe_rows(path: &Path) -> HashMap<u16, (u32, Vec<Row>)> {
    let streams = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=index,id",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .unwrap();
    let mut pid_of = HashMap::new();
    for l in String::from_utf8_lossy(&streams.stdout).lines() {
        let mut f = l.split(',');
        let (Some(i), Some(id)) = (f.next(), f.next()) else {
            continue;
        };
        if let (Ok(i), Some(h)) = (i.parse::<u32>(), id.strip_prefix("0x"))
            && let Ok(p) = u16::from_str_radix(h, 16)
        {
            pid_of.insert(i, p);
        }
    }
    let pk = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "packet=stream_index,pts,dts,duration,size,flags",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .unwrap();
    let mut out: HashMap<u16, (u32, Vec<Row>)> = HashMap::new();
    for l in String::from_utf8_lossy(&pk.stdout).lines() {
        let f: Vec<&str> = l.split(',').collect();
        if f.len() < 6 {
            continue;
        }
        let si: u32 = f[0].parse().unwrap();
        let Some(&pid) = pid_of.get(&si) else {
            continue;
        };
        let num = |s: &str| s.parse::<i64>().ok();
        out.entry(pid).or_insert((si, Vec::new())).1.push(Row {
            pid,
            pts: num(f[1]),
            dts: num(f[2]),
            dur: num(f[3]),
            size: f[4].parse().unwrap(),
            key: f[5].starts_with('K'),
        });
    }
    out
}

/// ffmpeg's stream copy of stream `index`, as one byte string.
pub fn ffmpeg_bytes(path: &Path, index: u32) -> Vec<u8> {
    let o = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-map",
            &format!("0:{index}"),
            "-c",
            "copy",
            "-f",
            "data",
            "-",
        ])
        .output()
        .unwrap();
    o.stdout
}

/// ffmpeg's decode of an LPCM stream as big-endian 16-bit samples.
pub fn ffmpeg_pcm_s16be(path: &Path, index: u32) -> Vec<u8> {
    Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", &format!("0:{index}"), "-f", "s16be", "-"])
        .output()
        .unwrap()
        .stdout
}

/// MPEG-2 CRC-32 (poly 0x04C11DB7, initial value all ones, no reflection).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// FNV-1a over a byte iterator.
pub fn fnv(bytes: impl Iterator<Item = u8>) -> u64 {
    bytes.fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    })
}

/// The committed fixtures directory (`tests/data`).
pub fn data() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

/// Seeded xorshift64 for the mutation tests.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}
