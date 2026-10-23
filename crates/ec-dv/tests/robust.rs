//! Damaged and hostile input: nothing may panic, and decoding is deterministic.
use ec_dv::{DvVideoDecoder, PROFILES, extract_audio};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

fn frame_for(p: &ec_dv::Profile, rng: &mut Rng, header_bytes: bool) -> Vec<u8> {
    let mut f: Vec<u8> = (0..p.frame_size).map(|_| rng.next() as u8).collect();
    if header_bytes {
        f[3] = (f[3] & 0x7f) | (p.dsf << 7);
        f[80 * 5 + 48 + 3] = p.video_stype;
        f[4] &= 0xf8;
        for b in 0..p.frame_size / 80 {
            // plausible DIF headers keep the segment status nibbles equal, as real streams do
            f[b * 80 + 3] = (f[b * 80 + 3] & 0x0f) | 0xf0;
        }
        f[3] = (f[3] & 0x7f) | (p.dsf << 7);
        f[80 * 5 + 48 + 3] = p.video_stype;
    }
    f
}

#[test]
fn random_frames_of_every_profile_do_not_panic() {
    let mut rng = Rng(7);
    for (i, p) in PROFILES.iter().enumerate() {
        if i == 2 || i == 9 {
            continue; // PAL 4:1:1 / IEC 61883-5 share their header bytes with other entries
        }
        for round in 0..6 {
            let f = frame_for(p, &mut rng, round % 2 == 0);
            let mut d = DvVideoDecoder::new();
            if round % 2 == 1 {
                let _ = d.decode(&f); // random header: an error is fine, a panic is not
                let _ = extract_audio(&f);
                continue;
            }
            let a = d.decode(&f).unwrap_or_else(|e| panic!("profile {i} round {round}: {e:?}"));
            let b = DvVideoDecoder::new().decode(&f).unwrap();
            assert_eq!(a.planes, b.planes, "decode must be deterministic");
            assert_eq!((a.width, a.height), (p.width, p.height));
            let _ = extract_audio(&f);
        }
    }
}

#[test]
fn bit_flips_and_truncations_do_not_panic() {
    let mut rng = Rng(99);
    let p = &PROFILES[0];
    let base = frame_for(p, &mut rng, true);
    let mut d = DvVideoDecoder::new();
    for n in 0..200 {
        let mut f = base.clone();
        for _ in 0..(1 + n % 20) {
            let at = rng.next() as usize % f.len();
            f[at] ^= 1 << (rng.next() % 8);
        }
        let _ = d.decode(&f);
        let cut = rng.next() as usize % f.len();
        let _ = d.decode(&f[..cut]);
        let _ = extract_audio(&f[..cut]);
        let _ = extract_audio(&f);
    }
}

#[test]
fn all_headers_with_hostile_audio_packs() {
    let mut rng = Rng(5);
    for p in PROFILES.iter() {
        for _ in 0..20 {
            let mut f = frame_for(p, &mut rng, true);
            for c in 0..10usize {
                let offs = if c & 1 == 1 { 80 * 6 + 3 + c * 12000 } else { 80 * 6 + 80 * 16 * 3 + 3 + c * 12000 };
                if offs + 5 < f.len() {
                    f[offs] = 0x50;
                    f[offs + 1] = rng.next() as u8;
                    f[offs + 3] = rng.next() as u8;
                    f[offs + 4] = rng.next() as u8;
                }
            }
            let _ = extract_audio(&f);
            let _ = DvVideoDecoder::new().decode(&f);
        }
    }
}
