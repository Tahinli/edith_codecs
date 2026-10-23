//! The in-loop filters run per CTB row, behind the reconstruction.
//!
//! The filters of 8.7 are defined over the whole picture: all deblocking, then all
//! SAO. Done one CTB row at a time they give the same samples when the rows are
//! run in this order, with `r` the CTB row:
//!
//! - Deblocking of row `r` (its vertical edges, then its horizontal edges, its top
//!   edge included) needs row `r + 1` reconstructed: intra prediction of row `r + 1`
//!   reads the unfiltered bottom line of row `r`.
//! - SAO of row `r` needs the deblocked rows `r - 1` (last line), `r` and `r + 1`
//!   (first line). Deblocking of row `r + 1` changes the bottom lines of row `r`, so
//!   SAO of `r` runs after that.
//!
//! So after row `k` is reconstructed the filter runs deblocking of row `k - 1`, then
//! SAO of row `k - 2`. A row is final once its SAO has run; the finished rows are a
//! prefix of the picture and can be published ([`super::shared::SharedPic`]).
//!
//! SAO of a row reads deblocked samples from a copy (`shadow`) that holds the row, its
//! neighbour lines and nothing else that SAO has changed, so the SAO of the row above
//! (which overwrote its own samples) never leaks into it. The last deblocked line of
//! the row above is kept in `above` for the same reason.

use super::deblock;
use super::pic::PicState;
use super::sao;

/// The filter state of one picture, advanced as its CTB rows are reconstructed.
#[derive(Default)]
pub struct RowFilter {
    /// The next CTB row to deblock.
    deblocked: usize,
    /// The next CTB row to run SAO on.
    filtered: usize,
    /// Per colour component: the deblocked last sample line of the CTB row above.
    above: [Vec<u16>; 3],
    /// Per colour component: the SAO input, a plane-sized buffer.
    shadow: [Vec<u16>; 3],
}

impl RowFilter {
    /// The filter state of a picture that has no rows filtered yet.
    pub fn new() -> RowFilter {
        RowFilter::default()
    }

    /// Run every filter step the reconstruction allows. `complete` is the number of
    /// CTB rows of `pic` that are fully reconstructed (rows `0..complete`); the whole
    /// picture is complete when it equals `pic.ctb_rows`. Returns the number of luma
    /// rows whose samples are final: the rows `0..returned` will not change again.
    pub fn advance(&mut self, pic: &mut PicState, complete: usize) -> usize {
        let rows = pic.ctb_rows;
        let all = complete >= rows;
        let ctb = 1usize << pic.ctb_log2;
        while self.deblocked < rows && (self.deblocked + 2 <= complete || all) {
            let r = self.deblocked;
            if pic.has_pixels() {
                deblock::deblock_rows(pic, r * ctb..(r + 1) * ctb);
            }
            self.deblocked += 1;
        }
        while self.filtered < rows
            && (self.filtered + 2 <= self.deblocked || self.deblocked == rows)
        {
            let r = self.filtered;
            self.sao_row(pic, r);
            self.filtered += 1;
        }
        if self.filtered == rows {
            pic.height
        } else {
            (self.filtered * ctb).min(pic.height)
        }
    }

    /// SAO of CTB row `r` for the three planes, then remember its last deblocked line.
    fn sao_row(&mut self, pic: &mut PicState, r: usize) {
        if !pic.has_pixels() {
            return;
        }
        let ctb = 1usize << pic.ctb_log2;
        // Does any CTB of the row use SAO? Without one the row is final as it is.
        let sao_on = (r * pic.ctb_cols..(r + 1) * pic.ctb_cols).any(|i| {
            pic.ctb
                .get(i)
                .is_some_and(|c| c.sao.type_idx.iter().any(|&t| t != 0))
        });
        for c in 0..3 {
            let (pw, ph, sub) = plane_geometry(pic, c);
            let ctb_c = ctb >> sub;
            let y0 = r * ctb_c;
            let y1 = (y0 + ctb_c).min(ph);
            if y0 >= y1 {
                continue;
            }
            if sao_on {
                let len = pw * ph;
                if self.shadow[c].len() != len {
                    self.shadow[c] = vec![0; len];
                }
                let shadow = &mut self.shadow[c];
                let plane = plane_samples(pic, c);
                if r > 0 {
                    shadow[(y0 - 1) * pw..y0 * pw].copy_from_slice(&self.above[c]);
                }
                let win_end = (y1 + 1).min(ph);
                shadow[y0 * pw..win_end * pw].copy_from_slice(&plane[y0 * pw..win_end * pw]);
            }
            // The deblocked last line, kept before SAO rewrites this row.
            self.above[c] = plane_samples(pic, c)[(y1 - 1) * pw..y1 * pw].to_vec();
            if sao_on {
                let mut band = plane_samples(pic, c)[y0 * pw..y1 * pw].to_vec();
                sao::sao_ctb_row(pic, c, &self.shadow[c], r, &mut band);
                plane_samples_mut(pic, c)[y0 * pw..y1 * pw].copy_from_slice(&band);
            }
        }
    }
}

/// Width, height and the vertical shift from luma to this plane (0 luma, `sub_y` chroma).
fn plane_geometry(pic: &PicState, c: usize) -> (usize, usize, usize) {
    if c == 0 {
        (pic.width, pic.height, 0)
    } else {
        (
            pic.width.div_ceil(1 << pic.sub_x()),
            pic.height.div_ceil(1 << pic.sub_y()),
            pic.sub_y() as usize,
        )
    }
}

fn plane_samples(pic: &PicState, c: usize) -> &[u16] {
    match c {
        0 => &pic.planes.y,
        1 => &pic.planes.cb,
        _ => &pic.planes.cr,
    }
}

fn plane_samples_mut(pic: &mut PicState, c: usize) -> &mut [u16] {
    match c {
        0 => &mut pic.planes.y,
        1 => &mut pic.planes.cb,
        _ => &mut pic.planes.cr,
    }
}
