//! Visual object / VOL / VOP header parsing (ISO/IEC 14496-2) and the H.263
//! baseline picture header, with the same acceptance as FFmpeg's decoder.

use ec_core::error::{Error, Result};

use crate::bits::Br;
use crate::tables::*;
use crate::vlc::vlcs;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pt {
    I,
    P,
    B,
    S,
}

#[derive(Clone)]
pub struct Vol {
    pub vo_type: u32,
    pub vo_ver_id: u32,
    pub width: u32,
    pub height: u32,
    pub low_delay: bool,
    pub vol_control_parameters: bool,
    pub progressive_sequence: bool,
    pub time_inc_res: u32,
    pub fixed_vop_rate_den: u32,
    pub time_increment_bits: u32,
    pub sprite_usage: u32,
    pub num_sprite_warping_points: u32,
    pub sprite_warping_accuracy: u32,
    pub sprite_brightness_change: bool,
    pub quant_precision: u32,
    pub mpeg_quant: bool,
    pub intra_matrix: [u16; 64],
    pub inter_matrix: [u16; 64],
    pub quarter_sample: bool,
    pub resync_marker: bool,
    pub data_partitioning: bool,
    pub rvlc: bool,
    pub cplx_trash_i: u32,
    pub cplx_trash_p: u32,
    pub cplx_trash_b: u32,
    pub sar: (u32, u32),
}

impl Default for Vol {
    fn default() -> Vol {
        let mut v = Vol {
            vo_type: 0,
            vo_ver_id: 1,
            width: 0,
            height: 0,
            low_delay: false,
            vol_control_parameters: false,
            progressive_sequence: true,
            time_inc_res: 0,
            fixed_vop_rate_den: 1,
            time_increment_bits: 4,
            sprite_usage: 0,
            num_sprite_warping_points: 0,
            sprite_warping_accuracy: 0,
            sprite_brightness_change: false,
            quant_precision: 5,
            mpeg_quant: false,
            intra_matrix: [0; 64],
            inter_matrix: [0; 64],
            quarter_sample: false,
            resync_marker: false,
            data_partitioning: false,
            rvlc: false,
            cplx_trash_i: 0,
            cplx_trash_p: 0,
            cplx_trash_b: 0,
            sar: (1, 1),
        };
        v.load_default_matrices();
        v
    }
}

impl Vol {
    pub fn load_default_matrices(&mut self) {
        for i in 0..64 {
            self.intra_matrix[i] = MPEG4_DEFAULT_INTRA_MATRIX[i] as u16;
            self.inter_matrix[i] = MPEG4_DEFAULT_NON_INTRA_MATRIX[i] as u16;
        }
    }
}

/// Encoder identification gathered from user data / FourCC, as FFmpeg does.
#[derive(Clone, Copy, Debug)]
pub struct Ident {
    pub divx_version: i32,
    pub divx_build: i32,
    pub xvid_build: i32,
    pub lavc_build: i32,
    pub divx_packed: bool,
}

impl Default for Ident {
    fn default() -> Ident {
        Ident {
            divx_version: -1,
            divx_build: -1,
            xvid_build: -1,
            lavc_build: -1,
            divx_packed: false,
        }
    }
}

fn parse_i(s: &[u8], mut i: usize) -> Option<(i64, usize)> {
    // like sscanf %d: optional sign, digits
    let st = i;
    if i < s.len() && (s[i] == b'-' || s[i] == b'+') {
        i += 1;
    }
    let ds = i;
    while i < s.len() && s[i].is_ascii_digit() {
        i += 1;
    }
    if i == ds {
        return None;
    }
    std::str::from_utf8(&s[st..i])
        .ok()?
        .parse::<i64>()
        .ok()
        .map(|v| (v, i))
}

impl Ident {
    /// FFmpeg's `decode_user_data`.
    pub fn parse_user_data(&mut self, buf: &[u8]) {
        // DivX%dBuild%d%c or DivX%db%d%c
        let mut e = 0;
        let (mut ver, mut build, mut last) = (0i64, 0i64, 0u8);
        if let Some(rest) = buf.strip_prefix(b"DivX")
            && let Some((v, i)) = parse_i(rest, 0)
        {
            if let Some(r2) = rest[i..].strip_prefix(b"Build")
                && let Some((b, j)) = parse_i(r2, 0)
            {
                ver = v;
                build = b;
                e = 2;
                if let Some(&c) = r2.get(j) {
                    last = c;
                    e = 3;
                }
            }
            if e < 2
                && let Some(r2) = rest[i..].strip_prefix(b"b")
                && let Some((b, j)) = parse_i(r2, 0)
            {
                ver = v;
                build = b;
                e = 2;
                if let Some(&c) = r2.get(j) {
                    last = c;
                    e = 3;
                }
            }
        }
        if e >= 2 {
            self.divx_version = ver as i32;
            self.divx_build = build as i32;
            self.divx_packed = e == 3 && last == b'p';
        }
        // libavcodec detection
        let mut got = false;
        if let Some(pos) = find(buf, b"FFmpe") {
            // "FFmpe%*[^b]b%d"
            let mut i = pos + 5;
            while i < buf.len() && buf[i] != b'b' {
                i += 1;
            }
            if i < buf.len()
                && let Some((b, _)) = parse_i(buf, i + 1)
            {
                self.lavc_build = b as i32;
                got = true;
            }
        }
        if !got && buf.starts_with(b"FFmpeg v") {
            // "FFmpeg v%d.%d.%d / libavcodec build: %d"
            let s = &buf[8..];
            if let Some(p) = find(s, b"build:")
                && let Some((b, _)) = parse_i(s, p + 6).or_else(|| parse_i(s, p + 7))
            {
                self.lavc_build = b as i32;
                got = true;
            }
        }
        if !got
            && let Some(r) = buf.strip_prefix(b"Lavc")
            && let Some((v1, i)) = parse_i(r, 0)
            && r.get(i) == Some(&b'.')
            && let Some((v2, j)) = parse_i(r, i + 1)
            && r.get(j) == Some(&b'.')
            && let Some((v3, _)) = parse_i(r, j + 1)
        {
            self.lavc_build = (((v1 & 0xff) << 16) + ((v2 & 0xff) << 8) + (v3 & 0xff)) as i32;
            got = true;
        }
        if !got && buf == b"ffmpeg" {
            self.lavc_build = 4600;
        }
        // Xvid detection: "XviD%d"
        if let Some(r) = buf.strip_prefix(b"XviD")
            && let Some((b, _)) = parse_i(r, 0)
        {
            self.xvid_build = b as i32;
        }
    }
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

fn marker(br: &mut Br) {
    br.skip(1);
}

fn unsupported(what: &str) -> Error {
    Error::unsupported("MPEG-4 Part 2", what.to_string())
}

/// Parse a video_object_layer after its start code. `width`/`height` of 0
/// keep the old values (as FFmpeg).
pub fn parse_vol(br: &mut Br, vol: &mut Vol, picture_number: u64) -> Result<()> {
    br.skip(1); // random access
    vol.vo_type = br.get(8);
    if vol.vo_type == 0x0E || vol.vo_type == 0x0F {
        return Err(unsupported("Simple Studio / Core Studio profile"));
    }
    if br.bit() != 0 {
        vol.vo_ver_id = br.get(4);
        br.skip(3);
    } else {
        vol.vo_ver_id = 1;
    }
    let aspect = br.get(4);
    if aspect == 15 {
        let n = br.get(8);
        let d = br.get(8);
        vol.sar = (n, d);
    } else {
        let a = H263_PIXEL_ASPECT[aspect as usize];
        vol.sar = (a[0] as u32, a[1] as u32);
    }
    vol.vol_control_parameters = br.bit() != 0;
    if vol.vol_control_parameters {
        br.skip(2); // chroma format
        vol.low_delay = br.bit() != 0;
        if br.bit() != 0 {
            br.skip(15 + 1 + 15 + 1 + 15 + 1 + 3 + 11 + 1 + 15 + 1);
        }
    } else if picture_number == 0 {
        vol.low_delay = matches!(vol.vo_type, 1 | 17);
    }
    let shape = br.get(2);
    if shape != 0 {
        return Err(unsupported(
            "non-rectangular video object layer (shape coding)",
        ));
    }
    marker(br);
    vol.time_inc_res = br.get(16);
    if vol.time_inc_res == 0 {
        return Err(Error::corrupt("MPEG-4 VOL: time_increment_resolution 0"));
    }
    vol.time_increment_bits = (32 - (vol.time_inc_res - 1).leading_zeros()).max(1);
    marker(br);
    if br.bit() != 0 {
        vol.fixed_vop_rate_den = br.get(vol.time_increment_bits);
    } else {
        vol.fixed_vop_rate_den = 1;
    }
    marker(br);
    let width = br.get(13);
    marker(br);
    let height = br.get(13);
    marker(br);
    if width != 0 && height != 0 {
        vol.width = width;
        vol.height = height;
    }
    vol.progressive_sequence = br.bit() ^ 1 != 0;
    br.skip(1); // obmc disable
    vol.sprite_usage = if vol.vo_ver_id == 1 {
        br.get(1)
    } else {
        br.get(2)
    };
    if vol.sprite_usage == 1 {
        return Err(unsupported("static sprite (sprite_enable=1)"));
    }
    if vol.sprite_usage == 2 {
        vol.num_sprite_warping_points = br.get(6);
        if vol.num_sprite_warping_points > 3 {
            vol.num_sprite_warping_points = 0;
            return Err(Error::corrupt(
                "MPEG-4 VOL: more than 3 sprite warping points",
            ));
        }
        vol.sprite_warping_accuracy = br.get(2);
        vol.sprite_brightness_change = br.bit() != 0;
        if vol.sprite_brightness_change {
            return Err(unsupported("GMC sprite brightness change"));
        }
    }
    if br.bit() == 1 {
        vol.quant_precision = br.get(4);
        if br.get(4) != 8 {
            return Err(unsupported("bits_per_pixel other than 8"));
        }
        if !(3..=9).contains(&vol.quant_precision) {
            vol.quant_precision = 5;
        }
    } else {
        vol.quant_precision = 5;
    }
    vol.mpeg_quant = br.bit() != 0;
    if vol.mpeg_quant {
        vol.load_default_matrices();
        if br.bit() != 0 {
            let mut last = 0u32;
            let mut i = 0;
            while i < 64 {
                if br.left() < 8 {
                    return Err(Error::corrupt(
                        "MPEG-4 VOL: insufficient data for custom matrix",
                    ));
                }
                let v = br.get(8);
                if v == 0 {
                    break;
                }
                last = v;
                vol.intra_matrix[ZIGZAG_DIRECT[i] as usize] = v as u16;
                i += 1;
            }
            while i < 64 {
                vol.intra_matrix[ZIGZAG_DIRECT[i] as usize] = last as u16;
                i += 1;
            }
        }
        if br.bit() != 0 {
            let mut last = 0u32;
            let mut i = 0;
            while i < 64 {
                if br.left() < 8 {
                    return Err(Error::corrupt(
                        "MPEG-4 VOL: insufficient data for custom matrix",
                    ));
                }
                let v = br.get(8);
                if v == 0 {
                    break;
                }
                last = v;
                vol.inter_matrix[ZIGZAG_DIRECT[i] as usize] = v as u16;
                i += 1;
            }
            while i < 64 {
                vol.inter_matrix[ZIGZAG_DIRECT[i] as usize] = last as u16;
                i += 1;
            }
        }
    }
    vol.quarter_sample = if vol.vo_ver_id != 1 {
        br.bit() != 0
    } else {
        false
    };
    if br.left() < 4 {
        return Err(Error::corrupt("MPEG-4 VOL header truncated"));
    }
    vol.cplx_trash_i = 0;
    vol.cplx_trash_p = 0;
    vol.cplx_trash_b = 0;
    if br.bit() == 0 {
        // complexity_estimation_disable == 0
        let pos = br.pos();
        let method = br.get(2);
        let mut ok = true;
        if method < 2 {
            let (mut ti, mut tp, mut tb) = (0u32, 0u32, 0u32);
            if br.bit() == 0 {
                for _ in 0..6 {
                    ti += 8 * br.bit();
                }
            }
            if br.bit() == 0 {
                ti += 8 * br.bit();
                tp += 8 * br.bit();
                tp += 8 * br.bit();
                ti += 8 * br.bit();
            }
            if br.bit() == 0 {
                ok = false;
            } else {
                if br.bit() == 0 {
                    ti += 8 * br.bit();
                    ti += 8 * br.bit();
                    ti += 8 * br.bit();
                    ti += 4 * br.bit();
                }
                if br.bit() == 0 {
                    tp += 8 * br.bit();
                    tp += 8 * br.bit();
                    tb += 8 * br.bit();
                    tp += 8 * br.bit();
                    tp += 8 * br.bit();
                    tp += 8 * br.bit();
                }
                if br.bit() == 0 {
                    ok = false;
                } else if method == 1 {
                    ti += 8 * br.bit();
                    tp += 8 * br.bit();
                }
            }
            if ok {
                vol.cplx_trash_i = ti;
                vol.cplx_trash_p = tp;
                vol.cplx_trash_b = tb;
            } else {
                br.set_pos(pos);
                // FFmpeg's `no_cplx_est` jump: counters stay zero and parsing
                // continues at the saved position (which is the marker-failed
                // case); mirror that.
            }
        }
    }
    vol.resync_marker = br.bit() == 0;
    vol.data_partitioning = br.bit() != 0;
    vol.rvlc = if vol.data_partitioning {
        br.bit() != 0
    } else {
        false
    };
    if vol.vo_ver_id != 1 {
        if br.bit() != 0 {
            return Err(unsupported("NEWPRED"));
        }
        if br.bit() != 0 {
            return Err(unsupported("reduced resolution VOP"));
        }
    }
    if br.bit() != 0 {
        // scalability: FFmpeg accepts a malformed header only when a sampling
        // factor is zero; a real scalable layer is refused.
        let save = br.pos();
        br.skip(1 + 4 + 1);
        let hn = br.get(5);
        let hm = br.get(5);
        let vn = br.get(5);
        let vm = br.get(5);
        br.skip(1);
        if hn == 0 || hm == 0 || vn == 0 || vm == 0 {
            br.set_pos(save);
        } else {
            return Err(unsupported("scalability (enhancement layer)"));
        }
    }
    Ok(())
}

/// Parsed VOP header fields.
#[derive(Clone, Default)]
pub struct Vop {
    pub pict_type: Option<Pt>,
    pub coded: bool,
    pub no_rounding: bool,
    pub intra_dc_threshold: i32,
    pub top_field_first: bool,
    pub alternate_scan: bool,
    pub qscale: i32,
    pub f_code: i32,
    pub b_code: i32,
}

/// Time-base bookkeeping carried between VOPs.
#[derive(Clone, Default)]
pub struct TimeState {
    pub time_base: i32,
    pub last_time_base: i32,
    pub time: i64,
    pub last_non_b_time: i64,
    pub pp_time: u16,
    pub pb_time: u16,
    pub pp_field_time: u16,
    pub pb_field_time: u16,
    pub t_frame: i64,
}

fn rounded_div(a: i64, b: i64) -> i64 {
    if a >= 0 {
        (a + (b >> 1)) / b
    } else {
        -((-a + (b >> 1)) / b)
    }
}

pub enum VopResult {
    /// Decode this VOP.
    Coded,
    /// Header says nothing to decode (not-coded VOP or out-of-order B).
    Skipped { not_coded: bool },
}

/// Parse a VOP header (after the 0x1B6 start code). Mirrors `decode_vop_header`.
pub fn parse_vop(
    br: &mut Br,
    vol: &mut Vol,
    ts: &mut TimeState,
    vop: &mut Vop,
    _ident: &Ident,
    bug_ump4: bool,
    picture_number: u64,
) -> Result<VopResult> {
    let pt = match br.get(2) {
        0 => Pt::I,
        1 => Pt::P,
        2 => Pt::B,
        _ => Pt::S,
    };
    vop.pict_type = Some(pt);
    if pt == Pt::B && vol.low_delay && !vol.vol_control_parameters {
        vol.low_delay = false;
    }
    let mut time_incr = 0i64;
    while br.bit() != 0 {
        time_incr += 1;
        if br.left() <= 0 {
            return Err(Error::corrupt("MPEG-4 VOP: runaway modulo_time_base"));
        }
    }
    marker(br);
    if vol.time_increment_bits == 0 || br.peek(vol.time_increment_bits + 1) & 1 == 0 {
        // wrong time_increment_bits: guess from the bitstream
        let mut found = false;
        for n in 1..16 {
            vol.time_increment_bits = n;
            if pt == Pt::P || (pt == Pt::S && vol.sprite_usage == 2) {
                if br.peek(n + 6) & 0x37 == 0x30 {
                    found = true;
                    break;
                }
            } else if br.peek(n + 5) & 0x1F == 0x18 {
                found = true;
                break;
            }
        }
        if !found {
            vol.time_increment_bits = 16;
        }
    }
    let time_increment = br.get(vol.time_increment_bits) as i64;
    let res = vol.time_inc_res as i64;
    if pt != Pt::B {
        ts.last_time_base = ts.time_base;
        ts.time_base = ts.time_base.wrapping_add(time_incr as i32);
        ts.time = ts.time_base as i64 * res + time_increment;
        if bug_ump4 && ts.time < ts.last_non_b_time {
            ts.time_base += 1;
            ts.time += res;
        }
        ts.pp_time = (ts.time - ts.last_non_b_time) as u16;
        ts.last_non_b_time = ts.time;
    } else {
        ts.time = (ts.last_time_base as i64 + time_incr) * res + time_increment;
        ts.pb_time = (ts.pp_time as i64 - (ts.last_non_b_time - ts.time)) as u16;
        let (pp, pb) = (ts.pp_time as i32, ts.pb_time as i32);
        if pp <= pb || pp <= pp - pb || pp <= 0 {
            return Ok(VopResult::Skipped { not_coded: false });
        }
        if ts.t_frame == 0 {
            ts.t_frame = ts.pb_time as i64;
        }
        if ts.t_frame == 0 {
            ts.t_frame = 1;
        }
        let tf = ts.t_frame;
        ts.pp_field_time = ((rounded_div(ts.last_non_b_time, tf)
            - rounded_div(ts.last_non_b_time - ts.pp_time as i64, tf))
            * 2) as u16;
        ts.pb_field_time = ((rounded_div(ts.time, tf)
            - rounded_div(ts.last_non_b_time - ts.pp_time as i64, tf))
            * 2) as u16;
        if ts.pp_field_time <= ts.pb_field_time || ts.pb_field_time <= 1 {
            ts.pb_field_time = 2;
            ts.pp_field_time = 4;
            if !vol.progressive_sequence {
                return Ok(VopResult::Skipped { not_coded: false });
            }
        }
    }
    marker(br);
    if br.bit() != 1 {
        return Ok(VopResult::Skipped { not_coded: true });
    }
    vop.coded = true;
    vop.no_rounding = if pt == Pt::P || (pt == Pt::S && vol.sprite_usage == 2) {
        br.bit() != 0
    } else {
        false
    };
    // complexity estimation trash bits
    br.skip(vol.cplx_trash_i);
    if pt != Pt::I {
        br.skip(vol.cplx_trash_p);
    }
    if pt == Pt::B {
        br.skip(vol.cplx_trash_b);
    }
    if br.left() < 3 {
        return Err(Error::corrupt("MPEG-4 VOP header truncated"));
    }
    vop.intra_dc_threshold = MPEG4_DC_THRESHOLD[br.get(3) as usize] as i32;
    if !vol.progressive_sequence {
        vop.top_field_first = br.bit() != 0;
        vop.alternate_scan = br.bit() != 0;
    } else {
        vop.alternate_scan = false;
    }
    let _ = picture_number;
    Ok(VopResult::Coded)
}

/// Rest of the VOP header: sprite trajectory, quant, f_code, b_code. Sprite
/// parsing needs the display size, so it is separate.
pub fn parse_vop_tail(
    br: &mut Br,
    vol: &Vol,
    vop: &mut Vop,
    sprite: Option<&mut SpriteState>,
    ident: &Ident,
) -> Result<()> {
    let pt = vop.pict_type.unwrap();
    if pt == Pt::S && vol.sprite_usage == 2 {
        let sp = sprite.unwrap();
        parse_sprite_trajectory(br, vol, sp, ident)?;
    }
    vop.f_code = 1;
    vop.b_code = 1;
    vop.qscale = br.get(vol.quant_precision) as i32;
    if vop.qscale == 0 {
        return Err(Error::corrupt("MPEG-4 VOP: qscale 0"));
    }
    if pt != Pt::I {
        vop.f_code = br.get(3) as i32;
        if vop.f_code == 0 {
            vop.f_code = 1;
            return Err(Error::corrupt("MPEG-4 VOP: f_code 0"));
        }
    }
    if pt == Pt::B {
        vop.b_code = br.get(3) as i32;
        if vop.b_code == 0 {
            vop.b_code = 1;
            return Err(Error::corrupt("MPEG-4 VOP: b_code 0"));
        }
    }
    Ok(())
}

#[derive(Clone, Default)]
pub struct SpriteState {
    pub offset: [[i64; 2]; 2],
    pub delta: [[i64; 2]; 2],
    pub shift: [i32; 2],
    pub real_points: u32,
    pub width: i32,
    pub height: i32,
}

/// `mpeg4_decode_sprite_trajectory` for a rectangular VOL.
pub fn parse_sprite_trajectory(
    br: &mut Br,
    vol: &Vol,
    sp: &mut SpriteState,
    ident: &Ident,
) -> Result<()> {
    let accuracy = vol.sprite_warping_accuracy as i32;
    let a: i64 = 2i64 << accuracy;
    let rho: i32 = 3 - accuracy;
    let r: i64 = 16 / a;
    let w = sp.width as i64;
    let h = sp.height as i64;
    if w <= 0 || h <= 0 {
        return Err(Error::corrupt("MPEG-4 sprite: empty picture"));
    }
    let npts = vol.num_sprite_warping_points as usize;
    let vlc = &vlcs().sprite_traj;
    let div500_413 = ident.divx_version == 500 && ident.divx_build == 413;
    let mut d = [[0i64; 2]; 4];
    for i in 0..npts {
        let mut x = 0;
        let mut y = 0;
        let len = vlc
            .get(br)
            .ok_or_else(|| Error::corrupt("MPEG-4 sprite trajectory vlc"))?;
        if len > 0 {
            x = br.xbits(len) as i64;
        }
        if !div500_413 {
            marker(br);
        }
        let len = vlc
            .get(br)
            .ok_or_else(|| Error::corrupt("MPEG-4 sprite trajectory vlc"))?;
        if len > 0 {
            y = br.xbits(len) as i64;
        }
        marker(br);
        d[i] = [x, y];
    }
    let mut alpha = 1;
    let mut beta = 0;
    while (1i64 << alpha) < w {
        alpha += 1;
    }
    while (1i64 << beta) < h {
        beta += 1;
    }
    let w2 = 1i64 << alpha;
    let h2 = 1i64 << beta;
    let vop_ref = [[0i64, 0], [w, 0], [0, h], [w, h]];
    let mut sprite_ref = [[0i64; 2]; 4];
    if div500_413 {
        sprite_ref[0] = [a * vop_ref[0][0] + d[0][0], a * vop_ref[0][1] + d[0][1]];
        sprite_ref[1] = [
            a * vop_ref[1][0] + d[0][0] + d[1][0],
            a * vop_ref[1][1] + d[0][1] + d[1][1],
        ];
        sprite_ref[2] = [
            a * vop_ref[2][0] + d[0][0] + d[2][0],
            a * vop_ref[2][1] + d[0][1] + d[2][1],
        ];
    } else {
        let ah = a >> 1;
        sprite_ref[0] = [
            ah * (2 * vop_ref[0][0] + d[0][0]),
            ah * (2 * vop_ref[0][1] + d[0][1]),
        ];
        sprite_ref[1] = [
            ah * (2 * vop_ref[1][0] + d[0][0] + d[1][0]),
            ah * (2 * vop_ref[1][1] + d[0][1] + d[1][1]),
        ];
        sprite_ref[2] = [
            ah * (2 * vop_ref[2][0] + d[0][0] + d[2][0]),
            ah * (2 * vop_ref[2][1] + d[0][1] + d[2][1]),
        ];
    }
    let virtual_ref = [
        [
            16 * (vop_ref[0][0] + w2)
                + rounded_div(
                    (w - w2) * (r * sprite_ref[0][0] - 16 * vop_ref[0][0])
                        + w2 * (r * sprite_ref[1][0] - 16 * vop_ref[1][0]),
                    w,
                ),
            16 * vop_ref[0][1]
                + rounded_div(
                    (w - w2) * (r * sprite_ref[0][1] - 16 * vop_ref[0][1])
                        + w2 * (r * sprite_ref[1][1] - 16 * vop_ref[1][1]),
                    w,
                ),
        ],
        [
            16 * vop_ref[0][0]
                + rounded_div(
                    (h - h2) * (r * sprite_ref[0][0] - 16 * vop_ref[0][0])
                        + h2 * (r * sprite_ref[2][0] - 16 * vop_ref[2][0]),
                    h,
                ),
            16 * (vop_ref[0][1] + h2)
                + rounded_div(
                    (h - h2) * (r * sprite_ref[0][1] - 16 * vop_ref[0][1])
                        + h2 * (r * sprite_ref[2][1] - 16 * vop_ref[2][1]),
                    h,
                ),
        ],
    ];
    let mut off = [[0i64; 2]; 2];
    let mut delta = [[0i64; 2]; 2];
    let mut shift = [0i32; 2];
    match npts {
        0 => {
            delta = [[a, 0], [0, a]];
        }
        1 => {
            off[0][0] = sprite_ref[0][0] - a * vop_ref[0][0];
            off[0][1] = sprite_ref[0][1] - a * vop_ref[0][1];
            off[1][0] =
                ((sprite_ref[0][0] >> 1) | (sprite_ref[0][0] & 1)) - a * (vop_ref[0][0] / 2);
            off[1][1] =
                ((sprite_ref[0][1] >> 1) | (sprite_ref[0][1] & 1)) - a * (vop_ref[0][1] / 2);
            delta = [[a, 0], [0, a]];
        }
        2 => {
            let (ar, rho_) = (alpha, rho);
            off[0][0] = (sprite_ref[0][0] * (1i64 << (ar + rho_)))
                + (-r * sprite_ref[0][0] + virtual_ref[0][0]) * (-vop_ref[0][0])
                + (r * sprite_ref[0][1] - virtual_ref[0][1]) * (-vop_ref[0][1])
                + (1i64 << (ar + rho_ - 1));
            off[0][1] = (sprite_ref[0][1] * (1i64 << (ar + rho_)))
                + (-r * sprite_ref[0][1] + virtual_ref[0][1]) * (-vop_ref[0][0])
                + (-r * sprite_ref[0][0] + virtual_ref[0][0]) * (-vop_ref[0][1])
                + (1i64 << (ar + rho_ - 1));
            off[1][0] = (-r * sprite_ref[0][0] + virtual_ref[0][0]) * (-2 * vop_ref[0][0] + 1)
                + (r * sprite_ref[0][1] - virtual_ref[0][1]) * (-2 * vop_ref[0][1] + 1)
                + 2 * w2 * r * sprite_ref[0][0]
                - 16 * w2
                + (1i64 << (ar + rho_ + 1));
            off[1][1] = (-r * sprite_ref[0][1] + virtual_ref[0][1]) * (-2 * vop_ref[0][0] + 1)
                + (-r * sprite_ref[0][0] + virtual_ref[0][0]) * (-2 * vop_ref[0][1] + 1)
                + 2 * w2 * r * sprite_ref[0][1]
                - 16 * w2
                + (1i64 << (ar + rho_ + 1));
            delta[0][0] = -r * sprite_ref[0][0] + virtual_ref[0][0];
            delta[0][1] = r * sprite_ref[0][1] - virtual_ref[0][1];
            delta[1][0] = -r * sprite_ref[0][1] + virtual_ref[0][1];
            delta[1][1] = -r * sprite_ref[0][0] + virtual_ref[0][0];
            shift = [ar + rho_, ar + rho_ + 2];
        }
        _ => {
            let min_ab = alpha.min(beta);
            let w3 = w2 >> min_ab;
            let h3 = h2 >> min_ab;
            let s0 = alpha + beta + rho - min_ab;
            off[0][0] = (sprite_ref[0][0] * (1i64 << s0))
                + (-r * sprite_ref[0][0] + virtual_ref[0][0]) * h3 * (-vop_ref[0][0])
                + (-r * sprite_ref[0][0] + virtual_ref[1][0]) * w3 * (-vop_ref[0][1])
                + (1i64 << (s0 - 1));
            off[0][1] = (sprite_ref[0][1] * (1i64 << s0))
                + (-r * sprite_ref[0][1] + virtual_ref[0][1]) * h3 * (-vop_ref[0][0])
                + (-r * sprite_ref[0][1] + virtual_ref[1][1]) * w3 * (-vop_ref[0][1])
                + (1i64 << (s0 - 1));
            off[1][0] = (-r * sprite_ref[0][0] + virtual_ref[0][0]) * h3 * (-2 * vop_ref[0][0] + 1)
                + (-r * sprite_ref[0][0] + virtual_ref[1][0]) * w3 * (-2 * vop_ref[0][1] + 1)
                + 2 * w2 * h3 * r * sprite_ref[0][0]
                - 16 * w2 * h3
                + (1i64 << (s0 + 1));
            off[1][1] = (-r * sprite_ref[0][1] + virtual_ref[0][1]) * h3 * (-2 * vop_ref[0][0] + 1)
                + (-r * sprite_ref[0][1] + virtual_ref[1][1]) * w3 * (-2 * vop_ref[0][1] + 1)
                + 2 * w2 * h3 * r * sprite_ref[0][1]
                - 16 * w2 * h3
                + (1i64 << (s0 + 1));
            delta[0][0] = (-r * sprite_ref[0][0] + virtual_ref[0][0]) * h3;
            delta[0][1] = (-r * sprite_ref[0][0] + virtual_ref[1][0]) * w3;
            delta[1][0] = (-r * sprite_ref[0][1] + virtual_ref[0][1]) * h3;
            delta[1][1] = (-r * sprite_ref[0][1] + virtual_ref[1][1]) * w3;
            shift = [s0, s0 + 2];
        }
    }
    if delta[0][0] == a << shift[0]
        && delta[0][1] == 0
        && delta[1][0] == 0
        && delta[1][1] == a << shift[0]
    {
        off[0][0] >>= shift[0];
        off[0][1] >>= shift[0];
        off[1][0] >>= shift[1];
        off[1][1] >>= shift[1];
        delta = [[a, 0], [0, a]];
        shift = [0, 0];
        sp.real_points = 1;
    } else {
        let shift_y = 16 - shift[0];
        let shift_c = 16 - shift[1];
        let lim = i32::MAX as i64;
        for i in 0..2 {
            if shift_c < 0
                || shift_y < 0
                || off[0][i].abs() >= lim >> shift_y
                || off[1][i].abs() >= lim >> shift_c
                || delta[0][i].abs() >= lim >> shift_y
                || delta[1][i].abs() >= lim >> shift_y
            {
                sp.offset = [[0; 2]; 2];
                sp.delta = [[0; 2]; 2];
                return Err(unsupported("GMC: sprite shift, delta or offset too large"));
            }
        }
        for i in 0..2 {
            off[0][i] *= 1i64 << shift_y;
            off[1][i] *= 1i64 << shift_c;
            delta[0][i] *= 1i64 << shift_y;
            delta[1][i] *= 1i64 << shift_y;
        }
        shift = [16, 16];
        for i in 0..2 {
            let sd = [
                delta[i][0] - a * (1i64 << 16),
                delta[i][1] - a * (1i64 << 16),
            ];
            let (w16, h16) = (w + 16, h + 16);
            if (off[0][i] + delta[i][0] * w16).abs() >= lim
                || (off[0][i] + delta[i][1] * h16).abs() >= lim
                || (off[0][i] + delta[i][0] * w16 + delta[i][1] * h16).abs() >= lim
                || (delta[i][0] * w16).abs() >= lim
                || (delta[i][1] * h16).abs() >= lim
                || sd[0].abs() >= lim
                || sd[1].abs() >= lim
                || (off[0][i] + sd[0] * w16).abs() >= lim
                || (off[0][i] + sd[1] * h16).abs() >= lim
                || (off[0][i] + sd[0] * w16 + sd[1] * h16).abs() >= lim
            {
                sp.offset = [[0; 2]; 2];
                sp.delta = [[0; 2]; 2];
                return Err(unsupported("GMC: overflow on sprite points"));
            }
        }
        sp.real_points = vol.num_sprite_warping_points;
    }
    sp.shift = shift;
    sp.offset = off;
    sp.delta = delta;
    Ok(())
}
