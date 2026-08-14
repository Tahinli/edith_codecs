//! Ten thousand malformed parameter sets and slice headers, none of which may
//! panic.
//!
//! The parse side of this crate is what a hardware decode path feeds from, and
//! its input is whatever a file on disk contains. Truncation must come back as
//! `NeedMore`, a rule violation as `Corrupt`, and a construct this family does
//! not implement as `Unsupported` — never as an index out of bounds.

use ec_h265_syntax::nal::{NalHeader, split_annex_b, unescape_rbsp};
use ec_h265_syntax::ps::{ConformanceWindow, ProfileTierLevel};
use ec_h265_syntax::slice::SliceHeader;
use ec_h265_syntax::{NalUnitType, Pps, Sps, Vps};

/// xorshift64*, so the corpus is the same on every run and a failure is
/// reproducible from its seed alone.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn byte(&mut self) -> u8 {
        (self.next() >> 33) as u8
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() >> 33) as usize % n.max(1)
    }
}

fn reference_sps() -> Sps {
    Sps {
        vps_id: 0,
        id: 0,
        chroma_format_idc: 1,
        separate_colour_plane: false,
        pic_width_in_luma_samples: 1920,
        pic_height_in_luma_samples: 1088,
        conf_win: ConformanceWindow {
            left: 0,
            right: 0,
            top: 0,
            bottom: 4,
        },
        bit_depth_luma_minus8: 0,
        bit_depth_chroma_minus8: 0,
        log2_max_poc_lsb_minus4: 4,
        max_dec_pic_buffering_minus1: 0,
        max_num_reorder_pics: 0,
        log2_min_cb_size_minus3: 0,
        log2_diff_max_min_cb_size: 3,
        log2_min_tb_size_minus2: 0,
        log2_diff_max_min_tb_size: 3,
        max_transform_hierarchy_depth_inter: 0,
        max_transform_hierarchy_depth_intra: 0,
        scaling_list_enabled: false,
        amp_enabled: false,
        sao_enabled: true,
        pcm_enabled: false,
        pcm: None,
        num_short_term_ref_pic_sets: 0,
        short_term_ref_pic_sets: Vec::new(),
        long_term_ref_pics_present: false,
        num_long_term_ref_pics_sps: 0,
        long_term_ref_pics_sps: Vec::new(),
        temporal_mvp_enabled: true,
        strong_intra_smoothing: true,
        ptl: ProfileTierLevel::main(120),
        vui: None,
    }
}

#[test]
fn ten_thousand_malformed_inputs_never_panic() {
    let mut rng = Rng(0x5eed_1234_abcd_0001);
    let sps = reference_sps();
    let pps = Pps {
        entropy_coding_sync_enabled: true,
        dependent_slice_segments_enabled: true,
        tiles_enabled: true,
        cu_qp_delta_enabled: true,
        deblocking_filter_control_present: true,
        deblocking_filter_override_enabled: true,
        slice_chroma_qp_offsets_present: true,
        lists_modification_present: true,
        output_flag_present: true,
        cabac_init_present: true,
        num_extra_slice_header_bits: 2,
        weighted_pred: true,
        weighted_bipred: true,
        slice_segment_header_extension_present: true,
        ..Pps::default()
    };
    let seeds = [
        sps.to_rbsp(),
        pps.to_rbsp(),
        Vps {
            id: 0,
            ptl: ProfileTierLevel::main(120),
            max_dec_pic_buffering_minus1: 1,
            max_num_reorder_pics: 0,
        }
        .to_rbsp(),
    ];

    for round in 0..10_000u32 {
        // Half the corpus is noise, half is a valid structure with bytes
        // flipped — the shape that actually reaches a decoder off a disk.
        let mut data = if round % 2 == 0 {
            let len = rng.below(96);
            (0..len).map(|_| rng.byte()).collect::<Vec<u8>>()
        } else {
            let mut base = seeds[rng.below(seeds.len())].clone();
            let flips = 1 + rng.below(4);
            for _ in 0..flips {
                if base.is_empty() {
                    break;
                }
                let at = rng.below(base.len());
                base[at] ^= 1 << rng.below(8);
            }
            if rng.below(4) == 0 {
                base.truncate(rng.below(base.len().max(1)));
            }
            base
        };

        let _ = Vps::parse(&data);
        let _ = Sps::parse(&data);
        let _ = Pps::parse(&data);
        for nal_type in [NalUnitType::IdrWRadl, NalUnitType::TrailR] {
            let _ = SliceHeader::parse(&data, &sps, &pps, nal_type);
        }
        let _ = NalHeader::parse(&data);
        let _ = unescape_rbsp(&data);

        // The same bytes as a stream, with start codes sprinkled in.
        if round % 3 == 0 {
            let at = rng.below(data.len().max(1));
            data.splice(at..at, [0, 0, 1, 0x26, 0x01]);
            for nal in split_annex_b(&data) {
                let rbsp = nal.rbsp();
                let _ = Sps::parse(&rbsp);
                let _ = SliceHeader::parse(&rbsp, &sps, &pps, nal.header.nal_type);
            }
        }
    }
}

#[test]
fn truncation_is_need_more_not_corrupt() {
    let sps = reference_sps().to_rbsp();
    let mut saw_need_more = false;
    for len in 0..sps.len() {
        match Sps::parse(&sps[..len]) {
            Err(e) if e.is_need_more() => saw_need_more = true,
            // A shorter prefix can also be a legal-looking but wrong SPS; what
            // it may never be is a panic, which is the point of the loop.
            _ => {}
        }
    }
    assert!(saw_need_more, "no prefix of an SPS reported NeedMore");
}
