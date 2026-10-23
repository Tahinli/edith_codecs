//! The header of a slice segment (7.4.7.1). A dependent slice segment has no
//! slice header of its own: it takes the values of the independent segment before
//! it. Only its address, its entry points and its position in the NAL unit belong
//! to it.

use ec_h265_syntax::slice::SliceHeader;

/// The header a dependent slice segment decodes with: `base` (the header of the
/// slice's independent segment) with the segment's own fields.
pub fn inherit(base: &SliceHeader, segment: &SliceHeader) -> SliceHeader {
    let mut header = base.clone();
    header.first_slice_segment_in_pic = segment.first_slice_segment_in_pic;
    header.dependent_slice_segment = true;
    header.segment_address = segment.segment_address;
    header.entry_point_offsets = segment.entry_point_offsets.clone();
    header
}
