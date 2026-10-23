//! Inter prediction of the coding units of a P or B slice (8.5.3.3).
//!
//! [`InterRefs`] holds what a slice's prediction units read: the final (in-loop
//! filtered) samples of every picture in `RefPicList0` and `RefPicList1`, in list
//! order, and the explicit weights when the PPS turns weighted prediction on.
//! [`super::cu_syntax`] calls [`super::mc::predict_pu`] for each prediction unit,
//! right after its motion is derived, then adds the coding unit's residual.

use super::cu_syntax::{Motion, ResidualBlock};
use super::mc::{self, Block, McScratch, PlaneMut, PlaneRef, RefPicture};
use super::pic::PicState;
use super::shared::SharedPic;
use super::weighted::ExplicitWeights;
use crate::decode::DecodedPicture;
use ec_core::error::Result;
use ec_h265_syntax::Sps;
use ec_h265_syntax::slice::SliceHeader;
use std::sync::{Arc, RwLockReadGuard};

/// The reference pictures and weights of one P or B slice.
pub struct InterRefs {
    /// `pics[list][ref_idx]`: the reference pictures, in list order. A picture may still be
    /// being decoded; a prediction waits for the rows it reads (see [`InterRefs::predict`]).
    pub pics: [Vec<Arc<SharedPic>>; 2],
    /// Explicit weights (`pred_weight_table()` of the slice), when weighted prediction applies.
    pub weights: Option<ExplicitWeights>,
    /// The log2 chroma subsampling `(sub_x, sub_y)` of the sequence.
    pub sub: (u32, u32),
}

impl InterRefs {
    /// The reference set of a slice: the pictures of its lists, and its explicit weights.
    pub fn new(
        pics: [Vec<Arc<SharedPic>>; 2],
        header: &SliceHeader,
        sps: &Sps,
    ) -> Result<InterRefs> {
        let weights = match header.pred_weight_table.as_ref() {
            Some(table) => Some(ExplicitWeights::from_table(
                table,
                sps.bit_depth_luma_minus8 + 8,
                sps.bit_depth_chroma_minus8 + 8,
                sps.range.high_precision_offsets_enabled,
            )?),
            None => None,
        };
        Ok(InterRefs {
            pics,
            weights,
            sub: crate::decode::chroma_shifts(sps.chroma_format_idc),
        })
    }

    /// Predict one prediction unit into `out` (8.5.3.3). Before the samples are read,
    /// the unit waits until each reference has its rows final: the block, the filter
    /// taps below it (four rows) and the vertical motion. The read lock on a reference
    /// is held only for this call, so a writer never waits on a reader that waits on it.
    pub fn predict(
        &self,
        motion: &Motion,
        pu: Block,
        out: &mut [PlaneMut<'_>; 3],
        scratch: &mut McScratch,
    ) -> Result<()> {
        for list in 0..2 {
            if let Some(pic) = self.picture(motion, list) {
                // The luma taps reach 4 rows below the block, the chroma taps 2 chroma
                // rows; a vector far above the picture clamps its reads to row 0, so at
                // least one row is always waited for.
                let bottom = (pu.y + pu.height) as i64 + (i64::from(motion.mv[list][1]) >> 2) + 16;
                pic.wait_rows(bottom.clamp(1, pic.height as i64) as usize)?;
            }
        }
        let pics = [self.picture(motion, 0), self.picture(motion, 1)];
        let guards = lock_ref_planes(pics, || {});
        // A list that names the picture of the other list reads through that lock.
        let same = matches!(pics, [Some(a), Some(b)] if std::ptr::eq(a, b));
        let planes = [
            guards[0].as_deref(),
            if same {
                guards[0].as_deref()
            } else {
                guards[1].as_deref()
            },
        ];
        // `predict_pu` indexes a list by `ref_idx`, so each used list is a slice whose
        // entries up to that index all name the one picture.
        let mut lists: [Vec<RefPicture<'_>>; 2] = [Vec::new(), Vec::new()];
        for list in 0..2 {
            if let (Some(pic), Some(idx)) = (planes[list], self.index(motion, list)) {
                lists[list] = vec![ref_picture(pic); idx + 1];
            }
        }
        mc::predict_pu_fmt(
            [lists[0].as_slice(), lists[1].as_slice()],
            motion,
            self.weights.as_ref(),
            pu,
            self.sub,
            out,
            scratch,
        )
    }

    /// The reference index of `list` when the list is used and names a picture.
    fn index(&self, motion: &Motion, list: usize) -> Option<usize> {
        if !motion.pred_flags[list] {
            return None;
        }
        usize::try_from(motion.ref_idx[list])
            .ok()
            .filter(|&i| i < self.pics[list].len())
    }

    /// The picture a used list of `motion` names.
    fn picture(&self, motion: &Motion, list: usize) -> Option<&SharedPic> {
        self.index(motion, list).map(|i| &*self.pics[list][i])
    }
}

/// Read-lock the planes of the pictures `pics` names, each distinct picture once. Both
/// lists of a B slice often name one picture; a second read lock taken by the same thread
/// deadlocks as soon as the picture's worker queues a write in between. `between` runs
/// after the first lock is taken (the tests use it to queue that write).
fn lock_ref_planes<'a>(
    pics: [Option<&'a SharedPic>; 2],
    between: impl FnOnce(),
) -> [Option<RwLockReadGuard<'a, DecodedPicture>>; 2] {
    let same = matches!(pics, [Some(a), Some(b)] if std::ptr::eq(a, b));
    let first = pics[0].map(|p| p.read_planes());
    between();
    let second = match pics[1] {
        Some(p) if !same => Some(p.read_planes()),
        _ => None,
    };
    [first, second]
}

/// The three planes of a decoded picture, for motion compensation.
fn ref_picture(pic: &DecodedPicture) -> RefPicture<'_> {
    let (sx, sy) = pic.chroma_shifts();
    let (cw, ch) = (pic.width.div_ceil(1 << sx), pic.height.div_ceil(1 << sy));
    [
        PlaneRef {
            samples: &pic.y,
            stride: pic.width,
            width: pic.width,
            height: pic.height,
            bit_depth: pic.bit_depth,
        },
        PlaneRef {
            samples: &pic.cb,
            stride: cw,
            width: cw,
            height: ch,
            bit_depth: pic.bit_depth_c,
        },
        PlaneRef {
            samples: &pic.cr,
            stride: cw,
            width: cw,
            height: ch,
            bit_depth: pic.bit_depth_c,
        },
    ]
}

/// Add the residual blocks of an inter coding unit to the samples its prediction
/// units already wrote.
pub fn add_residuals(pic: &mut PicState, residuals: &[ResidualBlock]) {
    if !pic.has_pixels() {
        return;
    }
    for block in residuals {
        pic.add_residual(
            block.component,
            block.x,
            block.y,
            block.size,
            &block.samples,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// Both lists of a B slice naming one picture: the picture is read-locked once, so a
    /// writer that queues up while the prediction holds the lock does not deadlock it.
    /// The watchdog turns a deadlock into a failure.
    #[test]
    fn shared_reference_in_both_lists_locks_once() {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (w, h) = (64usize, 64usize);
            let pic = Arc::new(SharedPic::new(0, w, h, (8, 8), true, 1));
            let src = DecodedPicture {
                width: w,
                height: h,
                bit_depth: 8,
                bit_depth_c: 8,
                chroma_format_idc: 1,
                y: vec![0; w * h],
                cb: vec![0; (w / 2) * (h / 2)],
                cr: vec![0; (w / 2) * (h / 2)],
            };
            let writer = {
                let pic = Arc::clone(&pic);
                let src = src.clone();
                move || {
                    for to in 1..=h {
                        pic.publish_rows(&src, to);
                    }
                }
            };
            let mut writer_thread = None;
            let guards = lock_ref_planes([Some(&*pic), Some(&*pic)], || {
                // The picture's worker publishes rows now and queues behind the read lock.
                writer_thread = Some(std::thread::spawn(writer.clone()));
                std::thread::sleep(Duration::from_millis(100));
            });
            drop(guards);
            if let Some(t) = writer_thread {
                t.join().expect("writer");
            }
            let _ = tx.send(());
        });
        rx.recv_timeout(Duration::from_secs(20))
            .expect("prediction deadlocked: a reference picture was read-locked twice");
    }
}
