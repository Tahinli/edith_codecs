//! AV1 film grain synthesis (spec 7.18.3), ported bit-exact from libaom's
//! `av1/decoder/grain_synthesis.c` (`add_film_grain_run` and its helpers),
//! specialized to this crate's only picture shape: 4:2:0 planar
//! (`chroma_subsamp_x = chroma_subsamp_y = 1` throughout, which is why every
//! `<< (1 - subsamp)` in the reference collapses to a no-op and every
//! `>> subsamp` collapses to `>> 1` below).
//!
//! Grain is synthesized only into the picture handed back to the caller for
//! *output* -- the picture stored as a reference for later inter prediction
//! stays the clean, pre-grain one (spec 7.18.3.1: the synthesized grain "is
//! not stored anywhere and is not used for predicting subsequent frames").
//! [`crate::stream::decode_stream`] is the only caller: it applies this to
//! the frame it pushes onto its output `Vec<Picture>`, after saving the
//! clean decode into the reference slot bank.

use crate::encode::Picture;
use ec_av1_syntax::FilmGrainParams;

// Samples with Gaussian distribution in the range of [-2048, 2047] (12 bits)
// with zero mean and standard deviation of about 512 -- libaom
// `av1/decoder/grain_synthesis.c`'s `gaussian_sequence`, transcribed
// verbatim (spec 7.18.3.3 names this table `Gaussian_Sequence`).
#[rustfmt::skip]
const GAUSSIAN_SEQUENCE: [i32; 2048] = [
    56, 568, -180, 172, 124, -84, 172, -64, -900, 24, 820, 224, 1248, 996, 272, -8, -916, -388, -732, -104, -188, 800, 112, -652, -320, -376, 140, -252, 492, -168, 44, -788, 588, -584, 500, -228, 12, 680, 272, -476, 972, -100, 652, 368, 432, -196, -720, -192, 1000, -332, 652, -136, -552, -604, -4, 192, -220, -136, 1000, -52, 372, -96, -624, 124, -24, 396, 540, -12, -104, 640, 464, 244, -208, -84, 368, -528, -740, 248, -968, -848, 608, 376, -60, -292, -40, -156, 252, -292, 248, 224, -280, 400, -244, 244, -60, 76, -80, 212, 532, 340, 128, -36, 824, -352, -60, -264, -96, -612, 416, -704, 220, -204, 640, -160, 1220, -408, 900, 336, 20, -336, -96, -792, 304, 48, -28, -1232, -1172, -448, 104, -292, -520, 244, 60, -948, 0, -708, 268, 108, 356, -548, 488, -344, -136, 488, -196, -224, 656, -236, -1128, 60, 4, 140, 276, -676, -376, 168, -108, 464, 8, 564, 64, 240, 308, -300, -400, -456, -136, 56, 120, -408, -116, 436, 504, -232, 328, 844, -164, -84, 784, -168, 232, -224, 348, -376, 128, 568, 96, -1244, -288, 276, 848, 832, -360, 656, 464, -384, -332, -356, 728, -388, 160, -192, 468, 296, 224, 140, -776, -100, 280, 4, 196, 44, -36, -648, 932, 16, 1428, 28, 528, 808, 772, 20, 268, 88, -332, -284, 124, -384, -448, 208, -228, -1044, -328, 660, 380, -148, -300, 588, 240, 540, 28, 136, -88, -436, 256, 296, -1000, 1400, 0, -48, 1056, -136, 264, -528, -1108, 632, -484, -592, -344, 796, 124, -668, -768, 388, 1296, -232, -188, -200, -288, -4, 308, 100, -168, 256, -500, 204, -508, 648, -136, 372, -272, -120, -1004, -552, -548, -384, 548, -296, 428, -108, -8, -912, -324, -224, -88, -112, -220, -100, 996, -796, 548, 360, -216, 180, 428, -200, -212, 148, 96, 148, 284, 216, -412, -320, 120, -300, -384, -604, -572, -332, -8, -180, -176, 696, 116, -88, 628, 76, 44, -516, 240, -208, -40, 100, -592, 344, -308, -452, -228, 20, 916, -1752, -136, -340, -804, 140, 40, 512, 340, 248, 184, -492, 896, -156, 932, -628, 328, -688, -448, -616, -752, -100, 560, -1020, 180, -800, -64, 76, 576, 1068, 396, 660, 552, -108, -28, 320, -628, 312, -92, -92, -472, 268, 16, 560, 516, -672, -52, 492, -100, 260, 384, 284, 292, 304, -148, 88, -152, 1012, 1064, -228, 164, -376, -684, 592, -392, 156, 196, -524, -64, -884, 160, -176, 636, 648, 404, -396, -436, 864, 424, -728, 988, -604, 904, -592, 296, -224, 536, -176, -920, 436, -48, 1176, -884, 416, -776, -824, -884, 524, -548, -564, -68, -164, -96, 692, 364, -692, -1012, -68, 260, -480, 876, -1116, 452, -332, -352, 892, -1088, 1220, -676, 12, -292, 244, 496, 372, -32, 280, 200, 112, -440, -96, 24, -644, -184, 56, -432, 224, -980, 272, -260, 144, -436, 420, 356, 364, -528, 76, 172, -744, -368, 404, -752, -416, 684, -688, 72, 540, 416, 92, 444, 480, -72, -1416, 164, -1172, -68, 24, 424, 264, 1040, 128, -912, -524, -356, 64, 876, -12, 4, -88, 532, 272, -524, 320, 276, -508, 940, 24, -400, -120, 756, 60, 236, -412, 100, 376, -484, 400, -100, -740, -108, -260, 328, -268, 224, -200, -416, 184, -604, -564, -20, 296, 60, 892, -888, 60, 164, 68, -760, 216, -296, 904, -336, -28, 404, -356, -568, -208, -1480, -512, 296, 328, -360, -164, -1560, -776, 1156, -428, 164, -504, -112, 120, -216, -148, -264, 308, 32, 64, -72, 72, 116, 176, -64, -272, 460, -536, -784, -280, 348, 108, -752, -132, 524, -540, -776, 116, -296, -1196, -288, -560, 1040, -472, 116, -848, -1116, 116, 636, 696, 284, -176, 1016, 204, -864, -648, -248, 356, 972, -584, -204, 264, 880, 528, -24, -184, 116, 448, -144, 828, 524, 212, -212, 52, 12, 200, 268, -488, -404, -880, 824, -672, -40, 908, -248, 500, 716, -576, 492, -576, 16, 720, -108, 384, 124, 344, 280, 576, -500, 252, 104, -308, 196, -188, -8, 1268, 296, 1032, -1196, 436, 316, 372, -432, -200, -660, 704, -224, 596, -132, 268, 32, -452, 884, 104, -1008, 424, -1348, -280, 4, -1168, 368, 476, 696, 300, -8, 24, 180, -592, -196, 388, 304, 500, 724, -160, 244, -84, 272, -256, -420, 320, 208, -144, -156, 156, 364, 452, 28, 540, 316, 220, -644, -248, 464, 72, 360, 32, -388, 496, -680, -48, 208, -116, -408, 60, -604, -392, 548, -840, 784, -460, 656, -544, -388, -264, 908, -800, -628, -612, -568, 572, -220, 164, 288, -16, -308, 308, -112, -636, -760, 280, -668, 432, 364, 240, -196, 604, 340, 384, 196, 592, -44, -500, 432, -580, -132, 636, -76, 392, 4, -412, 540, 508, 328, -356, -36, 16, -220, -64, -248, -60, 24, -192, 368, 1040, 92, -24, -1044, -32, 40, 104, 148, 192, -136, -520, 56, -816, -224, 732, 392, 356, 212, -80, -424, -1008, -324, 588, -1496, 576, 460, -816, -848, 56, -580, -92, -1372, -112, -496, 200, 364, 52, -140, 48, -48, -60, 84, 72, 40, 132, -356, -268, -104, -284, -404, 732, -520, 164, -304, -540, 120, 328, -76, -460, 756, 388, 588, 236, -436, -72, -176, -404, -316, -148, 716, -604, 404, -72, -88, -888, -68, 944, 88, -220, -344, 960, 472, 460, -232, 704, 120, 832, -228, 692, -508, 132, -476, 844, -748, -364, -44, 1116, -1104, -1056, 76, 428, 552, -692, 60, 356, 96, -384, -188, -612, -576, 736, 508, 892, 352, -1132, 504, -24, -352, 324, 332, -600, -312, 292, 508, -144, -8, 484, 48, 284, -260, -240, 256, -100, -292, -204, -44, 472, -204, 908, -188, -1000, -256, 92, 1164, -392, 564, 356, 652, -28, -884, 256, 484, -192, 760, -176, 376, -524, -452, -436, 860, -736, 212, 124, 504, -476, 468, 76, -472, 552, -692, -944, -620, 740, -240, 400, 132, 20, 192, -196, 264, -668, -1012, -60, 296, -316, -828, 76, -156, 284, -768, -448, -832, 148, 248, 652, 616, 1236, 288, -328, -400, -124, 588, 220, 520, -696, 1032, 768, -740, -92, -272, 296, 448, -464, 412, -200, 392, 440, -200, 264, -152, -260, 320, 1032, 216, 320, -8, -64, 156, -1016, 1084, 1172, 536, 484, -432, 132, 372, -52, -256, 84, 116, -352, 48, 116, 304, -384, 412, 924, -300, 528, 628, 180, 648, 44, -980, -220, 1320, 48, 332, 748, 524, -268, -720, 540, -276, 564, -344, -208, -196, 436, 896, 88, -392, 132, 80, -964, -288, 568, 56, -48, -456, 888, 8, 552, -156, -292, 948, 288, 128, -716, -292, 1192, -152, 876, 352, -600, -260, -812, -468, -28, -120, -32, -44, 1284, 496, 192, 464, 312, -76, -516, -380, -456, -1012, -48, 308, -156, 36, 492, -156, -808, 188, 1652, 68, -120, -116, 316, 160, -140, 352, 808, -416, 592, 316, -480, 56, 528, -204, -568, 372, -232, 752, -344, 744, -4, 324, -416, -600, 768, 268, -248, -88, -132, -420, -432, 80, -288, 404, -316, -1216, -588, 520, -108, 92, -320, 368, -480, -216, -92, 1688, -300, 180, 1020, -176, 820, -68, -228, -260, 436, -904, 20, 40, -508, 440, -736, 312, 332, 204, 760, -372, 728, 96, -20, -632, -520, -560, 336, 1076, -64, -532, 776, 584, 192, 396, -728, -520, 276, -188, 80, -52, -612, -252, -48, 648, 212, -688, 228, -52, -260, 428, -412, -272, -404, 180, 816, -796, 48, 152, 484, -88, -216, 988, 696, 188, -528, 648, -116, -180, 316, 476, 12, -564, 96, 476, -252, -364, -376, -392, 556, -256, -576, 260, -352, 120, -16, -136, -260, -492, 72, 556, 660, 580, 616, 772, 436, 424, -32, -324, -1268, 416, -324, -80, 920, 160, 228, 724, 32, -516, 64, 384, 68, -128, 136, 240, 248, -204, -68, 252, -932, -120, -480, -628, -84, 192, 852, -404, -288, -132, 204, 100, 168, -68, -196, -868, 460, 1080, 380, -80, 244, 0, 484, -888, 64, 184, 352, 600, 460, 164, 604, -196, 320, -64, 588, -184, 228, 12, 372, 48, -848, -344, 224, 208, -200, 484, 128, -20, 272, -468, -840, 384, 256, -720, -520, -464, -580, 112, -120, 644, -356, -208, -608, -528, 704, 560, -424, 392, 828, 40, 84, 200, -152, 0, -144, 584, 280, -120, 80, -556, -972, -196, -472, 724, 80, 168, -32, 88, 160, -688, 0, 160, 356, 372, -776, 740, -128, 676, -248, -480, 4, -364, 96, 544, 232, -1032, 956, 236, 356, 20, -40, 300, 24, -676, -596, 132, 1120, -104, 532, -1096, 568, 648, 444, 508, 380, 188, -376, -604, 1488, 424, 24, 756, -220, -192, 716, 120, 920, 688, 168, 44, -460, 568, 284, 1144, 1160, 600, 424, 888, 656, -356, -320, 220, 316, -176, -724, -188, -816, -628, -348, -228, -380, 1012, -452, -660, 736, 928, 404, -696, -72, -268, -892, 128, 184, -344, -780, 360, 336, 400, 344, 428, 548, -112, 136, -228, -216, -820, -516, 340, 92, -136, 116, -300, 376, -244, 100, -316, -520, -284, -12, 824, 164, -548, -180, -128, 116, -924, -828, 268, -368, -580, 620, 192, 160, 0, -1676, 1068, 424, -56, -360, 468, -156, 720, 288, -528, 556, -364, 548, -148, 504, 316, 152, -648, -620, -684, -24, -376, -384, -108, -920, -1032, 768, 180, -264, -508, -1268, -260, -60, 300, -240, 988, 724, -376, -576, -212, -736, 556, 192, 1092, -620, -880, 376, -56, -4, -216, -32, 836, 268, 396, 1332, 864, -600, 100, 56, -412, -92, 356, 180, 884, -468, -436, 292, -388, -804, -704, -840, 368, -348, 140, -724, 1536, 940, 372, 112, -372, 436, -480, 1136, 296, -32, -228, 132, -48, -220, 868, -1016, -60, -1044, -464, 328, 916, 244, 12, -736, -296, 360, 468, -376, -108, -92, 788, 368, -56, 544, 400, -672, -420, 728, 16, 320, 44, -284, -380, -796, 488, 132, 204, -596, -372, 88, -152, -908, -636, -572, -624, -116, -692, -200, -56, 276, -88, 484, -324, 948, 864, 1000, -456, -184, -276, 292, -296, 156, 676, 320, 160, 908, -84, -1236, -288, -116, 260, -372, -644, 732, -756, -96, 84, 344, -520, 348, -688, 240, -84, 216, -1044, -136, -676, -396, -1500, 960, -40, 176, 168, 1516, 420, -504, -344, -364, -360, 1216, -940, -380, -212, 252, -660, -708, 484, -444, -152, 928, -120, 1112, 476, -260, 560, -148, -344, 108, -196, 228, -288, 504, 560, -328, -88, 288, -1008, 460, -228, 468, -836, -196, 76, 388, 232, 412, -1168, -716, -644, 756, -172, -356, -504, 116, 432, 528, 48, 476, -168, -608, 448, 160, -532, -272, 28, -676, -12, 828, 980, 456, 520, 104, -104, 256, -344, -4, -28, -368, -52, -524, -572, -556, -200, 768, 1124, -208, -512, 176, 232, 248, -148, -888, 604, -600, -304, 804, -156, -212, 488, -192, -804, -256, 368, -360, -916, -328, 228, -240, -448, -472, 856, -556, -364, 572, -12, -156, -368, -340, 432, 252, -752, -152, 288, 268, -580, -848, -592, 108, -76, 244, 312, -716, 592, -80, 436, 360, 4, -248, 160, 516, 584, 732, 44, -468, -280, -292, -156, -588, 28, 308, 912, 24, 124, 156, 180, -252, 944, -924, -772, -520, -428, -624, 300, -212, -1144, 32, -724, 800, -1128, -212, -1288, -848, 180, -416, 440, 192, -576, -792, -76, -1080, 80, -532, -352, -132, 380, -820, 148, 1112, 128, 164, 456, 700, -924, 144, -668, -384, 648, -832, 508, 552, -52, -100, -656, 208, -568, 748, -88, 680, 232, 300, 192, -408, -1012, -152, -252, -268, 272, -876, -664, -648, -332, -136, 16, 12, 1152, -28, 332, -536, 320, -672, -460, -316, 532, -260, 228, -40, 1052, -816, 180, 88, -496, -556, -672, -368, 428, 92, 356, 404, -408, 252, 196, -176, -556, 792, 268, 32, 372, 40, 96, -332, 328, 120, 372, -900, -40, 472, -264, -592, 952, 128, 656, 112, 664, -232, 420, 4, -344, -464, 556, 244, -416, -32, 252, 0, -412, 188, -696, 508, -476, 324, -1096, 656, -312, 560, 264, -136, 304, 160, -64, -580, 248, 336, -720, 560, -348, -288, -276, -196, -500, 852, -544, -236, -1128, -992, -776, 116, 56, 52, 860, 884, 212, -12, 168, 1020, 512, -552, 924, -148, 716, 188, 164, -340, -520, -184, 880, -152, -680, -208, -1156, -300, -528, -472, 364, 100, -744, -1056, -32, 540, 280, 144, -676, -32, -232, -280, -224, 96, 568, -76, 172, 148, 148, 104, 32, -296, -32, 788, -80, 32, -16, 280, 288, 944, 428, -484,
];

const GAUSS_BITS: u32 = 11;

const LEFT_PAD: i32 = 3;
const RIGHT_PAD: i32 = 3;
const TOP_PAD: i32 = 3;
const BOTTOM_PAD: i32 = 0;
const AR_PADDING: i32 = 3;

const LUMA_SUBBLOCK: i32 = 32;
const CHROMA_SUBBLOCK: i32 = 16; // LUMA_SUBBLOCK >> subsampling (always 1 here)

// 73 x 82, and 38 x 44 for chroma -- the sizes the mission's own recon named.
const LUMA_BLOCK_H: i32 = TOP_PAD + 2 * AR_PADDING + LUMA_SUBBLOCK * 2 + BOTTOM_PAD;
const LUMA_BLOCK_W: i32 =
    LEFT_PAD + 2 * AR_PADDING + LUMA_SUBBLOCK * 2 + 2 * AR_PADDING + RIGHT_PAD;
const CHROMA_BLOCK_H: i32 = TOP_PAD + AR_PADDING + CHROMA_SUBBLOCK * 2 + BOTTOM_PAD;
const CHROMA_BLOCK_W: i32 = LEFT_PAD + AR_PADDING + CHROMA_SUBBLOCK * 2 + AR_PADDING + RIGHT_PAD;

thread_local! {
    /// `params->bit_depth` of the picture currently being grained. libaom
    /// keeps `grain_min`/`grain_max` as file statics set once per
    /// `av1_add_film_grain_run` (`grain_synthesis.c:1041-1045`); this mirrors
    /// that rather than threading a parameter through every helper.
    static GRAIN_BIT_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(8) };
}

/// `params->bit_depth` for the [`apply_grain`] call in progress.
fn grain_bit_depth() -> u32 {
    GRAIN_BIT_DEPTH.with(|c| c.get())
}

/// `grain_min`/`grain_max` (`grain_synthesis.c:1043-1045`):
/// `grain_center = 128 << (bit_depth - 8)`.
fn grain_range() -> (i32, i32) {
    let center = 128i32 << (grain_bit_depth() - 8);
    (-center, center - 1)
}

const MIN_LUMA_LEGAL: i32 = 16;
const MAX_LUMA_LEGAL: i32 = 235;
const MIN_CHROMA_LEGAL: i32 = 16;
const MAX_CHROMA_LEGAL: i32 = 240;

/// The spec's 16-bit LFSR (`get_random_number` / `init_random_generator`,
/// libaom `grain_synthesis.c`), threaded as one piece of mutable state
/// across synthesis in the exact call order libaom uses: a raw seed load for
/// the luma template, then a fresh line-mixed reseed for each of the chroma
/// templates and for each row of placement offsets.
struct Rng(u16);

impl Rng {
    fn raw(seed: u16) -> Rng {
        Rng(seed)
    }

    /// `init_random_generator` (spec 7.18.3.5).
    fn line(luma_line: i32, seed: u16) -> Rng {
        let msb = (seed >> 8) & 255;
        let lsb = seed & 255;
        let mut r: u16 = (msb << 8) + lsb;
        let luma_num = luma_line >> 5;
        r ^= (((luma_num.wrapping_mul(37) + 178) & 255) as u16) << 8;
        r ^= ((luma_num.wrapping_mul(173) + 105) & 255) as u16;
        Rng(r)
    }

    /// `get_random_number` (spec 7.18.3.4).
    fn get(&mut self, bits: u32) -> i32 {
        let r = self.0;
        let bit = ((r) ^ (r >> 1) ^ (r >> 3) ^ (r >> 12)) & 1;
        self.0 = (r >> 1) | (bit << 15);
        ((self.0 >> (16 - bits)) & ((1u16 << bits) - 1)) as i32
    }
}

#[inline]
fn at(buf: &[i32], stride: i32, row: i32, col: i32) -> i32 {
    buf[(row * stride + col) as usize]
}

#[inline]
fn set_at(buf: &mut [i32], stride: i32, row: i32, col: i32, v: i32) {
    buf[(row * stride + col) as usize] = v;
}

/// `pred_pos_luma`/`pred_pos_chroma` (libaom `init_arrays`): the AR filter's
/// neighbour taps, in `(delta_row, delta_col, kind)` order, `kind == 1`
/// marking the one extra chroma tap that averages a 2x2 luma quad instead of
/// reading its own plane (spec 7.18.3.3's `avgLuma` case, only present when
/// `num_y_points > 0`).
fn build_pred_pos(lag: i32, has_luma_avg: bool) -> Vec<(i32, i32, i32)> {
    let mut pos = Vec::new();
    for row in -lag..0 {
        for col in -lag..=lag {
            pos.push((row, col, 0));
        }
    }
    for col in -lag..0 {
        pos.push((0, col, 0));
    }
    if has_luma_avg {
        pos.push((0, 0, 1));
    }
    pos
}

/// `generate_luma_grain_block` (spec 7.18.3.3).
fn generate_luma_grain_block(
    fg: &FilmGrainParams,
    pred_pos: &[(i32, i32, i32)],
    rng: &mut Rng,
) -> Vec<i32> {
    let (grain_min, grain_max) = grain_range();
    let mut block = vec![0i32; (LUMA_BLOCK_H * LUMA_BLOCK_W) as usize];
    if fg.num_y_points == 0 {
        return block;
    }
    let gauss_sec_shift = 12 - grain_bit_depth() as i32 + fg.grain_scale_shift as i32;
    let ar_coeff_shift = fg.ar_coeff_shift_minus_6 as i32 + 6;
    let rounding_offset = 1i32 << (ar_coeff_shift - 1);

    for i in 0..LUMA_BLOCK_H {
        for j in 0..LUMA_BLOCK_W {
            let g = GAUSSIAN_SEQUENCE[rng.get(GAUSS_BITS) as usize];
            let v = (g + ((1 << gauss_sec_shift) >> 1)) >> gauss_sec_shift;
            set_at(&mut block, LUMA_BLOCK_W, i, j, v);
        }
    }
    for i in TOP_PAD..LUMA_BLOCK_H - BOTTOM_PAD {
        for j in LEFT_PAD..LUMA_BLOCK_W - RIGHT_PAD {
            let mut wsum = 0i32;
            for (idx, &(dy, dx, _)) in pred_pos.iter().enumerate() {
                wsum += fg.ar_coeffs_y[idx] as i32 * at(&block, LUMA_BLOCK_W, i + dy, j + dx);
            }
            let v = (at(&block, LUMA_BLOCK_W, i, j) + ((wsum + rounding_offset) >> ar_coeff_shift))
                .clamp(grain_min, grain_max);
            set_at(&mut block, LUMA_BLOCK_W, i, j, v);
        }
    }
    block
}

/// `generate_chroma_grain_blocks` (spec 7.18.3.3), returning `(cb, cr)`.
fn generate_chroma_grain_blocks(
    fg: &FilmGrainParams,
    pred_pos: &[(i32, i32, i32)],
    luma_block: &[i32],
    seed: u16,
) -> (Vec<i32>, Vec<i32>) {
    let (grain_min, grain_max) = grain_range();
    let mut cb = vec![0i32; (CHROMA_BLOCK_H * CHROMA_BLOCK_W) as usize];
    let mut cr = vec![0i32; (CHROMA_BLOCK_H * CHROMA_BLOCK_W) as usize];
    let gauss_sec_shift = 12 - grain_bit_depth() as i32 + fg.grain_scale_shift as i32;
    let ar_coeff_shift = fg.ar_coeff_shift_minus_6 as i32 + 6;
    let rounding_offset = 1i32 << (ar_coeff_shift - 1);

    let apply_cb = fg.num_cb_points > 0 || fg.chroma_scaling_from_luma;
    let apply_cr = fg.num_cr_points > 0 || fg.chroma_scaling_from_luma;

    if apply_cb {
        let mut rng = Rng::line(7 << 5, seed);
        for i in 0..CHROMA_BLOCK_H {
            for j in 0..CHROMA_BLOCK_W {
                let g = GAUSSIAN_SEQUENCE[rng.get(GAUSS_BITS) as usize];
                set_at(
                    &mut cb,
                    CHROMA_BLOCK_W,
                    i,
                    j,
                    (g + ((1 << gauss_sec_shift) >> 1)) >> gauss_sec_shift,
                );
            }
        }
    }
    if apply_cr {
        let mut rng = Rng::line(11 << 5, seed);
        for i in 0..CHROMA_BLOCK_H {
            for j in 0..CHROMA_BLOCK_W {
                let g = GAUSSIAN_SEQUENCE[rng.get(GAUSS_BITS) as usize];
                set_at(
                    &mut cr,
                    CHROMA_BLOCK_W,
                    i,
                    j,
                    (g + ((1 << gauss_sec_shift) >> 1)) >> gauss_sec_shift,
                );
            }
        }
    }

    for i in TOP_PAD..CHROMA_BLOCK_H - BOTTOM_PAD {
        for j in LEFT_PAD..CHROMA_BLOCK_W - RIGHT_PAD {
            let mut wsum_cb = 0i32;
            let mut wsum_cr = 0i32;
            for (idx, &(dy, dx, kind)) in pred_pos.iter().enumerate() {
                if kind == 0 {
                    wsum_cb +=
                        fg.ar_coeffs_cb[idx] as i32 * at(&cb, CHROMA_BLOCK_W, i + dy, j + dx);
                    wsum_cr +=
                        fg.ar_coeffs_cr[idx] as i32 * at(&cr, CHROMA_BLOCK_W, i + dy, j + dx);
                } else {
                    // `avgLuma` (spec 7.18.3.3): subsampling_x = subsampling_y = 1
                    // always here, so the 2x2 luma quad is exactly one 4:2:0 cell.
                    let luma_y = ((i - TOP_PAD) << 1) + TOP_PAD;
                    let luma_x = ((j - LEFT_PAD) << 1) + LEFT_PAD;
                    let mut av = 0i32;
                    for k in luma_y..luma_y + 2 {
                        for l in luma_x..luma_x + 2 {
                            av += at(luma_block, LUMA_BLOCK_W, k, l);
                        }
                    }
                    av = (av + 2) >> 2;
                    wsum_cb += fg.ar_coeffs_cb[idx] as i32 * av;
                    wsum_cr += fg.ar_coeffs_cr[idx] as i32 * av;
                }
            }
            if apply_cb {
                let v = (at(&cb, CHROMA_BLOCK_W, i, j)
                    + ((wsum_cb + rounding_offset) >> ar_coeff_shift))
                    .clamp(grain_min, grain_max);
                set_at(&mut cb, CHROMA_BLOCK_W, i, j, v);
            }
            if apply_cr {
                let v = (at(&cr, CHROMA_BLOCK_W, i, j)
                    + ((wsum_cr + rounding_offset) >> ar_coeff_shift))
                    .clamp(grain_min, grain_max);
                set_at(&mut cr, CHROMA_BLOCK_W, i, j, v);
            }
        }
    }
    (cb, cr)
}

/// `init_scaling_function` (spec 7.18.3.2, piecewise-linear scaling LUT).
fn build_scaling_lut(values: &[u8], scaling: &[u8], num_points: usize) -> [i32; 256] {
    let mut lut = [0i32; 256];
    if num_points == 0 {
        return lut;
    }
    for i in 0..values[0] as usize {
        lut[i] = scaling[0] as i32;
    }
    for p in 0..num_points - 1 {
        let delta_y = scaling[p + 1] as i32 - scaling[p] as i32;
        let delta_x = values[p + 1] as i32 - values[p] as i32;
        let inner = (65536 + (delta_x >> 1)) / delta_x;
        let delta = delta_y as i64 * inner as i64;
        for x in 0..delta_x {
            lut[(values[p] as i32 + x) as usize] =
                scaling[p] as i32 + (((x as i64) * delta + 32768) >> 16) as i32;
        }
    }
    for i in values[num_points - 1] as usize..256 {
        lut[i] = scaling[num_points - 1] as i32;
    }
    lut
}

/// `scale_LUT` (`grain_synthesis.c:616`): the scaling LUT has 256 entries
/// regardless of bit depth, so at 10/12 bit the index is shifted down and the
/// two neighbouring entries are interpolated with the dropped low bits.
fn scale_lut(lut: &[i32; 256], index: i32, bit_depth: u32) -> i32 {
    let shift = bit_depth - 8;
    let x = (index >> shift) as usize;
    if shift == 0 || x == 255 {
        lut[x]
    } else {
        lut[x] + (((lut[x + 1] - lut[x]) * (index & ((1 << shift) - 1)) + (1 << (shift - 1)))
            >> shift)
    }
}

/// [`scale_lut`] evaluated over its whole index domain once per frame.
///
/// The scaling LUT is fixed for the frame, so the interpolation the spec
/// writes per sample is a pure function of the sample value with at most
/// `256 << (bit_depth - 8)` distinct answers -- 1024 entries at 10 bit. This
/// is the same function, tabulated: `add_noise_to_block` was 9.0% self time
/// and ran that branchy interpolation once per luma sample and twice per
/// chroma sample.
fn expand_scaling_lut(lut: &[i32; 256], bit_depth: u32) -> Vec<i32> {
    (0..(256i32 << (bit_depth - 8)))
        .map(|i| scale_lut(lut, i, bit_depth))
        .collect()
}

/// `add_noise_to_block` (spec 7.18.3.5), 4:2:0 only. `(py0, px0)` is
/// the luma-plane pixel origin; the matching chroma origin is `(py0/2,
/// px0/2)`. Grain is read from `(grain, stride, row0, col0)` triples so the
/// same function serves both the raw 82x73/44x38 templates and the small
/// blended col/line buffers below.
#[allow(clippy::too_many_arguments)]
fn add_noise_to_block(
    fg: &FilmGrainParams,
    mc_identity: bool,
    scaling_y: &[i32],
    scaling_cb: &[i32],
    scaling_cr: &[i32],
    picture: &mut Picture,
    py0: i32,
    px0: i32,
    luma_grain: &[i32],
    lg_stride: i32,
    lg_row0: i32,
    lg_col0: i32,
    cb_grain: &[i32],
    cr_grain: &[i32],
    cg_stride: i32,
    cg_row0: i32,
    cg_col0: i32,
    half_h: i32,
    half_w: i32,
) {
    let y_stride = picture.width as i32;
    let c_stride = (picture.width / 2) as i32;
    let scaling_shift = fg.grain_scaling_minus_8 as i32 + 8;
    let bit_depth = grain_bit_depth();
    let bd_shift = bit_depth - 8;
    // `(256 << (bit_depth - 8)) - 1`, the scaling-LUT index clamp.
    let index_max = (256i32 << bd_shift) - 1;

    let mut cb_mult = fg.cb_mult as i32 - 128;
    let mut cb_luma_mult = fg.cb_luma_mult as i32 - 128;
    let mut cb_offset = ((fg.cb_offset as i32) << bd_shift) - (1 << bit_depth);
    let mut cr_mult = fg.cr_mult as i32 - 128;
    let mut cr_luma_mult = fg.cr_luma_mult as i32 - 128;
    let mut cr_offset = ((fg.cr_offset as i32) << bd_shift) - (1 << bit_depth);
    if fg.chroma_scaling_from_luma {
        cb_mult = 0;
        cb_luma_mult = 64;
        cb_offset = 0;
        cr_mult = 0;
        cr_luma_mult = 64;
        cr_offset = 0;
    }
    let rounding_offset = 1i32 << (scaling_shift - 1);
    let apply_y = fg.num_y_points > 0;
    let apply_cb = fg.num_cb_points > 0 || fg.chroma_scaling_from_luma;
    let apply_cr = fg.num_cr_points > 0 || fg.chroma_scaling_from_luma;

    let (min_luma, max_luma, min_chroma, max_chroma) = if fg.clip_to_restricted_range {
        if mc_identity {
            (
                MIN_LUMA_LEGAL << bd_shift,
                MAX_LUMA_LEGAL << bd_shift,
                MIN_LUMA_LEGAL << bd_shift,
                MAX_LUMA_LEGAL << bd_shift,
            )
        } else {
            (
                MIN_LUMA_LEGAL << bd_shift,
                MAX_LUMA_LEGAL << bd_shift,
                MIN_CHROMA_LEGAL << bd_shift,
                MAX_CHROMA_LEGAL << bd_shift,
            )
        }
    } else {
        (0, index_max, 0, index_max)
    };

    let cy0 = py0 / 2;
    let cx0 = px0 / 2;
    for i in 0..half_h {
        for j in 0..half_w {
            let ly = py0 + i * 2;
            let lx = px0 + j * 2;
            let l0 = (ly * y_stride + lx) as usize;
            let average_luma = (picture.y[l0] as i32 + picture.y[l0 + 1] as i32 + 1) >> 1;
            let cidx = ((cy0 + i) * c_stride + (cx0 + j)) as usize;
            let gidx = ((cg_row0 + i) * cg_stride + (cg_col0 + j)) as usize;
            if apply_cb {
                let scaled = (((average_luma * cb_luma_mult + cb_mult * picture.u[cidx] as i32)
                    >> 6)
                    + cb_offset)
                    .clamp(0, index_max);
                let delta =
                    (scaling_cb[scaled as usize] * cb_grain[gidx] + rounding_offset)
                        >> scaling_shift;
                picture.u[cidx] =
                    ((picture.u[cidx] as i32 + delta).clamp(min_chroma, max_chroma)) as u16;
            }
            if apply_cr {
                let scaled = (((average_luma * cr_luma_mult + cr_mult * picture.v[cidx] as i32)
                    >> 6)
                    + cr_offset)
                    .clamp(0, index_max);
                let delta =
                    (scaling_cr[scaled as usize] * cr_grain[gidx] + rounding_offset)
                        >> scaling_shift;
                picture.v[cidx] =
                    ((picture.v[cidx] as i32 + delta).clamp(min_chroma, max_chroma)) as u16;
            }
        }
    }
    if apply_y {
        // Both operands are contiguous along a row, so the row bases are
        // computed once instead of a multiply-add per sample.
        let width = (half_w * 2) as usize;
        for i in 0..half_h * 2 {
            let prow = ((py0 + i) * y_stride + px0) as usize;
            let grow = ((lg_row0 + i) * lg_stride + lg_col0) as usize;
            let pixels = &mut picture.y[prow..prow + width];
            let grain = &luma_grain[grow..grow + width];
            for (p, &g) in pixels.iter_mut().zip(grain) {
                let delta =
                    (scaling_y[*p as usize] * g + rounding_offset) >> scaling_shift;
                *p = ((*p as i32 + delta).clamp(min_luma, max_luma)) as u16;
            }
        }
    }
}

/// `ver_boundary_overlap` (spec 7.18.3.5), in place on `dst` (== the C's
/// aliased `left` argument -- every call site overlaps them, and each row is
/// read once then written once, so there is no hazard).
#[allow(clippy::too_many_arguments)]
fn ver_overlap_inplace(
    dst: &mut [i32],
    dst_stride: i32,
    dst_row0: i32,
    dst_col0: i32,
    right: &[i32],
    right_stride: i32,
    right_row0: i32,
    right_col0: i32,
    width: i32,
    height: i32,
) {
    let (grain_min, grain_max) = grain_range();
    for h in 0..height {
        if width == 1 {
            let l = at(dst, dst_stride, dst_row0 + h, dst_col0);
            let r = at(right, right_stride, right_row0 + h, right_col0);
            set_at(
                dst,
                dst_stride,
                dst_row0 + h,
                dst_col0,
                ((l * 23 + r * 22 + 16) >> 5).clamp(grain_min, grain_max),
            );
        } else {
            let l0 = at(dst, dst_stride, dst_row0 + h, dst_col0);
            let l1 = at(dst, dst_stride, dst_row0 + h, dst_col0 + 1);
            let r0 = at(right, right_stride, right_row0 + h, right_col0);
            let r1 = at(right, right_stride, right_row0 + h, right_col0 + 1);
            set_at(
                dst,
                dst_stride,
                dst_row0 + h,
                dst_col0,
                ((27 * l0 + 17 * r0 + 16) >> 5).clamp(grain_min, grain_max),
            );
            set_at(
                dst,
                dst_stride,
                dst_row0 + h,
                dst_col0 + 1,
                ((17 * l1 + 27 * r1 + 16) >> 5).clamp(grain_min, grain_max),
            );
        }
    }
}

/// `hor_boundary_overlap` (spec 7.18.3.5), in place on `dst` (== the C's
/// aliased `top` argument, same no-hazard reasoning as above).
#[allow(clippy::too_many_arguments)]
fn hor_overlap_inplace(
    dst: &mut [i32],
    dst_stride: i32,
    dst_row0: i32,
    dst_col0: i32,
    bottom: &[i32],
    bot_stride: i32,
    bot_row0: i32,
    bot_col0: i32,
    width: i32,
    height: i32,
) {
    let (grain_min, grain_max) = grain_range();
    if height == 1 {
        for w in 0..width {
            let t = at(dst, dst_stride, dst_row0, dst_col0 + w);
            let b = at(bottom, bot_stride, bot_row0, bot_col0 + w);
            set_at(
                dst,
                dst_stride,
                dst_row0,
                dst_col0 + w,
                ((t * 23 + b * 22 + 16) >> 5).clamp(grain_min, grain_max),
            );
        }
    } else {
        for w in 0..width {
            let t0 = at(dst, dst_stride, dst_row0, dst_col0 + w);
            let t1 = at(dst, dst_stride, dst_row0 + 1, dst_col0 + w);
            let b0 = at(bottom, bot_stride, bot_row0, bot_col0 + w);
            let b1 = at(bottom, bot_stride, bot_row0 + 1, bot_col0 + w);
            set_at(
                dst,
                dst_stride,
                dst_row0,
                dst_col0 + w,
                ((27 * t0 + 17 * b0 + 16) >> 5).clamp(grain_min, grain_max),
            );
            set_at(
                dst,
                dst_stride,
                dst_row0 + 1,
                dst_col0 + w,
                ((17 * t1 + 27 * b1 + 16) >> 5).clamp(grain_min, grain_max),
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn copy_area(
    dst: &mut [i32],
    dst_stride: i32,
    dst_row0: i32,
    dst_col0: i32,
    src: &[i32],
    src_stride: i32,
    src_row0: i32,
    src_col0: i32,
    width: i32,
    height: i32,
) {
    let (grain_min, grain_max) = grain_range();
    for h in 0..height {
        for w in 0..width {
            let v = at(src, src_stride, src_row0 + h, src_col0 + w);
            set_at(dst, dst_stride, dst_row0 + h, dst_col0 + w, v);
        }
    }
}

thread_local! {
    /// Firing count for the film-grain gates (class `gate-blind-to-feature`):
    /// how many pictures actually ran through grain synthesis.
    static GRAIN_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`GRAIN_HITS`].
pub(crate) fn grain_hits() -> usize {
    GRAIN_HITS.with(|c| c.get())
}

/// `av1_add_film_grain`/`add_film_grain_run` (spec 7.18.3.5): synthesize and
/// apply film grain to `picture`, returning a new grained picture. `picture`
/// itself is unchanged -- it stays the clean reference-frame-bank copy.
/// Returns `picture.clone()` untouched when `apply_grain` is unset.
pub(crate) fn apply_grain(
    picture: &Picture,
    fg: &FilmGrainParams,
    mc_identity: bool,
    bit_depth: u32,
) -> Picture {
    let mut out = picture.clone();
    if !fg.apply_grain {
        return out;
    }
    GRAIN_BIT_DEPTH.with(|c| c.set(bit_depth));
    GRAIN_HITS.with(|c| c.set(c.get() + 1));
    let width = out.width as i32;
    let height = out.height as i32;
    let y_stride = width;
    let c_stride = width / 2;

    let lag = fg.ar_coeff_lag as i32;
    let pred_luma = build_pred_pos(lag, false);
    let pred_chroma = build_pred_pos(lag, fg.num_y_points > 0);

    let mut rng = Rng::raw(fg.grain_seed);
    let luma_template = generate_luma_grain_block(fg, &pred_luma, &mut rng);
    let (cb_template, cr_template) =
        generate_chroma_grain_blocks(fg, &pred_chroma, &luma_template, fg.grain_seed);

    let scaling_y = build_scaling_lut(
        &fg.point_y_value,
        &fg.point_y_scaling,
        fg.num_y_points as usize,
    );
    let (scaling_cb, scaling_cr) = if fg.chroma_scaling_from_luma {
        (scaling_y, scaling_y)
    } else {
        (
            build_scaling_lut(
                &fg.point_cb_value,
                &fg.point_cb_scaling,
                fg.num_cb_points as usize,
            ),
            build_scaling_lut(
                &fg.point_cr_value,
                &fg.point_cr_scaling,
                fg.num_cr_points as usize,
            ),
        )
    };

    let expand_bd = grain_bit_depth();
    let scaling_y = expand_scaling_lut(&scaling_y, expand_bd);
    let scaling_cb = expand_scaling_lut(&scaling_cb, expand_bd);
    let scaling_cr = expand_scaling_lut(&scaling_cr, expand_bd);

    let overlap = fg.overlap_flag;

    let mut y_line_buf = vec![0i32; (y_stride * 2).max(1) as usize];
    let mut cb_line_buf = vec![0i32; c_stride.max(1) as usize];
    let mut cr_line_buf = vec![0i32; c_stride.max(1) as usize];
    let mut y_col_buf = vec![0i32; ((LUMA_SUBBLOCK + 2) * 2) as usize];
    let mut cb_col_buf = vec![0i32; (CHROMA_SUBBLOCK + 1) as usize];
    let mut cr_col_buf = vec![0i32; (CHROMA_SUBBLOCK + 1) as usize];

    let mut y = 0;
    while y < height / 2 {
        let mut row_rng = Rng::line(y * 2, fg.grain_seed);
        let mut x = 0;
        while x < width / 2 {
            let raw = row_rng.get(8);
            let offset_x = (raw >> 4) & 15;
            let offset_y = raw & 15;

            let luma_off_y = LEFT_PAD + 2 * AR_PADDING + (offset_y << 1);
            let luma_off_x = TOP_PAD + 2 * AR_PADDING + (offset_x << 1);
            let chroma_off_y = TOP_PAD + AR_PADDING + offset_y;
            let chroma_off_x = LEFT_PAD + AR_PADDING + offset_x;

            if overlap && x != 0 {
                let h_count = (LUMA_SUBBLOCK + 2).min(height - (y << 1));
                ver_overlap_inplace(
                    &mut y_col_buf,
                    2,
                    0,
                    0,
                    &luma_template,
                    LUMA_BLOCK_W,
                    luma_off_y,
                    luma_off_x,
                    2,
                    h_count,
                );
                let hc_count = (CHROMA_SUBBLOCK + 1).min((height - (y << 1)) >> 1);
                ver_overlap_inplace(
                    &mut cb_col_buf,
                    1,
                    0,
                    0,
                    &cb_template,
                    CHROMA_BLOCK_W,
                    chroma_off_y,
                    chroma_off_x,
                    1,
                    hc_count,
                );
                ver_overlap_inplace(
                    &mut cr_col_buf,
                    1,
                    0,
                    0,
                    &cr_template,
                    CHROMA_BLOCK_W,
                    chroma_off_y,
                    chroma_off_x,
                    1,
                    hc_count,
                );

                let i = if y != 0 { 1 } else { 0 };
                add_noise_to_block(
                    fg,
                    mc_identity,
                    &scaling_y,
                    &scaling_cb,
                    &scaling_cr,
                    &mut out,
                    (y + i) << 1,
                    x << 1,
                    &y_col_buf,
                    2,
                    i * 2,
                    0,
                    &cb_col_buf,
                    &cr_col_buf,
                    1,
                    i,
                    0,
                    (LUMA_SUBBLOCK >> 1).min(height / 2 - y) - i,
                    1,
                );
            }

            if overlap && y != 0 {
                if x != 0 {
                    hor_overlap_inplace(
                        &mut y_line_buf,
                        y_stride,
                        0,
                        x << 1,
                        &y_col_buf,
                        2,
                        0,
                        0,
                        2,
                        2,
                    );
                    hor_overlap_inplace(
                        &mut cb_line_buf,
                        c_stride,
                        0,
                        x,
                        &cb_col_buf,
                        1,
                        0,
                        0,
                        1,
                        1,
                    );
                    hor_overlap_inplace(
                        &mut cr_line_buf,
                        c_stride,
                        0,
                        x,
                        &cr_col_buf,
                        1,
                        0,
                        0,
                        1,
                        1,
                    );
                }
                let lcol0 = if x != 0 { (x + 1) << 1 } else { 0 };
                let ccol0 = if x != 0 { x + 1 } else { 0 };
                let lw = (LUMA_SUBBLOCK - (if x != 0 { 2 } else { 0 })).min(width - lcol0);
                hor_overlap_inplace(
                    &mut y_line_buf,
                    y_stride,
                    0,
                    lcol0,
                    &luma_template,
                    LUMA_BLOCK_W,
                    luma_off_y,
                    luma_off_x + if x != 0 { 2 } else { 0 },
                    lw,
                    2,
                );
                let cw = (CHROMA_SUBBLOCK - (if x != 0 { 1 } else { 0 })).min((width - lcol0) >> 1);
                hor_overlap_inplace(
                    &mut cb_line_buf,
                    c_stride,
                    0,
                    ccol0,
                    &cb_template,
                    CHROMA_BLOCK_W,
                    chroma_off_y,
                    chroma_off_x + if x != 0 { 1 } else { 0 },
                    cw,
                    1,
                );
                hor_overlap_inplace(
                    &mut cr_line_buf,
                    c_stride,
                    0,
                    ccol0,
                    &cr_template,
                    CHROMA_BLOCK_W,
                    chroma_off_y,
                    chroma_off_x + if x != 0 { 1 } else { 0 },
                    cw,
                    1,
                );

                add_noise_to_block(
                    fg,
                    mc_identity,
                    &scaling_y,
                    &scaling_cb,
                    &scaling_cr,
                    &mut out,
                    y << 1,
                    x << 1,
                    &y_line_buf,
                    y_stride,
                    0,
                    x << 1,
                    &cb_line_buf,
                    &cr_line_buf,
                    c_stride,
                    0,
                    x,
                    1,
                    (LUMA_SUBBLOCK >> 1).min(width / 2 - x),
                );
            }

            let i = if overlap && y != 0 { 1 } else { 0 };
            let j = if overlap && x != 0 { 1 } else { 0 };
            add_noise_to_block(
                fg,
                mc_identity,
                &scaling_y,
                &scaling_cb,
                &scaling_cr,
                &mut out,
                (y + i) << 1,
                (x + j) << 1,
                &luma_template,
                LUMA_BLOCK_W,
                luma_off_y + (i << 1),
                luma_off_x + (j << 1),
                &cb_template,
                &cr_template,
                CHROMA_BLOCK_W,
                chroma_off_y + i,
                chroma_off_x + j,
                (LUMA_SUBBLOCK >> 1).min(height / 2 - y) - i,
                (LUMA_SUBBLOCK >> 1).min(width / 2 - x) - j,
            );

            if overlap {
                if x != 0 {
                    copy_area(
                        &mut y_line_buf,
                        y_stride,
                        0,
                        x << 1,
                        &y_col_buf,
                        2,
                        LUMA_SUBBLOCK,
                        0,
                        2,
                        2,
                    );
                    copy_area(
                        &mut cb_line_buf,
                        c_stride,
                        0,
                        x,
                        &cb_col_buf,
                        1,
                        CHROMA_SUBBLOCK,
                        0,
                        1,
                        1,
                    );
                    copy_area(
                        &mut cr_line_buf,
                        c_stride,
                        0,
                        x,
                        &cr_col_buf,
                        1,
                        CHROMA_SUBBLOCK,
                        0,
                        1,
                        1,
                    );
                }
                let lcol0 = if x != 0 { (x + 1) << 1 } else { 0 };
                let ccol0 = if x != 0 { x + 1 } else { 0 };
                let lcut = if x != 0 { 2 } else { 0 };
                let ccut = if x != 0 { 1 } else { 0 };
                copy_area(
                    &mut y_line_buf,
                    y_stride,
                    0,
                    lcol0,
                    &luma_template,
                    LUMA_BLOCK_W,
                    luma_off_y + LUMA_SUBBLOCK,
                    luma_off_x + lcut,
                    LUMA_SUBBLOCK.min(width - (x << 1)) - lcut,
                    2,
                );
                copy_area(
                    &mut cb_line_buf,
                    c_stride,
                    0,
                    ccol0,
                    &cb_template,
                    CHROMA_BLOCK_W,
                    chroma_off_y + CHROMA_SUBBLOCK,
                    chroma_off_x + ccut,
                    CHROMA_SUBBLOCK.min((width - (x << 1)) >> 1) - ccut,
                    1,
                );
                copy_area(
                    &mut cr_line_buf,
                    c_stride,
                    0,
                    ccol0,
                    &cr_template,
                    CHROMA_BLOCK_W,
                    chroma_off_y + CHROMA_SUBBLOCK,
                    chroma_off_x + ccut,
                    CHROMA_SUBBLOCK.min((width - (x << 1)) >> 1) - ccut,
                    1,
                );

                let h_count = (LUMA_SUBBLOCK + 2).min(height - (y << 1));
                copy_area(
                    &mut y_col_buf,
                    2,
                    0,
                    0,
                    &luma_template,
                    LUMA_BLOCK_W,
                    luma_off_y,
                    luma_off_x + LUMA_SUBBLOCK,
                    2,
                    h_count,
                );
                let hc_count = (CHROMA_SUBBLOCK + 1).min((height - (y << 1)) >> 1);
                copy_area(
                    &mut cb_col_buf,
                    1,
                    0,
                    0,
                    &cb_template,
                    CHROMA_BLOCK_W,
                    chroma_off_y,
                    chroma_off_x + CHROMA_SUBBLOCK,
                    1,
                    hc_count,
                );
                copy_area(
                    &mut cr_col_buf,
                    1,
                    0,
                    0,
                    &cr_template,
                    CHROMA_BLOCK_W,
                    chroma_off_y,
                    chroma_off_x + CHROMA_SUBBLOCK,
                    1,
                    hc_count,
                );
            }

            x += LUMA_SUBBLOCK >> 1;
        }
        y += LUMA_SUBBLOCK >> 1;
    }

    out
}
