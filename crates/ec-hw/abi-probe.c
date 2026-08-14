/* abi-probe.c — print the layout of every libva codec parameter struct this
 * crate transcribes, compiled against the *system* headers.
 *
 * The numbers it prints are pasted into the `const _: () = assert!(...)` block
 * at the end of each `src/params/*.rs`, so a transcription slip is a compile
 * error rather than a driver reading garbage. Rerun after a libva upgrade:
 *
 *   cc -o /tmp/abi-probe crates/ec-hw/abi-probe.c $(pkg-config --cflags libva) && /tmp/abi-probe
 *
 * Same contract as crates/ec-va/abi-probe.c, one level up the stack.
 */
#include <stdio.h>
#include <stddef.h>
#include <va/va.h>
#include <va/va_dec_hevc.h>
#include <va/va_dec_vp9.h>
#include <va/va_dec_av1.h>
#include <va/va_enc_h264.h>
#include <va/va_enc_hevc.h>
#include <va/va_enc_av1.h>

#define S(t) printf("%-46s size=%-6zu align=%zu\n", #t, sizeof(t), _Alignof(t))
#define O(t, f) printf("  %-44s %zu\n", #t "." #f, offsetof(t, f))

int main(void)
{
    printf("libva %d.%d.%d\n", VA_MAJOR_VERSION, VA_MINOR_VERSION, VA_MICRO_VERSION);

    S(VAPictureH264);
    O(VAPictureH264, frame_idx);
    O(VAPictureH264, flags);
    O(VAPictureH264, TopFieldOrderCnt);
    O(VAPictureH264, BottomFieldOrderCnt);

    S(VAPictureParameterBufferH264);
    O(VAPictureParameterBufferH264, ReferenceFrames);
    O(VAPictureParameterBufferH264, picture_width_in_mbs_minus1);
    O(VAPictureParameterBufferH264, bit_depth_luma_minus8);
    O(VAPictureParameterBufferH264, num_ref_frames);
    O(VAPictureParameterBufferH264, seq_fields);
    O(VAPictureParameterBufferH264, pic_init_qp_minus26);
    O(VAPictureParameterBufferH264, pic_fields);
    O(VAPictureParameterBufferH264, frame_num);

    S(VAIQMatrixBufferH264);
    O(VAIQMatrixBufferH264, ScalingList8x8);

    S(VASliceParameterBufferH264);
    O(VASliceParameterBufferH264, slice_data_bit_offset);
    O(VASliceParameterBufferH264, slice_type);
    O(VASliceParameterBufferH264, cabac_init_idc);
    O(VASliceParameterBufferH264, RefPicList0);
    O(VASliceParameterBufferH264, RefPicList1);
    O(VASliceParameterBufferH264, luma_log2_weight_denom);
    O(VASliceParameterBufferH264, luma_weight_l0);
    O(VASliceParameterBufferH264, chroma_weight_l0);
    O(VASliceParameterBufferH264, luma_weight_l1_flag);
    O(VASliceParameterBufferH264, chroma_offset_l1);

    S(VAPictureHEVC);
    O(VAPictureHEVC, pic_order_cnt);
    O(VAPictureHEVC, flags);

    S(VAPictureParameterBufferHEVC);
    O(VAPictureParameterBufferHEVC, ReferenceFrames);
    O(VAPictureParameterBufferHEVC, pic_width_in_luma_samples);
    O(VAPictureParameterBufferHEVC, pic_fields);
    O(VAPictureParameterBufferHEVC, sps_max_dec_pic_buffering_minus1);
    O(VAPictureParameterBufferHEVC, column_width_minus1);
    O(VAPictureParameterBufferHEVC, row_height_minus1);
    O(VAPictureParameterBufferHEVC, slice_parsing_fields);
    O(VAPictureParameterBufferHEVC, log2_max_pic_order_cnt_lsb_minus4);
    O(VAPictureParameterBufferHEVC, st_rps_bits);

    S(VASliceParameterBufferHEVC);
    O(VASliceParameterBufferHEVC, slice_segment_address);
    O(VASliceParameterBufferHEVC, RefPicList);
    O(VASliceParameterBufferHEVC, LongSliceFlags);
    O(VASliceParameterBufferHEVC, collocated_ref_idx);
    O(VASliceParameterBufferHEVC, luma_log2_weight_denom);
    O(VASliceParameterBufferHEVC, delta_luma_weight_l0);
    O(VASliceParameterBufferHEVC, delta_luma_weight_l1);
    O(VASliceParameterBufferHEVC, five_minus_max_num_merge_cand);
    O(VASliceParameterBufferHEVC, num_entry_point_offsets);
    O(VASliceParameterBufferHEVC, slice_data_num_emu_prevn_bytes);

    S(VAIQMatrixBufferHEVC);
    O(VAIQMatrixBufferHEVC, ScalingList32x32);
    O(VAIQMatrixBufferHEVC, ScalingListDC16x16);

    S(VADecPictureParameterBufferVP9);
    O(VADecPictureParameterBufferVP9, reference_frames);
    O(VADecPictureParameterBufferVP9, pic_fields);
    O(VADecPictureParameterBufferVP9, filter_level);
    O(VADecPictureParameterBufferVP9, first_partition_size);
    O(VADecPictureParameterBufferVP9, mb_segment_tree_probs);
    O(VADecPictureParameterBufferVP9, profile);

    S(VASegmentParameterVP9);
    O(VASegmentParameterVP9, filter_level);
    O(VASegmentParameterVP9, luma_ac_quant_scale);

    S(VASliceParameterBufferVP9);
    O(VASliceParameterBufferVP9, seg_param);

    S(VASegmentationStructAV1);
    O(VASegmentationStructAV1, feature_data);
    O(VASegmentationStructAV1, feature_mask);
    S(VAFilmGrainStructAV1);
    O(VAFilmGrainStructAV1, grain_seed);
    O(VAFilmGrainStructAV1, ar_coeffs_y);
    O(VAFilmGrainStructAV1, cr_offset);
    S(VAWarpedMotionParamsAV1);
    O(VAWarpedMotionParamsAV1, wmmat);
    O(VAWarpedMotionParamsAV1, invalid);

    S(VADecPictureParameterBufferAV1);
    O(VADecPictureParameterBufferAV1, seq_info_fields);
    O(VADecPictureParameterBufferAV1, current_frame);
    O(VADecPictureParameterBufferAV1, anchor_frames_num);
    O(VADecPictureParameterBufferAV1, anchor_frames_list);
    O(VADecPictureParameterBufferAV1, frame_width_minus1);
    O(VADecPictureParameterBufferAV1, ref_frame_map);
    O(VADecPictureParameterBufferAV1, ref_frame_idx);
    O(VADecPictureParameterBufferAV1, seg_info);
    O(VADecPictureParameterBufferAV1, film_grain_info);
    O(VADecPictureParameterBufferAV1, tile_cols);
    O(VADecPictureParameterBufferAV1, width_in_sbs_minus_1);
    O(VADecPictureParameterBufferAV1, height_in_sbs_minus_1);
    O(VADecPictureParameterBufferAV1, tile_count_minus_1);
    O(VADecPictureParameterBufferAV1, pic_info_fields);
    O(VADecPictureParameterBufferAV1, superres_scale_denominator);
    O(VADecPictureParameterBufferAV1, loop_filter_info_fields);
    O(VADecPictureParameterBufferAV1, ref_deltas);
    O(VADecPictureParameterBufferAV1, base_qindex);
    O(VADecPictureParameterBufferAV1, qmatrix_fields);
    O(VADecPictureParameterBufferAV1, mode_control_fields);
    O(VADecPictureParameterBufferAV1, cdef_damping_minus_3);
    O(VADecPictureParameterBufferAV1, cdef_y_strengths);
    O(VADecPictureParameterBufferAV1, loop_restoration_fields);
    O(VADecPictureParameterBufferAV1, wm);

    S(VASliceParameterBufferAV1);
    O(VASliceParameterBufferAV1, tile_row);
    O(VASliceParameterBufferAV1, anchor_frame_idx);
    O(VASliceParameterBufferAV1, tile_idx_in_tile_list);

    S(VAEncSequenceParameterBufferH264);
    O(VAEncSequenceParameterBufferH264, intra_period);
    O(VAEncSequenceParameterBufferH264, picture_width_in_mbs);
    O(VAEncSequenceParameterBufferH264, seq_fields);
    O(VAEncSequenceParameterBufferH264, bit_depth_luma_minus8);
    O(VAEncSequenceParameterBufferH264, offset_for_non_ref_pic);
    O(VAEncSequenceParameterBufferH264, offset_for_ref_frame);
    O(VAEncSequenceParameterBufferH264, frame_cropping_flag);
    O(VAEncSequenceParameterBufferH264, frame_crop_left_offset);
    O(VAEncSequenceParameterBufferH264, vui_parameters_present_flag);
    O(VAEncSequenceParameterBufferH264, vui_fields);
    O(VAEncSequenceParameterBufferH264, aspect_ratio_idc);
    O(VAEncSequenceParameterBufferH264, num_units_in_tick);
    O(VAEncSequenceParameterBufferH264, time_scale);

    S(VAEncPictureParameterBufferH264);
    O(VAEncPictureParameterBufferH264, ReferenceFrames);
    O(VAEncPictureParameterBufferH264, coded_buf);
    O(VAEncPictureParameterBufferH264, pic_parameter_set_id);
    O(VAEncPictureParameterBufferH264, frame_num);
    O(VAEncPictureParameterBufferH264, pic_init_qp);
    O(VAEncPictureParameterBufferH264, pic_fields);

    S(VAEncSliceParameterBufferH264);
    O(VAEncSliceParameterBufferH264, macroblock_info);
    O(VAEncSliceParameterBufferH264, slice_type);
    O(VAEncSliceParameterBufferH264, idr_pic_id);
    O(VAEncSliceParameterBufferH264, delta_pic_order_cnt_bottom);
    O(VAEncSliceParameterBufferH264, direct_spatial_mv_pred_flag);
    O(VAEncSliceParameterBufferH264, RefPicList0);
    O(VAEncSliceParameterBufferH264, RefPicList1);
    O(VAEncSliceParameterBufferH264, luma_log2_weight_denom);
    O(VAEncSliceParameterBufferH264, cabac_init_idc);

    S(VAEncSequenceParameterBufferHEVC);
    O(VAEncSequenceParameterBufferHEVC, intra_period);
    O(VAEncSequenceParameterBufferHEVC, pic_width_in_luma_samples);
    O(VAEncSequenceParameterBufferHEVC, seq_fields);
    O(VAEncSequenceParameterBufferHEVC, log2_min_luma_coding_block_size_minus3);
    O(VAEncSequenceParameterBufferHEVC, pcm_sample_bit_depth_luma_minus1);
    O(VAEncSequenceParameterBufferHEVC, vui_parameters_present_flag);
    O(VAEncSequenceParameterBufferHEVC, vui_fields);
    O(VAEncSequenceParameterBufferHEVC, aspect_ratio_idc);
    O(VAEncSequenceParameterBufferHEVC, vui_num_units_in_tick);
    O(VAEncSequenceParameterBufferHEVC, min_spatial_segmentation_idc);
    O(VAEncSequenceParameterBufferHEVC, scc_fields);

    S(VAEncPictureParameterBufferHEVC);
    O(VAEncPictureParameterBufferHEVC, reference_frames);
    O(VAEncPictureParameterBufferHEVC, coded_buf);
    O(VAEncPictureParameterBufferHEVC, collocated_ref_pic_index);
    O(VAEncPictureParameterBufferHEVC, column_width_minus1);
    O(VAEncPictureParameterBufferHEVC, row_height_minus1);
    O(VAEncPictureParameterBufferHEVC, log2_parallel_merge_level_minus2);
    O(VAEncPictureParameterBufferHEVC, nal_unit_type);
    O(VAEncPictureParameterBufferHEVC, pic_fields);
    O(VAEncPictureParameterBufferHEVC, hierarchical_level_plus1);
    O(VAEncPictureParameterBufferHEVC, scc_fields);

    S(VAEncSliceParameterBufferHEVC);
    O(VAEncSliceParameterBufferHEVC, num_ctu_in_slice);
    O(VAEncSliceParameterBufferHEVC, slice_type);
    O(VAEncSliceParameterBufferHEVC, ref_pic_list0);
    O(VAEncSliceParameterBufferHEVC, ref_pic_list1);
    O(VAEncSliceParameterBufferHEVC, luma_log2_weight_denom);
    O(VAEncSliceParameterBufferHEVC, max_num_merge_cand);
    O(VAEncSliceParameterBufferHEVC, slice_fields);
    O(VAEncSliceParameterBufferHEVC, pred_weight_table_bit_offset);

    S(VAEncSequenceParameterBufferAV1);
    O(VAEncSequenceParameterBufferAV1, intra_period);
    O(VAEncSequenceParameterBufferAV1, seq_fields);
    O(VAEncSequenceParameterBufferAV1, order_hint_bits_minus_1);
    S(VAEncSegParamAV1);
    O(VAEncSegParamAV1, feature_data);
    O(VAEncSegParamAV1, feature_mask);
    S(VAEncWarpedMotionParamsAV1);
    S(VAEncPictureParameterBufferAV1);
    O(VAEncPictureParameterBufferAV1, reconstructed_frame);
    O(VAEncPictureParameterBufferAV1, coded_buf);
    O(VAEncPictureParameterBufferAV1, reference_frames);
    O(VAEncPictureParameterBufferAV1, ref_frame_idx);
    O(VAEncPictureParameterBufferAV1, ref_frame_ctrl_l0);
    O(VAEncPictureParameterBufferAV1, picture_flags);
    O(VAEncPictureParameterBufferAV1, seg_id_block_size);
    O(VAEncPictureParameterBufferAV1, filter_level);
    O(VAEncPictureParameterBufferAV1, loop_filter_flags);
    O(VAEncPictureParameterBufferAV1, ref_deltas);
    O(VAEncPictureParameterBufferAV1, base_qindex);
    O(VAEncPictureParameterBufferAV1, min_base_qindex);
    O(VAEncPictureParameterBufferAV1, qmatrix_flags);
    O(VAEncPictureParameterBufferAV1, mode_control_flags);
    O(VAEncPictureParameterBufferAV1, segments);
    O(VAEncPictureParameterBufferAV1, tile_cols);
    O(VAEncPictureParameterBufferAV1, width_in_sbs_minus_1);
    O(VAEncPictureParameterBufferAV1, context_update_tile_id);
    O(VAEncPictureParameterBufferAV1, cdef_damping_minus_3);
    O(VAEncPictureParameterBufferAV1, loop_restoration_flags);
    O(VAEncPictureParameterBufferAV1, wm);
    O(VAEncPictureParameterBufferAV1, bit_offset_qindex);
    O(VAEncPictureParameterBufferAV1, tile_group_obu_hdr_info);
    O(VAEncPictureParameterBufferAV1, number_skip_frames);
    O(VAEncPictureParameterBufferAV1, skip_frames_reduced_size);
    S(VAEncTileGroupBufferAV1);

    S(VAEncMiscParameterRateControl);
    O(VAEncMiscParameterRateControl, rc_flags);
    O(VAEncMiscParameterRateControl, ICQ_quality_factor);
    O(VAEncMiscParameterRateControl, max_qp);
    S(VAEncMiscParameterFrameRate);
    O(VAEncMiscParameterFrameRate, framerate_flags);
    S(VAEncMiscParameterHRD);
    O(VAEncMiscParameterHRD, buffer_size);
    S(VAEncMiscParameterBufferQualityLevel);
    S(VAEncPackedHeaderParameterBuffer);
    O(VAEncPackedHeaderParameterBuffer, bit_length);
    O(VAEncPackedHeaderParameterBuffer, has_emulation_bytes);
    S(VACodedBufferSegment);
    O(VACodedBufferSegment, bit_offset);
    O(VACodedBufferSegment, status);
    O(VACodedBufferSegment, buf);
    O(VACodedBufferSegment, next);
    return 0;
}
