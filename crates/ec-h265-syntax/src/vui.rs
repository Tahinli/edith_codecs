//! VUI parameters — the part of an SPS that says what the samples *mean*.

use ec_core::bitio::{BitReader, BitWriter};
use ec_core::color::ColorDescription;
use ec_core::error::Result;

/// `colour_primaries`, `transfer_characteristics`, `matrix_coeffs` (H.273 code
/// points), carried verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ColourDescription {
    /// `colour_primaries`: 1 = BT.709, 9 = BT.2020.
    pub colour_primaries: u8,
    /// `transfer_characteristics`: 1 = BT.709, 16 = PQ, 18 = HLG.
    pub transfer_characteristics: u8,
    /// `matrix_coeffs`: 1 = BT.709, 9 = BT.2020 non-constant luminance.
    pub matrix_coeffs: u8,
}

impl ColourDescription {
    /// The three code points a [`ColorDescription`] resolves to.
    ///
    /// This is the bridge that keeps the bitstream and the container from
    /// drifting apart: both sides come from the same H.273 resolution in
    /// [`ec_core::color`].
    pub fn from_color(color: ColorDescription) -> ColourDescription {
        let (primaries, transfer, matrix) = color.codes();
        ColourDescription {
            colour_primaries: primaries as u8,
            transfer_characteristics: transfer as u8,
            matrix_coeffs: matrix as u8,
        }
    }
}

/// `video_signal_type` and the colour description under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoSignalType {
    /// The analogue system a picture came off; 5 ("unspecified") for anything
    /// this family encodes, because none of it did.
    pub video_format: u8,
    /// False = limited range (16..235), true = full range.
    pub video_full_range_flag: bool,
    /// The H.273 code points, when stated.
    pub colour_description: Option<ColourDescription>,
}

impl Default for VideoSignalType {
    fn default() -> Self {
        VideoSignalType {
            video_format: 5,
            video_full_range_flag: false,
            colour_description: None,
        }
    }
}

/// The VUI fields this family writes and reads (E.2.1).
///
/// Everything absent here is written as "not present"; an HRD block in
/// particular is deliberately never written, since nothing downstream reads one
/// and a wrong one is worse than none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VuiParameters {
    /// Sample aspect ratio as `aspect_ratio_idc`, or `Some((w, h))` for the
    /// extended form.
    pub sample_aspect_ratio: Option<(u16, u16)>,
    /// What the samples mean.
    pub video_signal_type: Option<VideoSignalType>,
    /// `(num_units_in_tick, time_scale)`: a rational frame rate, exact.
    pub timing: Option<(u32, u32)>,
}

impl VuiParameters {
    /// Write `vui_parameters()`.
    pub fn write(&self, w: &mut BitWriter) {
        match self.sample_aspect_ratio {
            Some((sw, sh)) => {
                w.write_bit(true);
                w.write_bits(255, 8); // EXTENDED_SAR
                w.write_bits(u32::from(sw), 16);
                w.write_bits(u32::from(sh), 16);
            }
            None => w.write_bit(false),
        }
        w.write_bit(false); // overscan_info_present_flag
        match self.video_signal_type {
            Some(vst) => {
                w.write_bit(true);
                w.write_bits(u32::from(vst.video_format), 3);
                w.write_bit(vst.video_full_range_flag);
                match vst.colour_description {
                    Some(cd) => {
                        w.write_bit(true);
                        w.write_bits(u32::from(cd.colour_primaries), 8);
                        w.write_bits(u32::from(cd.transfer_characteristics), 8);
                        w.write_bits(u32::from(cd.matrix_coeffs), 8);
                    }
                    None => w.write_bit(false),
                }
            }
            None => w.write_bit(false),
        }
        w.write_bit(false); // chroma_loc_info_present_flag
        w.write_bit(false); // neutral_chroma_indication_flag
        w.write_bit(false); // field_seq_flag
        w.write_bit(false); // frame_field_info_present_flag
        w.write_bit(false); // default_display_window_flag
        match self.timing {
            Some((units, scale)) => {
                w.write_bit(true);
                w.write_bits(units, 32);
                w.write_bits(scale, 32);
                w.write_bit(false); // vui_poc_proportional_to_timing_flag
                w.write_bit(false); // vui_hrd_parameters_present_flag
            }
            None => w.write_bit(false),
        }
        w.write_bit(false); // bitstream_restriction_flag
    }

    /// Parse `vui_parameters()`, skipping past the parts not modelled.
    pub fn parse(r: &mut BitReader, max_sub_layers_minus1: u32) -> Result<VuiParameters> {
        let mut vui = VuiParameters::default();
        if r.read_bit()? {
            let idc = r.read_bits(8)?;
            if idc == 255 {
                let sw = r.read_bits(16)? as u16;
                let sh = r.read_bits(16)? as u16;
                vui.sample_aspect_ratio = Some((sw, sh));
            }
        }
        if r.read_bit()? {
            r.read_bit()?; // overscan_appropriate_flag
        }
        if r.read_bit()? {
            let video_format = r.read_bits(3)? as u8;
            let video_full_range_flag = r.read_bit()?;
            let colour_description = if r.read_bit()? {
                Some(ColourDescription {
                    colour_primaries: r.read_bits(8)? as u8,
                    transfer_characteristics: r.read_bits(8)? as u8,
                    matrix_coeffs: r.read_bits(8)? as u8,
                })
            } else {
                None
            };
            vui.video_signal_type = Some(VideoSignalType {
                video_format,
                video_full_range_flag,
                colour_description,
            });
        }
        if r.read_bit()? {
            r.read_ue()?;
            r.read_ue()?;
        }
        r.read_bit()?; // neutral_chroma_indication_flag
        r.read_bit()?; // field_seq_flag
        r.read_bit()?; // frame_field_info_present_flag
        if r.read_bit()? {
            for _ in 0..4 {
                r.read_ue()?;
            }
        }
        if r.read_bit()? {
            let units = r.read_bits(32)?;
            let scale = r.read_bits(32)?;
            vui.timing = Some((units, scale));
            if r.read_bit()? {
                r.read_ue()?;
            }
            if r.read_bit()? {
                skip_hrd_parameters(r, true, max_sub_layers_minus1)?;
            }
        }
        if r.read_bit()? {
            r.read_bit()?;
            r.read_bit()?;
            r.read_bit()?;
            for _ in 0..5 {
                r.read_ue()?;
            }
        }
        Ok(vui)
    }
}

/// Walk past an `hrd_parameters()` block (E.2.2) without modelling it.
fn skip_hrd_parameters(
    r: &mut BitReader,
    common_inf_present: bool,
    max_sub_layers_minus1: u32,
) -> Result<()> {
    let mut nal_hrd = false;
    let mut vcl_hrd = false;
    let mut sub_pic_hrd = false;
    if common_inf_present {
        nal_hrd = r.read_bit()?;
        vcl_hrd = r.read_bit()?;
        if nal_hrd || vcl_hrd {
            sub_pic_hrd = r.read_bit()?;
            if sub_pic_hrd {
                r.read_bits(8)?; // tick_divisor_minus2
                r.read_bits(5)?; // du_cpb_removal_delay_increment_length_minus1
                r.read_bit()?; // sub_pic_cpb_params_in_pic_timing_sei_flag
                r.read_bits(5)?; // dpb_output_delay_du_length_minus1
            }
            r.read_bits(4)?; // bit_rate_scale
            r.read_bits(4)?; // cpb_size_scale
            if sub_pic_hrd {
                r.read_bits(4)?; // cpb_size_du_scale
            }
            r.read_bits(5)?; // initial_cpb_removal_delay_length_minus1
            r.read_bits(5)?; // au_cpb_removal_delay_length_minus1
            r.read_bits(5)?; // dpb_output_delay_length_minus1
        }
    }
    for _ in 0..=max_sub_layers_minus1 {
        let fixed_pic_rate_general = r.read_bit()?;
        let mut fixed_pic_rate_within_cvs = fixed_pic_rate_general;
        if !fixed_pic_rate_general {
            fixed_pic_rate_within_cvs = r.read_bit()?;
        }
        let mut low_delay = false;
        if fixed_pic_rate_within_cvs {
            r.read_ue()?; // elemental_duration_in_tc_minus1
        } else {
            low_delay = r.read_bit()?;
        }
        let mut cpb_cnt = 1u32;
        if !low_delay {
            cpb_cnt = r.read_ue()? + 1;
        }
        for present in [nal_hrd, vcl_hrd] {
            if !present {
                continue;
            }
            for _ in 0..cpb_cnt {
                r.read_ue()?; // bit_rate_value_minus1
                r.read_ue()?; // cpb_size_value_minus1
                if sub_pic_hrd {
                    r.read_ue()?; // cpb_size_du_value_minus1
                    r.read_ue()?; // bit_rate_du_value_minus1
                }
                r.read_bit()?; // cbr_flag
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vui_round_trips() {
        let vui = VuiParameters {
            sample_aspect_ratio: Some((1, 1)),
            video_signal_type: Some(VideoSignalType {
                video_format: 5,
                video_full_range_flag: false,
                colour_description: Some(ColourDescription {
                    colour_primaries: 1,
                    transfer_characteristics: 1,
                    matrix_coeffs: 1,
                }),
            }),
            timing: Some((1001, 60_000)),
        };
        let mut w = BitWriter::new();
        vui.write(&mut w);
        w.align_to_byte();
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        assert_eq!(VuiParameters::parse(&mut r, 0).unwrap(), vui);

        // Every export this family writes declares itself SDR: 709 above 720
        // lines and 601 below, which is the rule ec-core states, not a guess
        // made here.
        for height in [1080, 2160] {
            let cd = ColourDescription::from_color(ColorDescription::output(height));
            assert_eq!((cd.colour_primaries, cd.transfer_characteristics, cd.matrix_coeffs), (1, 1, 1));
        }
        let cd = ColourDescription::from_color(ColorDescription::output(480));
        assert_eq!((cd.colour_primaries, cd.matrix_coeffs), (6, 6));
    }
}
