//! IMA/MS ADPCM and G.711 through the probe (Reader + AudioDecoder) from
//! ffmpeg-made WAV, AVI and MOV files, compared with ffmpeg's s16 decode.
//! Skips without ffmpeg unless `EC_REQUIRE_FIXTURES=1`.

use std::path::PathBuf;
use std::process::Command;

use ec_core::registry::MediaType;
use ec_probe::{AudioDecoder, Reader};

#[test]
fn legacy_audio_decodes_through_the_probe() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        assert!(std::env::var_os("EC_REQUIRE_FIXTURES").is_none(), "ffmpeg missing");
        return;
    }
    let dir: PathBuf = std::env::temp_dir().join(format!("ec-probe-legacy-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("tmp");
    let mut n = 0;
    for (enc, ext) in [
        ("adpcm_ima_wav", "wav"),
        ("adpcm_ima_wav", "avi"),
        ("adpcm_ms", "wav"),
        ("adpcm_ms", "avi"),
        ("adpcm_ima_qt", "mov"),
        ("pcm_alaw", "wav"),
        ("pcm_alaw", "mov"),
        ("pcm_mulaw", "avi"),
        ("pcm_mulaw", "mov"),
    ] {
        for ch in ["1", "2"] {
            let path = dir.join(format!("{enc}-{ch}.{ext}"));
            let o = Command::new("ffmpeg")
                .args(["-nostdin", "-y", "-v", "error", "-f", "lavfi", "-i"])
                .arg("aevalsrc='0.6*sin(2*PI*440*t)+0.2*sin(2*PI*3000*t*t)':s=16000:d=1.1")
                .args(["-ac", ch, "-c:a", enc])
                .arg(&path)
                .output()
                .expect("ffmpeg");
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
            let want = Command::new("ffmpeg")
                .args(["-nostdin", "-v", "error", "-i"])
                .arg(&path)
                .args(["-f", "f32le", "-"])
                .output()
                .expect("ffmpeg");
            let mut reader = Reader::open(&path).unwrap_or_else(|e| panic!("{enc}.{ext}: {e}"));
            let s = reader
                .streams()
                .iter()
                .find(|s| s.params.codec.media_type() == MediaType::Audio)
                .expect("audio")
                .clone();
            let mut dec = AudioDecoder::new(&s.params).expect("decoder");
            let mut got: Vec<f32> = Vec::new();
            loop {
                match reader.next_packet() {
                    Ok(p) if p.stream == s.index => {
                        let mut part = Vec::new();
                        dec.decode(&p, &mut part).expect("decode");
                        got.extend(part);
                    }
                    Ok(_) => {}
                    Err(e) if e.is_eof() => break,
                    Err(e) => panic!("{enc}.{ext}: {e}"),
                }
            }
            let mut tail = Vec::new();
            dec.flush(&mut tail).expect("flush");
            got.extend(tail);
            let want: Vec<f32> = want
                .stdout
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| f32::from_le_bytes(*c))
                .collect();
            assert_eq!(got.len(), want.len(), "{enc}-{ch}.{ext}: sample count");
            assert!(got == want, "{enc}-{ch}.{ext}: samples differ");
            n += 1;
        }
    }
    std::fs::remove_dir_all(&dir).ok();
    eprintln!("probe legacy audio: {n} files bit-exact");
}
