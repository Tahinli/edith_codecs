use super::*;

/// One plane of the picture being coded, and the reconstruction being built
/// beside it.
/// What one committed 4x4 cell left behind for the coefficient contexts of
/// the blocks that read it as a neighbour -- tile.rs `neighbour_state`, which
/// is the same state decode.rs records.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct CoefCtx {
    /// spec `cul_level`: the sum of the unit's coded magnitudes, clamped to 7.
    pub(super) level: u8,
    /// The DC sign this cell votes with: `-1` negative, `1` positive, `0` when
    /// the DC itself was zero (tile.rs `dc_vote`).
    pub(super) dc: i8,
}

/// One plane's coefficient contexts, committed in CODING ORDER: the search
/// prices partition TREES, so the neighbour state a candidate reads has to be
/// whatever the blocks coded before it left -- the parent's committed
/// neighbours for a tree's first child, the just-committed sibling for the
/// later ones. Keeping the map as a full per-4x4-cell grid rather than the
/// writer's two running bands is what makes that fall out: a trial commits
/// over its own rectangle ([`Plane::commit`]) and a losing trial is undone
/// with the pixels ([`Plane::restore`]), so what a candidate reads above and
/// to its left is exactly what the tile writer's `Neighbours` bands will hold
/// when it gets there.
///
/// Before this map the pricer took both contexts as zero, which is exact for
/// a luma transform covering its whole block and the largest remaining row of
/// the pricer census everywhere else (all-zero chroma under-priced 41-81%).
#[derive(Default)]
pub(super) struct CoefCtxMap {
    pub(super) cells: Vec<CoefCtx>,
    pub(super) stride: usize,
    /// Whether the transforms being priced right now are SMALLER than the
    /// block they sit in -- a `TxMode::Select` depth split
    /// ([`Plane::code_tx_depth`]) or an inter var-tx split
    /// ([`commit_inter_luma`]). That is the one thing the (x, y, side) of a
    /// trial cannot tell, and it is what decides whether luma reads context 0
    /// or `SKIP_CONTEXTS` (tile.rs `write_luma_tus`).
    pub(super) tu_split: bool,
}

/// Whether [`Plane::block_sse`] scores a block the true frame edge cuts
/// through over its INSIDE part alone. On; `EC_AV1_EDGESSE=0` restores the
/// padding-inclusive score the search used before lane-sse, which is how the
/// before/after arms of the straddling gate rows are read off ONE binary.
pub(super) fn edge_sse() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_EDGESSE")
            .ok()
            .map(|v| v != "0")
    });
    ENV.unwrap_or(true)
}

/// Whether [`Plane::block_sse`] counts the blocks the true frame edge cuts
/// through and the share of their raw squared error that sat in the padding
/// (`EC_AV1_EDGE_CENSUS=1`). Off by default: only edge blocks would pay for
/// it, but the numbers are an instrument, not a decision (class
/// `gate-blind-to-feature` -- both gate films are superblock-aligned, so
/// without this census no gate row can say the clip fires at all).
pub(super) fn edge_census() -> bool {
    static ENV: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_EDGE_CENSUS").ok().as_deref() == Some("1")
    });
    *ENV
}

/// Per block side (indexed by `side.trailing_zeros()`, so 2 is 4x4 and 7 is
/// 128x128): how many scores clipped, the inside squared error they kept and
/// the padding squared error they dropped.
pub(super) static EDGE_SCORES: [[std::sync::atomic::AtomicU64; 3]; 8] =
    [const { [const { std::sync::atomic::AtomicU64::new(0) }; 3] }; 8];

/// One clipped score, for [`take_edge_census`].
pub(super) fn record_edge_score(side: usize, inside: u64, padding: u64) {
    let row = &EDGE_SCORES[(side.trailing_zeros() as usize).min(7)];
    row[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    row[1].fetch_add(inside, std::sync::atomic::Ordering::Relaxed);
    row[2].fetch_add(padding, std::sync::atomic::Ordering::Relaxed);
}

/// The straddling-block census since the last call, and zero it: one
/// `(side, scores, inside_sse, padding_sse)` row per block size that fired.
#[must_use]
pub fn take_edge_census() -> Vec<(usize, u64, u64, u64)> {
    EDGE_SCORES
        .iter()
        .enumerate()
        .filter_map(|(log2, row)| {
            let n = row[0].swap(0, std::sync::atomic::Ordering::Relaxed);
            let inside = row[1].swap(0, std::sync::atomic::Ordering::Relaxed);
            let padding = row[2].swap(0, std::sync::atomic::Ordering::Relaxed);
            (n != 0).then_some((1usize << log2, n, inside, padding))
        })
        .collect()
}

pub(super) struct Plane<'a> {
    pub(super) source: &'a [u8],
    pub(super) reconstruction: Vec<u8>,
    pub(super) width: usize,
    pub(super) height: usize,
    /// The frame's true, decodable extent in this plane's own units --
    /// `mi_cols`/`mi_rows` (spec `compute_image_size`) converted to samples,
    /// not `width`/`height` above, which is the padded coding-surface size.
    /// A decoder's `BlockDecoded` bookkeeping (spec 7.4) and its `mb_to_
    /// right_edge`/`mb_to_bottom_edge` clamps (libaom's
    /// `av1/common/reconintra.c` `build_intra_predictors`) never see past
    /// this bound, even though the padded reconstruction buffer holds real
    /// (if never-decoded-by-a-real-decoder) samples out there -- reading
    /// those instead of stopping here is what desyncs prediction at the true
    /// edge.
    pub(super) true_width: usize,
    pub(super) true_height: usize,
    /// This plane's own tile's pixel bounds, in this plane's units -- the
    /// encoder side of the decoder's `PlaneBuf::set_tile_origin`: intra
    /// prediction never reaches across a tile edge even though the
    /// neighbouring tile's real samples sit right there in the shared
    /// reconstruction. `(0, 0, width, height)` is the single-tile case and
    /// leaves every edge exactly as it was before tiles existed.
    pub(super) tile_x0: usize,
    pub(super) tile_y0: usize,
    pub(super) tile_x1: usize,
    pub(super) tile_y1: usize,
    /// [`CoefCtxMap`]: what the blocks already committed left for the
    /// coefficient contexts of the ones still being priced. Sized on the
    /// first commit, so a plane literal does not have to know its own extent
    /// twice.
    pub(super) ctx: CoefCtxMap,
}

impl Plane<'_> {
    /// Clips this plane's edge reads to one tile ([`Self::tile_x0`]),
    /// called at every superblock of the frame-raster search with the rect
    /// of the tile that superblock belongs to.
    pub(super) fn set_tile(&mut self, x0: usize, y0: usize, x1: usize, y1: usize) {
        self.tile_x0 = x0;
        self.tile_y0 = y0;
        self.tile_x1 = x1.min(self.width);
        self.tile_y1 = y1.min(self.height);
    }
    /// The reconstructed samples a block at `(x, y)` predicts from: the row
    /// above it, the column to its left and the sample between them, each
    /// missing where the block sits against an edge of the frame.
    ///
    /// `reach` says whether the samples above the block's right and below its
    /// left are decoded, which is what a directional mode reads into; where
    /// they are not, or where the frame ends first, the edge is shorter and
    /// the predictor repeats its last sample, exactly as the decoder's clamp
    /// to `aboveLimit` and `leftLimit` does (spec 7.11.2.2).
    /// lane-av1speed2: the two edges used to be a `Vec` each -- a malloc and
    /// a free per mode per block, on the search's hottest path -- while an
    /// edge is at most `2 * side` samples, which is `2 * BLOCK`. Same
    /// samples, on the caller's stack.
    pub(super) fn edges_into<'a>(
        &self,
        x: usize,
        y: usize,
        side: usize,
        reach: Reach,
        above_buf: &'a mut [u8; 2 * SUPERBLOCK],
        left_buf: &'a mut [u8; 2 * SUPERBLOCK],
    ) -> (Option<&'a [u8]>, Option<&'a [u8]>, Option<u8>) {
        // Even a block's *own* edge is clamped to the true frame bound, not
        // just a `reach` extension: libaom's `av1_predict_intra_block`
        // (`av1/common/reconintra.c`) derives `xr`/`yd` -- the distance from
        // this block's own right/bottom to the frame edge -- from
        // `mb_to_right_edge`/`mb_to_bottom_edge`, which are built from
        // `mi_cols`/`mi_rows` (the true, decodable extent), never from any
        // padded coding surface. `n_top_px`/`n_left_px` (how many *real*
        // samples the row/column above/left holds before the decoder starts
        // repeating the last one, same clamp as spec 7.11.2.2's `aboveLimit`/
        // `leftLimit`) is `min(side, xr + side)` even for a block whose own
        // extent straddles that edge legally (`has_half`) -- the transform
        // still covers the whole block, but prediction does not get to read
        // real reconstructed pixels past the true edge for its own row/column,
        // only for the padded tail it invents.
        let own_across = x + side.min(self.true_width.saturating_sub(x));
        let across = if reach.above_right {
            own_across + side.min(self.true_width.saturating_sub(own_across))
        } else {
            own_across
        }
        .min(self.width);
        let own_down = y + side.min(self.true_height.saturating_sub(y));
        let down = if reach.below_left {
            own_down + side.min(self.true_height.saturating_sub(own_down))
        } else {
            own_down
        }
        .min(self.height);
        // `across`/`down` can still fall at or below `x`/`y` for a sub-block
        // whose own origin already sits past the true edge -- only reachable
        // through a partition trial priced for comparison on a split this
        // frame's straddling quadrant will go on to refuse (see
        // `a_picture_off_the_block_grid_is_refused`), never through a block
        // this encoder actually commits. Such a block has no true-edge
        // samples above or left of it at all, same as `y == 0`/`x == 0`.
        let across = across.min(self.tile_x1);
        let down = down.min(self.tile_y1);
        let above_n = (y > self.tile_y0 && across > x).then(|| across - x);
        if let Some(n) = above_n {
            above_buf[..n].copy_from_slice(&self.reconstruction[(y - 1) * self.width + x..][..n]);
        }
        let left_n = (x > self.tile_x0 && down > y).then(|| down - y);
        if let Some(n) = left_n {
            for (d, row) in left_buf[..n].iter_mut().zip(y..down) {
                *d = self.reconstruction[row * self.width + x - 1];
            }
        }
        let corner = (x > self.tile_x0 && y > self.tile_y0)
            .then(|| self.reconstruction[(y - 1) * self.width + x - 1]);
        (
            above_n.map(|n| &above_buf[..n]),
            left_n.map(|n| &left_buf[..n]),
            corner,
        )
    }

    /// Codes one block under one mode without committing it: hands back the
    /// levels, the block the decoder would reconstruct, the squared error
    /// against the source and an estimate of what the levels cost in bits.
    pub(super) fn trial(
        &self,
        at: At,
        mode: u8,
        angle_delta: i32,
        base_q_idx: u8,
        deadzone: f64,
        fctx: &crate::decode::FrameCtx,
    ) -> Trial {
        self.trial_typed(
            at,
            mode,
            angle_delta,
            base_q_idx,
            deadzone,
            TxType::DctDct,
            fctx,
        )
    }

    /// [`Self::trial`] under a named transform type -- what an intra chroma
    /// block gets, since chroma codes no `tx_type` symbol and the decoder
    /// derives the type from the chroma mode itself (`Intra_Mode_To_Tx_Type`,
    /// spec 9.3, [`crate::decode::default_intra_tx_type`]). Luma keeps
    /// `DCT_DCT`, which is the one type this writer's `tx_type` symbol names.
    #[allow(clippy::too_many_arguments)]
    /// Sum of absolute differences between the source block at `(x, y)` and a
    /// `side x side` prediction, as the whole number it is.
    ///
    /// Bit-identical to the `f64` accumulation it replaces: every partial sum
    /// is an integer below `255 * 64 * 64` (under 2^21), which `f64` holds
    /// exactly, so no add here rounds and the order of the adds cannot change
    /// the result. Dropping the per-pixel `i / side` and `i % side` -- an
    /// integer division per sample -- is what lets the row walk vectorize.
    pub(super) fn block_sad(&self, x: usize, y: usize, side: usize, prediction: &[u8]) -> f64 {
        let mut sad = 0u32;
        for row in 0..side {
            let source = &self.source[(y + row) * self.width + x..][..side];
            for (&s, &p) in source.iter().zip(&prediction[row * side..][..side]) {
                sad += u32::from(s.abs_diff(p));
            }
        }
        f64::from(sad)
    }

    /// Squared error between the source block at `(x, y)` and a reconstruction
    /// of it, over the part of the block that is INSIDE the true frame.
    /// Exact for the same reason [`Self::block_sad`] is: the largest total is
    /// `255^2 * 64 * 64`, under 2^28.
    ///
    /// A block the frame edge cuts through is still coded whole -- the
    /// decoder reconstructs its outside samples too and the display crop
    /// throws them away -- so scoring the padding would rank a mode, a
    /// transform or a partition tree by how well it codes samples the source
    /// does not have (libaom clips the same way: `av1_dist_block` measures
    /// over `max_blocks_wide`/`max_blocks_high`). The padding is replicated
    /// edge pixels, so predicting it is nearly free, but coding coefficients
    /// for it spends real bits. Both gate films are superblock-aligned; this
    /// only reaches arbitrary export sizes (lane-sse, generalising
    /// lane-b64b's 64-root-only rescore of the 64 root).
    pub(super) fn block_sse(&self, x: usize, y: usize, side: usize, reconstruction: &[u8]) -> f64 {
        let inside_rows = side.min(self.true_height.saturating_sub(y));
        let inside_cols = side.min(self.true_width.saturating_sub(x));
        let straddles = inside_rows < side || inside_cols < side;
        // The knob is read only for a block the edge cuts, so the interior
        // path -- the hot one -- carries no extra load at all.
        let (rows, cols) = match straddles && !edge_sse() {
            true => (side, side),
            false => (inside_rows, inside_cols),
        };
        let mut sse = 0u64;
        for row in 0..rows {
            let source = &self.source[(y + row) * self.width + x..][..cols];
            for (&s, &r) in source.iter().zip(&reconstruction[row * side..][..cols]) {
                let d = u32::from(s.abs_diff(r));
                sse += u64::from(d * d);
            }
        }
        if straddles && edge_census() {
            let (mut inside, mut whole) = (0u64, 0u64);
            for row in 0..side {
                let source = &self.source[(y + row) * self.width + x..][..side];
                for (col, (&s, &r)) in source
                    .iter()
                    .zip(&reconstruction[row * side..][..side])
                    .enumerate()
                {
                    let d = u64::from(u32::from(s.abs_diff(r)).pow(2));
                    whole += d;
                    if row < inside_rows && col < inside_cols {
                        inside += d;
                    }
                }
            }
            record_edge_score(side, inside, whole - inside);
        }
        sse as f64
    }

    // Spec-shaped signature; bundling the operands would obscure the spec mapping.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn trial_typed(
        &self,
        at: At,
        mode: u8,
        // This block's `angle_delta_y`/`angle_delta_uv` (spec
        // `read_intra_angle_info`, -3..=3), zero for every non-directional
        // mode -- the predictor's own `angle_delta` argument.
        angle_delta: i32,
        base_q_idx: u8,
        deadzone: f64,
        tx_type: TxType,
        fctx: &crate::decode::FrameCtx,
    ) -> Trial {
        let At {
            x, y, side, reach, ..
        } = at;
        let (mut above_buf, mut left_buf) = ([0u8; 2 * SUPERBLOCK], [0u8; 2 * SUPERBLOCK]);
        let (above, left, corner) =
            self.edges_into(x, y, side, reach, &mut above_buf, &mut left_buf);
        // Per-candidate scratch on the search's hottest path, so both buffers
        // live on the stack -- `side` is at most `BLOCK` -- rather than
        // costing a malloc/free pair per trial (`mc_trial` already does this).
        let mut prediction = [0u8; BLOCK * BLOCK];
        let prediction = &mut prediction[..side * side];
        intra_predict_u8(
            mode,
            angle_delta,
            above,
            left,
            corner,
            side,
            side,
            false,
            false,
            prediction,
            fctx,
        );

        let mut residual = [0i32; BLOCK * BLOCK];
        let residual = &mut residual[..side * side];
        for row in 0..side {
            for col in 0..side {
                residual[row * side + col] =
                    i32::from(self.source[(y + row) * self.width + x + col])
                        - i32::from(prediction[row * side + col]);
            }
        }
        #[cfg(test)]
        let t = stage_start();
        // lane-rdoq: the levels the writer takes are the RDOQ pass's, so the
        // reconstruction below and the price further down are both of the
        // grid this trial would really code.
        let (levels, rdoq_bits) = if rdoq_on() {
            let (mut levels, scaled) = forward_and_quantize_typed_scaled(
                residual,
                side,
                8,
                i32::from(base_q_idx),
                deadzone,
                tx_type,
            );
            let (skip_ctx, sign_ctx) = self.coef_ctx(x, y, side, at.set);
            let bits = crate::tile::rdoq(
                &mut levels,
                &scaled,
                side,
                base_q_idx,
                at.set,
                skip_ctx,
                sign_ctx,
                rdoq_lambda(),
                tx_type,
            );
            (levels, Some(bits))
        } else {
            (
                forward_and_quantize_typed(
                    residual,
                    side,
                    8,
                    i32::from(base_q_idx),
                    deadzone,
                    tx_type,
                ),
                None,
            )
        };
        // lane-av1speed2: 60% of the transform units of an inter stream carry
        // no coefficient at all, and the square entry point re-inflates that
        // case into `side * side` zeros (one allocate-and-memset per trial)
        // only for the sum below to add them to the prediction. The `_wh`
        // entry hands back the empty all-zero marker instead, and a zero
        // residual reconstructs to the prediction itself -- `clamp(p + 0)` is
        // `p` for a `u8`, so the bytes are the same.
        let coded = dequant_and_inverse_typed_wh(
            &levels,
            side,
            side,
            8,
            i32::from(base_q_idx),
            0,
            0,
            tx_type,
            None,
        );
        #[cfg(test)]
        stage_since(2, t);

        let reconstruction: Vec<u8> = if coded.is_empty() {
            prediction.to_vec()
        } else {
            prediction
                .iter()
                .zip(&coded)
                .map(|(&p, &c)| (i32::from(p) + c).clamp(0, 255) as u8)
                .collect()
        };
        let sse = self.block_sse(x, y, side, &reconstruction);
        // What the levels cost is priced through the same CDFs the tile writer
        // will code them with, so the search ranks modes -- and the partition
        // trial ranks trees -- by the bits they actually spend.
        #[cfg(test)]
        let t = stage_start();
        let bits = if cfg!(test) && std::env::var_os("EC_AV1_ESTIMATE").is_some() {
            // What the search used before it could price a block exactly: a
            // level costs its magnitude's width plus a sign and a run. Kept
            // reachable from the sweep so the two rate terms can be compared
            // on one build.
            levels
                .iter()
                .filter(|&&level| level != 0)
                .map(|&level| 2.0 + 2.0 * f64::from(level.unsigned_abs() + 1).log2())
                .sum()
        } else if let Some(bits) = rdoq_bits {
            // Already priced, against the grid the pass settled on.
            bits
        } else {
            {
                let (skip_ctx, sign_ctx) = self.coef_ctx(x, y, side, at.set);
                crate::tile::coeff_bits_typed(
                    &levels,
                    at.set,
                    crate::decode::q_ctx_of(base_q_idx),
                    skip_ctx,
                    sign_ctx,
                    tx_type,
                )
            }
        };
        #[cfg(test)]
        stage_since(3, t);
        Trial {
            levels,
            reconstruction,
            sse,
            bits,
        }
    }

    /// [`Self::trial`] with the prediction already built, so that an inter
    /// block's motion-compensated prediction can go through the same
    /// residual/quantize/reconstruct path an intra block's does. `skip`
    /// takes the prediction as the reconstruction outright, coding no
    /// residual at all — what a `NEARESTMV` block that names no coefficients
    /// codes.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn code_from_prediction(
        &self,
        x: usize,
        y: usize,
        side: usize,
        prediction: &[u8],
        skip: bool,
        base_q_idx: u8,
        deadzone: f64,
        set: TxbSet,
    ) -> Trial {
        self.code_from_prediction_typed(
            x,
            y,
            side,
            prediction,
            skip,
            base_q_idx,
            deadzone,
            set,
            TxType::DctDct,
        )
    }

    /// [`Self::code_from_prediction`] coding the residual with a NAMED
    /// transform type (lane-txset2): the inter luma sets carry a two-type
    /// alphabet (`TX_SET_INTER_3`, `IDTX`/`DCT_DCT`) the writer already codes
    /// a symbol for, and [`commit_inter_luma`] prices both through this.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn code_from_prediction_typed(
        &self,
        x: usize,
        y: usize,
        side: usize,
        prediction: &[u8],
        skip: bool,
        base_q_idx: u8,
        deadzone: f64,
        set: TxbSet,
        tx_type: TxType,
    ) -> Trial {
        if skip {
            let sse = self.block_sse(x, y, side, prediction);
            return Trial {
                levels: vec![0i32; side * side],
                reconstruction: prediction.to_vec(),
                sse,
                bits: 0.0,
            };
        }
        // A 64x64 root's residual is four times the scratch every other
        // block coder here sizes for [`BLOCK`]; only that one case pays for
        // a heap buffer, so the 4x4..32x32 path keeps its stack array.
        let mut small = [0i32; BLOCK * BLOCK];
        let mut big = if side > BLOCK {
            vec![0i32; side * side]
        } else {
            Vec::new()
        };
        let residual: &mut [i32] = if side > BLOCK {
            &mut big
        } else {
            &mut small[..side * side]
        };
        for row in 0..side {
            for col in 0..side {
                residual[row * side + col] =
                    i32::from(self.source[(y + row) * self.width + x + col])
                        - i32::from(prediction[row * side + col]);
            }
        }
        #[cfg(test)]
        let t = stage_start();
        // lane-rdoq, as in [`Self::trial_typed`].
        let (levels, rdoq_bits) = if rdoq_on() {
            let (mut levels, scaled) = forward_and_quantize_typed_scaled(
                residual,
                side,
                8,
                i32::from(base_q_idx),
                deadzone,
                tx_type,
            );
            let (skip_ctx, sign_ctx) = self.coef_ctx(x, y, side, set);
            let bits = crate::tile::rdoq(
                &mut levels,
                &scaled,
                side,
                base_q_idx,
                set,
                skip_ctx,
                sign_ctx,
                rdoq_lambda(),
                tx_type,
            );
            (levels, Some(bits))
        } else {
            (
                forward_and_quantize_typed(
                    residual,
                    side,
                    8,
                    i32::from(base_q_idx),
                    deadzone,
                    tx_type,
                ),
                None,
            )
        };
        // The same all-zero shortcut [`Self::trial_typed`] takes.
        let coded = dequant_and_inverse_typed_wh(
            &levels,
            side,
            side,
            8,
            i32::from(base_q_idx),
            0,
            0,
            tx_type,
            None,
        );
        #[cfg(test)]
        stage_since(2, t);
        let reconstruction: Vec<u8> = if coded.is_empty() {
            prediction.to_vec()
        } else {
            prediction
                .iter()
                .zip(&coded)
                .map(|(&p, &c)| (i32::from(p) + c).clamp(0, 255) as u8)
                .collect()
        };
        let sse = self.block_sse(x, y, side, &reconstruction);
        // A 64-point transform codes only its top-left 32x32 corner (spec
        // 5.11.39's zero-out; `TxbSet::Luma64`'s tables are 32-sided and the
        // key frame writer takes a 32x32 level grid for one). The inverse
        // above reads the whole 64x64 array, but the price below and the
        // levels the writer takes are that corner.
        let levels = if side > BLOCK {
            coded_corner(&levels, side, BLOCK)
        } else {
            levels
        };
        #[cfg(test)]
        let t = stage_start();
        let bits = if cfg!(test) && std::env::var_os("EC_AV1_ESTIMATE").is_some() {
            levels
                .iter()
                .filter(|&&level| level != 0)
                .map(|&level| 2.0 + 2.0 * f64::from(level.unsigned_abs() + 1).log2())
                .sum()
        } else if let Some(bits) = rdoq_bits {
            bits
        } else {
            {
                let (skip_ctx, sign_ctx) = self.coef_ctx(x, y, side, set);
                crate::tile::coeff_bits_typed(
                    &levels,
                    set,
                    crate::decode::q_ctx_of(base_q_idx),
                    skip_ctx,
                    sign_ctx,
                    tx_type,
                )
            }
        };
        #[cfg(test)]
        stage_since(3, t);
        Trial {
            levels,
            reconstruction,
            sse,
            bits,
        }
    }

    /// Codes this block's luma at transform depth `depth`: the
    /// `(side >> depth)^2` transform units in raster order, each predicted
    /// from the reconstruction the units before it left behind -- exactly
    /// what the decoder's multi-transform-unit branch does (decode.rs
    /// `decode_block`, `tu_reach`) -- and each transformed, quantised and
    /// reconstructed on its own. Commits the result and hands back the
    /// block-coordinate levels, the block's squared error and what its
    /// coefficients cost.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn code_tx_depth(
        &mut self,
        at: At,
        mode: u8,
        angle_delta: i32,
        depth: usize,
        // This block's `filter_intra_mode` when it is coded with one, in
        // which case every transform unit predicts recursively off the
        // reconstruction the units before it left -- what the decoder does
        // per unit (decode.rs `decode_block`, `push_intra_rect`'s
        // `filter_intra`), not once over the whole block.
        filter_intra: Option<u8>,
        search: &Search,
        fctx: &crate::decode::FrameCtx,
    ) -> (Vec<i32>, f64, f64, Vec<TxType>) {
        let At {
            x, y, side, reach, ..
        } = at;
        let tx = side >> depth;
        let n = 1usize << depth;
        let set = luma_set_for(
            match tx {
                32 => TxbSet::Luma32,
                16 => TxbSet::Luma16,
                8 => TxbSet::Luma8,
                _ => TxbSet::Luma4,
            },
            search.screen,
        );
        let mut levels = vec![0i32; side * side];
        census_add(4, n * n);
        // A unit smaller than its block reads the neighbour magnitude table
        // for its `txb_skip` context, not context 0 ([`Plane::coef_ctx`]).
        self.ctx.tu_split = n > 1;
        let (mut sse, mut bits) = (0.0, 0.0);
        // One entry per unit at its own `tu_row * n + tu_col`, which is how
        // the writer indexes them (tile.rs `write_luma_tus`) -- a unit the
        // frame edge cuts away is never coded there, so a compacted list
        // would slide every later unit's type onto the wrong transform.
        let mut tx_types = vec![TxType::DctDct; n * n];
        for tu_row in 0..n {
            for tu_col in 0..n {
                let (col_off, row_off) = (tu_col * tx, tu_row * tx);
                let tu = At {
                    x: x + col_off,
                    y: y + row_off,
                    side: tx,
                    reach: Reach::of_tu(side, side, col_off, row_off, tx, tx, reach),
                    set,
                };
                let trial = match filter_intra {
                    Some(fi) => {
                        let (mut above_buf, mut left_buf) =
                            ([0u8; 2 * SUPERBLOCK], [0u8; 2 * SUPERBLOCK]);
                        let (above, left, corner) = self.edges_into(
                            tu.x,
                            tu.y,
                            tx,
                            tu.reach,
                            &mut above_buf,
                            &mut left_buf,
                        );
                        let mut prediction = vec![0u8; tx * tx];
                        filter_intra_predict_u8(
                            usize::from(fi),
                            above,
                            left,
                            corner,
                            tx,
                            &mut prediction,
                            fctx,
                        );
                        self.code_from_prediction(
                            tu.x,
                            tu.y,
                            tx,
                            &prediction,
                            false,
                            search.base_q_idx,
                            search.deadzone,
                            set,
                        )
                    }
                    None => {
                        // lane-txset: the unit picks its own transform type
                        // out of the set the writer can name it in, priced
                        // through the same RDOQ'd trial path (its `bits`
                        // already carry the `tx_type` symbol itself).
                        let mut best: Option<(f64, TxType, Trial)> = None;
                        for &tx_type in tx_type_candidates(set, search.screen) {
                            let candidate = self.trial_typed(
                                tu,
                                mode,
                                angle_delta,
                                search.base_q_idx,
                                search.deadzone,
                                tx_type,
                                fctx,
                            );
                            // lane-txi: a non-`DCT_DCT` type is charged its
                            // bits at `EC_AV1_TXRD_LAMBDA` times the mode
                            // search's weight (default 1 = unchanged). The
                            // film rows buy 0.04 dB for 1.2% more bytes at
                            // weight 1 on every frame and every PLANE, key
                            // frame included -- a rate term this decision
                            // under-charges (class
                            // `rd-rate-term-calibration`), not the
                            // propagation loss the leaves would have shown.
                            let cost = candidate.sse
                                + search.lambda * candidate.bits
                                + non_dct_surcharge(search.lambda, tx_type, candidate.bits);
                            if best.as_ref().is_none_or(|(best, _, _)| cost < *best) {
                                best = Some((cost, tx_type, candidate));
                            }
                        }
                        let (_, tx_type, trial) = best.expect("the candidate list is never empty");
                        tx_types[tu_row * n + tu_col] = tx_type;
                        TX_TYPE_HITS[tx_type as usize]
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        trial
                    }
                };
                sse += trial.sse;
                bits += trial.bits;
                self.commit(tu.x, tu.y, tx, &trial);
                for row in 0..tx {
                    levels[(row_off + row) * side + col_off..][..tx]
                        .copy_from_slice(&trial.levels[row * tx..][..tx]);
                }
            }
        }
        self.ctx.tu_split = false;
        (levels, sse, bits, tx_types)
    }

    /// Writes a trial's reconstruction back into the plane, and the
    /// coefficient context it leaves for its neighbours ([`CoefCtxMap`]).
    pub(super) fn commit(&mut self, x: usize, y: usize, side: usize, trial: &Trial) {
        for row in 0..side {
            self.reconstruction[(y + row) * self.width + x..][..side]
                .copy_from_slice(&trial.reconstruction[row * side..][..side]);
        }
        self.commit_ctx(x, y, side, &trial.levels);
    }

    /// Publishes one committed transform's own context over every 4x4 cell it
    /// covers -- tile.rs `neighbour_state` + `Neighbours::record_planes`,
    /// which is what the writer will read when it reaches the next block.
    pub(super) fn commit_ctx(&mut self, x: usize, y: usize, side: usize, levels: &[i32]) {
        if self.ctx.cells.is_empty() {
            self.ctx.stride = self.width.div_ceil(4);
            self.ctx.cells = vec![CoefCtx::default(); self.ctx.stride * self.height.div_ceil(4)];
        }
        let cul = levels.iter().map(|l| l.unsigned_abs()).sum::<u32>().min(7) as u8;
        let state = CoefCtx {
            level: cul,
            dc: match levels[0].signum() {
                0 => 0,
                -1 => -1,
                _ => 1,
            },
        };
        let n = (side / 4).max(1);
        // Only up to the frame's TRUE extent: the writer's own
        // `Neighbours::record_planes` clamps to `blocks_wide`/`blocks_high`
        // (spec `av1_set_entropy_contexts`, off the true `mi_cols`/`mi_rows`)
        // and leaves the cells past it uncoded even mid-block, so a
        // straddling block publishes context for its inside part only.
        let rows = n.min(self.true_height.saturating_sub(y).div_ceil(4));
        let cols = n.min(self.true_width.saturating_sub(x).div_ceil(4));
        for row in 0..rows {
            let start = (y / 4 + row) * self.ctx.stride + x / 4;
            let end = (start + cols).min(self.ctx.cells.len());
            if start < end {
                self.ctx.cells[start..end].fill(state);
            }
        }
    }

    /// A block committed without any residual at all ([`ibc_commit`]): the
    /// cells it covers coded nothing, and leaving an earlier trial's state
    /// there would price its neighbours off a block that was never coded.
    pub(super) fn commit_ctx_zero(&mut self, x: usize, y: usize, side: usize) {
        self.commit_ctx(x, y, side, &[0i32]);
    }

    /// The `txb_skip` and `dc_sign` contexts a transform at `(x, y)` of this
    /// plane reads, off what the blocks before it in coding order committed
    /// ([`CoefCtxMap`]) -- the writer's own derivation: chroma reads whether
    /// the above/left neighbours coded anything (tile.rs
    /// `write_block_planes`), luma reads context 0 when its transform covers
    /// its whole block and decode.rs `SKIP_CONTEXTS` off the neighbour
    /// magnitude tiers when it does not (tile.rs `write_luma_tus` /
    /// `Neighbours::luma_skip_ctx`), and both planes read the DC sign vote of
    /// the same neighbours (tile.rs `dc_sign_ctx`).
    ///
    /// A neighbour outside this plane's own TILE does not exist, exactly as
    /// the writer's per-tile `Neighbours` bands start blank.
    ///
    /// CLOSED (lane-av1straddle): the decisions this price changes used to
    /// make `decode_stream_round_trips_an_odd_size_gop` and
    /// `an_odd_size_gop_round_trips_bit_exact_against_the_encoder_reconstruction`
    /// fail at 216x96. Never this map: the writer coded the phantom
    /// transform units of a right-edge-straddling 32x32 (tile.rs
    /// `write_luma_tus`, now clipped to the frame's true `mi_cols`/`mi_rows`
    /// like decode.rs and libaom), which libdav1d and libaom both refused
    /// outright while a decision change was what first walked into it.
    pub(super) fn coef_ctx(&self, x: usize, y: usize, side: usize, set: TxbSet) -> (usize, usize) {
        if self.ctx.cells.is_empty() {
            return (0, 0);
        }
        let n = (side / 4).max(1);
        let (cx, cy) = (x / 4, y / 4);
        let (mut top, mut left) = (0u8, 0u8);
        let mut vote = 0i32;
        if y > self.tile_y0 && cy > 0 {
            for cell in &self.ctx.cells[(cy - 1) * self.ctx.stride + cx..][..n] {
                top |= cell.level;
                vote += i32::from(cell.dc);
            }
        }
        if x > self.tile_x0 && cx > 0 {
            for row in 0..n {
                let cell = self.ctx.cells[(cy + row) * self.ctx.stride + cx - 1];
                left |= cell.level;
                vote += i32::from(cell.dc);
            }
        }
        let skip_ctx = if set.is_chroma() {
            usize::from(top != 0) + usize::from(left != 0)
        } else if self.ctx.tu_split {
            crate::decode::SKIP_CONTEXTS[usize::from(top).min(4)][usize::from(left).min(4)]
        } else {
            0
        };
        let sign_ctx = match vote.signum() {
            0 => 0,
            -1 => 1,
            _ => 2,
        };
        (skip_ctx, sign_ctx)
    }

    /// The source samples of one square, contiguous — what a motion search
    /// compares its candidates' predictions against ([`motion::search`]'s
    /// `source`), since [`Self::source`] itself is the whole plane, strided
    /// by [`Self::width`].
    pub(super) fn source_block(&self, x: usize, y: usize, side: usize) -> Vec<u8> {
        rows_of(self.source, self.width, x, y, side)
    }

    /// lane-b128hv: [`Self::source_block`] over a rectangle, for the 128
    /// root's own HORZ/VERT motion search.
    pub(super) fn source_block_rect(&self, x: usize, y: usize, w: usize, h: usize) -> Vec<u8> {
        let mut out = vec![0u8; w * h];
        for row in 0..h {
            out[row * w..][..w].copy_from_slice(&self.source[(y + row) * self.width + x..][..w]);
        }
        out
    }

    /// The reconstructed samples of one square, so that a partition trial can
    /// be undone.
    /// The reconstructed samples of one square AND the coefficient contexts
    /// its cells hold, so that a partition trial can be undone whole: a
    /// losing trial that left its own nz/DC state behind would price the
    /// blocks after it off a block the tile writer never codes.
    pub(super) fn snapshot(&self, x: usize, y: usize, side: usize) -> (Vec<u8>, Vec<CoefCtx>) {
        (
            rows_of(&self.reconstruction, self.width, x, y, side),
            self.ctx_rows(x, y, side),
        )
    }

    pub(super) fn ctx_rows(&self, x: usize, y: usize, side: usize) -> Vec<CoefCtx> {
        if self.ctx.cells.is_empty() {
            return Vec::new();
        }
        let n = (side / 4).max(1);
        (0..n)
            .flat_map(|row| {
                let start = (y / 4 + row) * self.ctx.stride + x / 4;
                self.ctx.cells[start..(start + n).min(self.ctx.cells.len())].to_vec()
            })
            .collect()
    }

    /// Puts a snapshot back.
    pub(super) fn restore(&mut self, x: usize, y: usize, side: usize, saved: &(Vec<u8>, Vec<CoefCtx>)) {
        for row in 0..side {
            self.reconstruction[(y + row) * self.width + x..][..side]
                .copy_from_slice(&saved.0[row * side..][..side]);
        }
        if saved.1.is_empty() {
            return;
        }
        let n = (side / 4).max(1);
        for row in 0..n {
            let start = (y / 4 + row) * self.ctx.stride + x / 4;
            let end = (start + n).min(self.ctx.cells.len());
            if start < end {
                self.ctx.cells[start..end].copy_from_slice(&saved.1[row * n..][..end - start]);
            }
        }
    }

    /// The SAD-plus-mode-cost score [`Self::search_block`] ranks candidates
    /// by, exposed so [`search_inter_block`]'s intra-candidate loop can
    /// prescreen by the identical rule rather than a second one that could
    /// drift from it.
    pub(super) fn intra_scores(
        &self,
        at: At,
        modes: &[u8],
        mode_bits: &[f64; 13],
        lambda: f64,
        fctx: &crate::decode::FrameCtx,
    ) -> Vec<(f64, u8)> {
        let At {
            x, y, side, reach, ..
        } = at;
        let (mut above_buf, mut left_buf) = ([0u8; 2 * SUPERBLOCK], [0u8; 2 * SUPERBLOCK]);
        let (above, left, corner) =
            self.edges_into(x, y, side, reach, &mut above_buf, &mut left_buf);
        modes
            .iter()
            .map(|&mode| {
                let mut prediction = vec![0u8; side * side];
                intra_predict_u8(
                    mode,
                    0,
                    above,
                    left,
                    corner,
                    side,
                    side,
                    false,
                    false,
                    &mut prediction,
                    fctx,
                );
                let sad = self.block_sad(x, y, side, &prediction);
                (sad + lambda * mode_bits[usize::from(mode)], mode)
            })
            .collect()
    }

    /// Codes one block under every mode the search offers (or, when
    /// [`Search::top_k`] prunes it, the cheapest `top_k` of them by a SAD
    /// pre-pass plus DC) and commits the one whose squared error and
    /// estimated rate come out cheapest.
    ///
    /// The prediction is built once per mode regardless -- the full RD trial
    /// needs it too -- so ranking modes by SAD on that same prediction before
    /// running the expensive part (forward transform, quantize, reconstruct,
    /// `coeff_bits`) on only the survivors costs nothing extra to predict,
    /// only to sum.
    pub(super) fn search_block(
        &mut self,
        at: At,
        search: &Search,
        mode_bits: &[f64; 13],
        fctx: &crate::decode::FrameCtx,
    ) -> (Vec<Coeff>, u8, i32, f64, TxType) {
        let At {
            x, y, side, reach, ..
        } = at;
        let (mut above_buf, mut left_buf) = ([0u8; 2 * SUPERBLOCK], [0u8; 2 * SUPERBLOCK]);
        let (above, left, corner) =
            self.edges_into(x, y, side, reach, &mut above_buf, &mut left_buf);
        // Nothing ranks by the SAD pre-pass unless `top_k` prunes by it (or
        // the census reports it), so summing it under the default unpruned
        // search is `side * side` absolute differences per mode spent on a
        // number no one reads.
        let want_sad = search.top_k.is_some() || census_on();
        let mut scored: Vec<(f64, u8, Vec<u8>)> = search
            .modes
            .iter()
            .map(|&mode| {
                let mut prediction = vec![0u8; side * side];
                intra_predict_u8(
                    mode,
                    0,
                    above,
                    left,
                    corner,
                    side,
                    side,
                    false,
                    false,
                    &mut prediction,
                    fctx,
                );
                let sad = if want_sad {
                    self.block_sad(x, y, side, &prediction)
                } else {
                    0.0
                };
                (
                    sad + search.lambda * mode_bits[usize::from(mode)],
                    mode,
                    prediction,
                )
            })
            .collect();
        if let Some(k) = search.top_k
            && k < scored.len() {
                let dc_pos = scored.iter().position(|&(_, mode, _)| mode == DC_PRED);
                let dc_entry = dc_pos.map(|i| scored.remove(i));
                scored.sort_by(|a, b| a.0.partial_cmp(&b.0).expect("scores are finite"));
                scored.truncate(if dc_entry.is_some() {
                    k.saturating_sub(1)
                } else {
                    k
                });
                if let Some(dc_entry) = dc_entry {
                    scored.push(dc_entry);
                }
            }
        let ranking: Vec<(f64, u8)> = if census_on() {
            scored
                .iter()
                .map(|&(score, mode, _)| (score, mode))
                .collect()
        } else {
            Vec::new()
        };
        census_add(0, 1);
        census_add(1, scored.len());
        let mut best: Option<(f64, u8, Trial)> = None;
        for (_, mode, prediction) in scored {
            let trial = self.code_from_prediction(
                x,
                y,
                side,
                &prediction,
                false,
                search.base_q_idx,
                search.deadzone,
                at.set,
            );
            let cost = trial.sse + search.lambda * (trial.bits + mode_bits[usize::from(mode)]);
            if best
                .as_ref()
                .is_none_or(|(best_cost, _, _)| cost < *best_cost)
            {
                best = Some((cost, mode, trial));
            }
        }
        let (mut cost, mode, mut trial) = best.expect("the search offers at least one mode");
        // `angle_delta_y` (spec `read_intra_angle_info`): the eight
        // directional modes each carry a -3..=3 delta, three degrees a step,
        // which the writer has always coded as ZERO because nothing searched
        // it. Refined on the winning mode only -- libaom's own ordering, and
        // six extra trials on the directional blocks instead of on every
        // block times every mode. The edges are the ones the base angle
        // already read (`enable_intra_edge_filter` is off in this encoder's
        // sequence header, so no delta changes which samples exist), and the
        // predictor is the decoder's, `angle_delta` argument and all.
        let mut angle_delta = 0i32;
        // Not on a screen frame: measured on the 5-row native gate, the
        // refinement is worth -0.6/-0.6 on the 2160p film row and flat on the
        // 1080p one, but +0.7/+0.3 the WRONG way on the screen capture --
        // synthetic edges land on the base angles, so every delta there buys
        // a symbol and no prediction. Same `search.screen` gate palette and
        // intrabc already stand behind.
        if angle_delta_on() && !search.screen && (V_PRED..=D67_PRED).contains(&mode) {
            let row = &cdf::ANGLE_DELTA[usize::from(mode - V_PRED)];
            let zero_bits = symbol_bits(row, crate::tile::ANGLE_DELTA_ZERO);
            let mut prediction = vec![0u8; side * side];
            for delta in [-3i32, -2, -1, 1, 2, 3] {
                intra_predict_u8(
                    mode,
                    delta,
                    above,
                    left,
                    corner,
                    side,
                    side,
                    false,
                    false,
                    &mut prediction,
                    fctx,
                );
                let candidate = self.code_from_prediction(
                    x,
                    y,
                    side,
                    &prediction,
                    false,
                    search.base_q_idx,
                    search.deadzone,
                    at.set,
                );
                let bits =
                    symbol_bits(row, (crate::tile::ANGLE_DELTA_ZERO as i32 + delta) as usize)
                        - zero_bits;
                let cost_d = candidate.sse
                    + search.lambda * (candidate.bits + mode_bits[usize::from(mode)] + bits);
                if cost_d < cost {
                    cost = cost_d;
                    angle_delta = delta;
                    trial = candidate;
                }
            }
            ANGLE_DELTA_HITS[(crate::tile::ANGLE_DELTA_ZERO as i32 + angle_delta) as usize]
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        // lane-txset: the block's own transform type, searched on the mode
        // and angle the trials above settled -- the same ordering the angle
        // refinement uses, and the same RD terms.
        let mut tx_type = TxType::DctDct;
        for &candidate_type in tx_type_candidates(at.set, search.screen).iter().skip(1) {
            let candidate = self.trial_typed(
                at,
                mode,
                angle_delta,
                search.base_q_idx,
                search.deadzone,
                candidate_type,
                fctx,
            );
            let cost_t =
                candidate.sse + search.lambda * (candidate.bits + mode_bits[usize::from(mode)]);
            // lane-txi's surcharge, as in `code_tx_depth`: the incumbent is
            // `DCT_DCT` and pays none of it, so the returned `cost` stays the
            // block's true one for the caller's own decision.
            if cost_t + non_dct_surcharge(search.lambda, candidate_type, candidate.bits) < cost {
                cost = cost_t;
                tx_type = candidate_type;
                trial = candidate;
            }
        }
        TX_TYPE_HITS[tx_type as usize].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if census_on() {
            LUMA_RANK[sad_rank(&ranking, mode).min(12)]
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        self.commit(x, y, side, &trial);
        // A 64-point transform codes only its top-left 32x32 corner, which is
        // the grid `code_from_prediction` hands back (lane-i64).
        (
            coeffs(&trial.levels, side.min(BLOCK)),
            mode,
            angle_delta,
            cost,
            tx_type,
        )
    }
}

/// Where one block sits in its plane, and how far past its own edges its
/// prediction may read.
#[derive(Clone, Copy)]
pub(super) struct At {
    pub(super) x: usize,
    pub(super) y: usize,
    pub(super) side: usize,
    pub(super) reach: Reach,
    /// Which of the tile writer's coefficient tables this block's levels are
    /// coded with, and so which the search prices them through.
    pub(super) set: TxbSet,
}

/// Which of the samples past a block's own edges the decoder has decoded by
/// the time it predicts the block, and so which a directional mode may read.
///
/// The decoder derives these from its `BlockDecoded` flags, which are cleared
/// per superblock (`clear_block_decoded_flags`, spec 7.4). Rather than carry
/// that map, this reads the answer the same way libaom and rav1e do: for a
/// block whose transform covers it whole, whether the samples above its right
/// (or below its left) are decoded depends only on where the block sits inside
/// its 64x64 superblock, which is a pinned table per block size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Reach {
    pub(crate) above_right: bool,
    pub(crate) below_left: bool,
}

/// `has_tr_16x16` / `has_tr_32x32` (`recon_intra.rs` in rav1e, `has_tr_*` in
/// libaom): for a block that is neither in the superblock's top row nor its
/// rightmost column, whether the block above and to its right is coded before
/// it. Indexed by the block's position in the superblock, in blocks of its own
/// size, as `row * (128 / side) + col` (`Reach::table_stride`) -- libaom's
/// 128, not this crate's 64 superblock -- bit by bit from the low end.
pub(super) const HAS_TOP_RIGHT: [&[u8]; 4] = [
    &[255, 85, 119, 85, 127, 85, 119, 85],
    &[95, 87],
    // has_tr_8x8 (libaom av1/common/reconintra.c) -- table_stride(8) == 16
    // (128 / 8), so this row is 16 wide by 16 rows, 32 bytes.
    &[
        255, 255, 85, 85, 119, 119, 85, 85, 127, 127, 85, 85, 119, 119, 85, 85, 255, 127, 85, 85,
        119, 119, 85, 85, 127, 127, 85, 85, 119, 119, 85, 85,
    ],
    // has_tr_4x4 (libaom av1/common/reconintra.c, transcribed verbatim) --
    // table_stride(4) == 32 (128 / 4), so 32 bits per row, 32 rows, 128
    // bytes. Sub-8x8 leaves (lane-sub8) are the only blocks that reach here
    // with side 4 outside a TX4 split; without their own row they clamped
    // into the 8x8 table and read above-right samples the decoder has not
    // written yet (all-zero pixels in the bottom-right triangle of a
    // directional 4x4 leaf).
    &[
        255, 255, 255, 255, 85, 85, 85, 85, 119, 119, 119, 119, 85, 85, 85, 85, 127, 127, 127, 127,
        85, 85, 85, 85, 119, 119, 119, 119, 85, 85, 85, 85, 255, 127, 255, 127, 85, 85, 85, 85,
        119, 119, 119, 119, 85, 85, 85, 85, 127, 127, 127, 127, 85, 85, 85, 85, 119, 119, 119, 119,
        85, 85, 85, 85, 255, 255, 255, 127, 85, 85, 85, 85, 119, 119, 119, 119, 85, 85, 85, 85,
        127, 127, 127, 127, 85, 85, 85, 85, 119, 119, 119, 119, 85, 85, 85, 85, 255, 127, 255, 127,
        85, 85, 85, 85, 119, 119, 119, 119, 85, 85, 85, 85, 127, 127, 127, 127, 85, 85, 85, 85,
        119, 119, 119, 119, 85, 85, 85, 85,
    ],
];

/// `has_bl_16x16` / `has_bl_32x32`: the same, for the block below and to the
/// left.
pub(super) const HAS_BOTTOM_LEFT: [&[u8]; 4] = [
    &[84, 16, 84, 0, 84, 16, 84, 0],
    &[4, 4],
    // has_bl_8x8 (libaom av1/common/reconintra.c)
    &[
        84, 85, 16, 17, 84, 85, 0, 1, 84, 85, 16, 17, 84, 85, 0, 0, 84, 85, 16, 17, 84, 85, 0, 1,
        84, 85, 16, 17, 84, 85, 0, 0,
    ],
    // has_bl_4x4 (same source, same order).
    &[
        84, 85, 85, 85, 16, 17, 17, 17, 84, 85, 85, 85, 0, 1, 1, 1, 84, 85, 85, 85, 16, 17, 17, 17,
        84, 85, 85, 85, 0, 0, 1, 0, 84, 85, 85, 85, 16, 17, 17, 17, 84, 85, 85, 85, 0, 1, 1, 1, 84,
        85, 85, 85, 16, 17, 17, 17, 84, 85, 85, 85, 0, 0, 0, 0, 84, 85, 85, 85, 16, 17, 17, 17, 84,
        85, 85, 85, 0, 1, 1, 1, 84, 85, 85, 85, 16, 17, 17, 17, 84, 85, 85, 85, 0, 0, 1, 0, 84, 85,
        85, 85, 16, 17, 17, 17, 84, 85, 85, 85, 0, 1, 1, 1, 84, 85, 85, 85, 16, 17, 17, 17, 84, 85,
        85, 85, 0, 0, 0, 0,
    ],
];

/// `has_tr_vert_*` (libaom `reconintra.c`, `has_tr_vert_tables`), indexed by
/// [`Reach::table`] exactly like [`HAS_TOP_RIGHT`]: inside a
/// `PARTITION_VERT_A`/`_B` the square sub-blocks are visited TL, BL, TR, BR
/// rather than in raster order, so the bottom-left square's top-right
/// neighbour is the partition's own right-hand rectangle -- not yet decoded.
/// libaom switches tables on the partition type (`get_has_tr_table`); the
/// ordinary table says 1 where this one says 0 (32x32 at row 1 col 0:
/// `95` bit 4 = 1 vs `15` bit 4 = 0), which is exactly the case
/// lane-part32 r5 caught reading undecoded samples.
pub(super) const HAS_TOP_RIGHT_VERT: [&[u8]; 3] = [
    // has_tr_vert_16x16
    &[255, 0, 119, 0, 127, 0, 119, 0],
    // has_tr_vert_32x32
    &[15, 7],
    // has_tr_vert_8x8
    &[
        255, 255, 0, 0, 119, 119, 0, 0, 127, 127, 0, 0, 119, 119, 0, 0, 255, 127, 0, 0, 119, 119,
        0, 0, 127, 127, 0, 0, 119, 119, 0, 0,
    ],
];

/// [`HAS_TOP_RIGHT_VERT`]'s sibling, `has_bl_vert_tables`.
pub(super) const HAS_BOTTOM_LEFT_VERT: [&[u8]; 3] = [
    // has_bl_vert_16x16
    &[254, 16, 254, 0, 254, 16, 254, 0],
    // has_bl_vert_32x32
    &[14, 14],
    // has_bl_vert_8x8
    &[
        254, 255, 16, 17, 254, 255, 0, 1, 254, 255, 16, 17, 254, 255, 0, 0, 254, 255, 16, 17, 254,
        255, 0, 1, 254, 255, 16, 17, 254, 255, 0, 0,
    ],
];

thread_local! {
    /// Set for the duration of a `PARTITION_VERT_A`/`_B`'s own sub-blocks
    /// (see [`Reach::vert_ab_partition`]) -- libaom carries the partition
    /// type down into `has_top_right`/`has_bottom_left` as an argument, and
    /// threading one bool through this crate's already twenty-argument
    /// `decode_block` would touch every call site instead of the two arms
    /// that need it.
    static VERT_AB_PARTITION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Restores [`VERT_AB_PARTITION`] to what it was when the guard was made.
pub(crate) struct VertAbGuard(bool);

impl Drop for VertAbGuard {
    fn drop(&mut self) {
        VERT_AB_PARTITION.with(|c| c.set(self.0));
    }
}

/// The `has_tr_*` / `has_bl_*` tables for every rectangular shape this
/// decoder codes (libaom `reconintra.c`, generated from the oracle's source
/// so no byte is retyped). Each is indexed by
/// `(blk_row_in_sb << (5 - log2(bw / 4))) + blk_col_in_sb`, i.e. row stride
/// `128 / bw` bits ([`Reach::table_stride`]) on libaom's 128-pixel
/// superblock grid; on this crate's 64-pixel superblock the highest index a
/// shape can reach stays inside its own table, so no wrap is involved.
///
/// lane-tx4x8 r3: the previous two-row `HAS_TOP_RIGHT_RECT`/
/// `HAS_BOTTOM_LEFT_RECT` pair held 32x16/16x32 only and a `_` arm routed
/// every other shape -- 16x8 strips among them -- to the 32x16 row, wrong at
/// 10 of the 21 reachable superblock positions for above-right and 7 for
/// below-left.
/// `has_tr_4x8` (`reconintra.c`), transcribed verbatim: 64 bytes, row stride 32 bits (`128 / 4`).
pub(super) const HAS_TR_4X8: [u8; 64] = [
    255, 255, 255, 255, 119, 119, 119, 119, 127, 127, 127, 127, 119, 119, 119, 119, 255, 127, 255,
    127, 119, 119, 119, 119, 127, 127, 127, 127, 119, 119, 119, 119, 255, 255, 255, 127, 119, 119,
    119, 119, 127, 127, 127, 127, 119, 119, 119, 119, 255, 127, 255, 127, 119, 119, 119, 119, 127,
    127, 127, 127, 119, 119, 119, 119,
];
/// `has_bl_4x8` (`reconintra.c`), transcribed verbatim: 64 bytes, row stride 32 bits (`128 / 4`).
pub(super) const HAS_BL_4X8: [u8; 64] = [
    16, 17, 17, 17, 0, 1, 1, 1, 16, 17, 17, 17, 0, 0, 1, 0, 16, 17, 17, 17, 0, 1, 1, 1, 16, 17, 17,
    17, 0, 0, 0, 0, 16, 17, 17, 17, 0, 1, 1, 1, 16, 17, 17, 17, 0, 0, 1, 0, 16, 17, 17, 17, 0, 1,
    1, 1, 16, 17, 17, 17, 0, 0, 0, 0,
];
/// `has_tr_8x4` (`reconintra.c`), transcribed verbatim: 64 bytes, row stride 16 bits (`128 / 8`).
pub(super) const HAS_TR_8X4: [u8; 64] = [
    255, 255, 0, 0, 85, 85, 0, 0, 119, 119, 0, 0, 85, 85, 0, 0, 127, 127, 0, 0, 85, 85, 0, 0, 119,
    119, 0, 0, 85, 85, 0, 0, 255, 127, 0, 0, 85, 85, 0, 0, 119, 119, 0, 0, 85, 85, 0, 0, 127, 127,
    0, 0, 85, 85, 0, 0, 119, 119, 0, 0, 85, 85, 0, 0,
];
/// `has_bl_8x4` (`reconintra.c`), transcribed verbatim: 64 bytes, row stride 16 bits (`128 / 8`).
pub(super) const HAS_BL_8X4: [u8; 64] = [
    254, 255, 84, 85, 254, 255, 16, 17, 254, 255, 84, 85, 254, 255, 0, 1, 254, 255, 84, 85, 254,
    255, 16, 17, 254, 255, 84, 85, 254, 255, 0, 0, 254, 255, 84, 85, 254, 255, 16, 17, 254, 255,
    84, 85, 254, 255, 0, 1, 254, 255, 84, 85, 254, 255, 16, 17, 254, 255, 84, 85, 254, 255, 0, 0,
];
/// `has_tr_8x16` (`reconintra.c`), transcribed verbatim: 16 bytes, row stride 16 bits (`128 / 8`).
pub(super) const HAS_TR_8X16: [u8; 16] = [
    255, 255, 119, 119, 127, 127, 119, 119, 255, 127, 119, 119, 127, 127, 119, 119,
];
/// `has_bl_8x16` (`reconintra.c`), transcribed verbatim: 16 bytes, row stride 16 bits (`128 / 8`).
pub(super) const HAS_BL_8X16: [u8; 16] = [16, 17, 0, 1, 16, 17, 0, 0, 16, 17, 0, 1, 16, 17, 0, 0];
/// `has_tr_16x8` (`reconintra.c`), transcribed verbatim: 16 bytes, row stride 8 bits (`128 / 16`).
pub(super) const HAS_TR_16X8: [u8; 16] = [255, 0, 85, 0, 119, 0, 85, 0, 127, 0, 85, 0, 119, 0, 85, 0];
/// `has_bl_16x8` (`reconintra.c`), transcribed verbatim: 16 bytes, row stride 8 bits (`128 / 16`).
pub(super) const HAS_BL_16X8: [u8; 16] = [
    254, 84, 254, 16, 254, 84, 254, 0, 254, 84, 254, 16, 254, 84, 254, 0,
];
/// `has_tr_16x32` (`reconintra.c`), transcribed verbatim: 4 bytes, row stride 8 bits (`128 / 16`).
pub(super) const HAS_TR_16X32: [u8; 4] = [255, 119, 127, 119];
/// `has_bl_16x32` (`reconintra.c`), transcribed verbatim: 4 bytes, row stride 8 bits (`128 / 16`).
pub(super) const HAS_BL_16X32: [u8; 4] = [16, 0, 16, 0];
/// `has_tr_32x16` (`reconintra.c`), transcribed verbatim: 4 bytes, row stride 4 bits (`128 / 32`).
pub(super) const HAS_TR_32X16: [u8; 4] = [15, 5, 7, 5];
/// `has_bl_32x16` (`reconintra.c`), transcribed verbatim: 4 bytes, row stride 4 bits (`128 / 32`).
pub(super) const HAS_BL_32X16: [u8; 4] = [78, 14, 78, 14];
/// `has_tr_32x64` (`reconintra.c`), transcribed verbatim: 1 bytes, row stride 4 bits (`128 / 32`).
pub(super) const HAS_TR_32X64: [u8; 1] = [127];
/// `has_bl_32x64` (`reconintra.c`), transcribed verbatim: 1 bytes, row stride 4 bits (`128 / 32`).
pub(super) const HAS_BL_32X64: [u8; 1] = [0];
/// `has_tr_64x32` (`reconintra.c`), transcribed verbatim: 1 bytes, row stride 2 bits (`128 / 64`).
pub(super) const HAS_TR_64X32: [u8; 1] = [19];
/// `has_bl_64x32` (`reconintra.c`), transcribed verbatim: 1 bytes, row stride 2 bits (`128 / 64`).
pub(super) const HAS_BL_64X32: [u8; 1] = [34];
/// `has_tr_4x16` (`reconintra.c`), transcribed verbatim: 32 bytes, row stride 32 bits (`128 / 4`).
pub(super) const HAS_TR_4X16: [u8; 32] = [
    255, 255, 255, 255, 127, 127, 127, 127, 255, 127, 255, 127, 127, 127, 127, 127, 255, 255, 255,
    127, 127, 127, 127, 127, 255, 127, 255, 127, 127, 127, 127, 127,
];
/// `has_bl_4x16` (`reconintra.c`), transcribed verbatim: 32 bytes, row stride 32 bits (`128 / 4`).
pub(super) const HAS_BL_4X16: [u8; 32] = [
    0, 1, 1, 1, 0, 0, 1, 0, 0, 1, 1, 1, 0, 0, 0, 0, 0, 1, 1, 1, 0, 0, 1, 0, 0, 1, 1, 1, 0, 0, 0, 0,
];
/// `has_tr_16x4` (`reconintra.c`), transcribed verbatim: 32 bytes, row stride 8 bits (`128 / 16`).
pub(super) const HAS_TR_16X4: [u8; 32] = [
    255, 0, 0, 0, 85, 0, 0, 0, 119, 0, 0, 0, 85, 0, 0, 0, 127, 0, 0, 0, 85, 0, 0, 0, 119, 0, 0, 0,
    85, 0, 0, 0,
];
/// `has_bl_16x4` (`reconintra.c`), transcribed verbatim: 32 bytes, row stride 8 bits (`128 / 16`).
pub(super) const HAS_BL_16X4: [u8; 32] = [
    254, 254, 254, 84, 254, 254, 254, 16, 254, 254, 254, 84, 254, 254, 254, 0, 254, 254, 254, 84,
    254, 254, 254, 16, 254, 254, 254, 84, 254, 254, 254, 0,
];
/// `has_tr_8x32` (`reconintra.c`), transcribed verbatim: 8 bytes, row stride 16 bits (`128 / 8`).
pub(super) const HAS_TR_8X32: [u8; 8] = [255, 255, 127, 127, 255, 127, 127, 127];
/// `has_bl_8x32` (`reconintra.c`), transcribed verbatim: 8 bytes, row stride 16 bits (`128 / 8`).
pub(super) const HAS_BL_8X32: [u8; 8] = [0, 1, 0, 0, 0, 1, 0, 0];
/// `has_tr_32x8` (`reconintra.c`), transcribed verbatim: 8 bytes, row stride 4 bits (`128 / 32`).
pub(super) const HAS_TR_32X8: [u8; 8] = [15, 0, 5, 0, 7, 0, 5, 0];
/// `has_bl_32x8` (`reconintra.c`), transcribed verbatim: 8 bytes, row stride 4 bits (`128 / 32`).
pub(super) const HAS_BL_32X8: [u8; 8] = [238, 78, 238, 14, 238, 78, 238, 14];
/// `has_tr_16x64` (`reconintra.c`), transcribed verbatim: 2 bytes, row stride 8 bits (`128 / 16`).
pub(super) const HAS_TR_16X64: [u8; 2] = [255, 127];
/// `has_bl_16x64` (`reconintra.c`), transcribed verbatim: 2 bytes, row stride 8 bits (`128 / 16`).
pub(super) const HAS_BL_16X64: [u8; 2] = [0, 0];
/// `has_tr_64x16` (`reconintra.c`), transcribed verbatim: 2 bytes, row stride 2 bits (`128 / 64`).
pub(super) const HAS_TR_64X16: [u8; 2] = [3, 1];
/// `has_bl_64x16` (`reconintra.c`), transcribed verbatim: 2 bytes, row stride 2 bits (`128 / 64`).
pub(super) const HAS_BL_64X16: [u8; 2] = [42, 42];

/// The `has_tr_*`/`has_bl_*` row for one rectangular block shape.
pub(super) fn rect_reach_tables(bw: usize, bh: usize) -> (&'static [u8], &'static [u8]) {
    match (bw, bh) {
        (4, 8) => (&HAS_TR_4X8, &HAS_BL_4X8),
        (8, 4) => (&HAS_TR_8X4, &HAS_BL_8X4),
        (8, 16) => (&HAS_TR_8X16, &HAS_BL_8X16),
        (16, 8) => (&HAS_TR_16X8, &HAS_BL_16X8),
        (16, 32) => (&HAS_TR_16X32, &HAS_BL_16X32),
        (32, 16) => (&HAS_TR_32X16, &HAS_BL_32X16),
        (32, 64) => (&HAS_TR_32X64, &HAS_BL_32X64),
        (64, 32) => (&HAS_TR_64X32, &HAS_BL_64X32),
        (4, 16) => (&HAS_TR_4X16, &HAS_BL_4X16),
        (16, 4) => (&HAS_TR_16X4, &HAS_BL_16X4),
        (8, 32) => (&HAS_TR_8X32, &HAS_BL_8X32),
        (32, 8) => (&HAS_TR_32X8, &HAS_BL_32X8),
        (16, 64) => (&HAS_TR_16X64, &HAS_BL_16X64),
        (64, 16) => (&HAS_TR_64X16, &HAS_BL_64X16),
        _ => unreachable!("no libaom has_tr/has_bl table for a {bw}x{bh} block"),
    }
}

impl Reach {
    /// `has_top_right`/`has_bottom_left` (libaom `reconintra.c`) for a
    /// transform unit at `(col_off, row_off)` INSIDE a `bw`x`bh` block whose
    /// own answer is `block` -- a different rule from the standalone-block
    /// tables, because a unit's neighbours are mostly its own block's
    /// already-reconstructed units:
    ///  * top-right: available while the unit to the right is still inside
    ///    the block (`col_off + tx < bw`); otherwise only the block's top row
    ///    of units reaches past it, through the block-level answer
    ///    (libaom's `row_off > 0` branch returns exactly
    ///    `col_off + txw < plane_bw`).
    ///  * bottom-left: a unit outside the block's left column has none
    ///    (libaom returns 0 for `col_off > 0`); inside the block
    ///    (`row_off + tx < bh`) they are the left unit's, already
    ///    reconstructed; the bottom row falls back to the block-level answer.
    pub(crate) fn of_tu(
        bw: usize,
        bh: usize,
        col_off: usize,
        row_off: usize,
        // The unit's own width and height in pixels; equal for a square
        // transform, different for the rect unit a 1:4 strip's first split
        // produces (lane-rectsplitx r1).
        tx_w: usize,
        tx_h: usize,
        block: Self,
    ) -> Self {
        // A block wider than 64 is coded in 64x64 "mu" chunks in raster
        // order, so both libaom predicates special-case it (lane-sb128c r10);
        // without this the last unit column of every chunk of a 128x64 block
        // predicts from a chunk libaom has not decoded yet.
        let wide = bw > 64;
        Self {
            above_right: if row_off > 0 {
                // `has_top_right`'s `row_off > 0` branch: the offset is taken
                // modulo the 64-wide chunk, except for the one unit whose
                // top-right corner is the centre of a 128x128 block.
                if wide {
                    (row_off == 64 && col_off + tx_w == 64) || (col_off % 64) + tx_w < 64
                } else {
                    col_off + tx_w < bw
                }
            } else if col_off + tx_w < bw {
                true
            } else {
                block.above_right
            },
            below_left: if wide && col_off > 0 && col_off.is_multiple_of(64) {
                // `has_bottom_left`: at the left edge of a right-hand chunk
                // the bottom-left pixels are in the already-coded left chunk,
                // provided they stay inside it.
                (row_off % 64) + tx_h < bh.min(64)
            } else if col_off > 0 {
                false
            } else if row_off + tx_h < bh {
                true
            } else {
                block.below_left
            },
        }
    }

    /// libaom's own `has_top_right`/`has_bottom_left` for one transform unit
    /// of a CHROMA plane, at the plane granularity libaom uses there
    /// (`reconintra.c:203` / `reconintra.c:381`).
    ///
    /// Two things make a chroma unit's answer differ from the same unit's
    /// LUMA answer, and [`Self::of_tu`] has neither:
    ///
    /// 1. libaom runs a chroma plane through `scale_chroma_bsize(bsize,
    ///    ss_x, ss_y)` (`reconintra.c:1823`, definition `:1649`) before
    ///    either call, and that remap is not the identity. At 4:2:2
    ///    (`ss (1,0)`) a BLOCK_4X4 luma leaf becomes BLOCK_8X4 -- TWO mi
    ///    wide and one mi tall.
    /// 2. `has_bottom_left`'s "leftmost column of superblock" arm
    ///    (`reconintra.c:429-435`) is then reached at `blk_col_in_sb == 0`,
    ///    which the one-mi-wide luma BLOCK_4X4 reads as column 1 and the
    ///    per-block table denies. [`Self::of_tu`] has no such arm at all: it
    ///    ends at `block.below_left`, the BLOCK-level answer, which stands in
    ///    for the table plus both superblock arms and so agrees with libaom
    ///    only where neither superblock arm is the deciding step.
    ///
    /// `mi_bw`/`mi_bh` are the PLANE-scaled block's mi extent ([`Self`]'s
    /// caller supplies it from [`chroma_mi_shape`], never the luma block's),
    /// and `col_off`/`row_off`/`tx_w`/`tx_h` are LUMA pixels as at every other
    /// `Reach` call site -- libaom's plane units are one `tx_size_*_unit`
    /// (`1 << (2 + ss)` luma pixels) wide, and each comparison below is
    /// restated in luma pixels rather than rescaled twice. The
    /// 128-wide arm libaom guards on `block_size_wide[bsize] > 64` is not
    /// transcribed: every caller keeps such blocks on [`Self::of_tu`].
    ///
    /// `above_avail` is libaom's `top_available && right_available` and
    /// `below_avail` its `bottom_available && left_available` -- the early
    /// returns at `reconintra.c:204` / `:383`. The caller states them because
    /// libaom's two edge tests are TILE bounds over the TRANSFORM's plane
    /// extent (`mi_col + ((col_off + txw) << ss_x) < mi_col_end`,
    /// `mi_row + ((row_off + txh) << ss_y) < mi_row_end`,
    /// `reconintra.c:1815-1817`) while `top_available`/`left_available` are
    /// `y0 > 0` / `x0 > 0`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn of_tu_chroma(
        mi_bw: usize,
        mi_bh: usize,
        mi_row: usize,
        mi_col: usize,
        sb_mi: usize,
        col_off: usize,
        row_off: usize,
        tx_w: usize,
        tx_h: usize,
        ss_x: usize,
        ss_y: usize,
        above_avail: bool,
        below_avail: bool,
    ) -> Self {
        if !above_avail && !below_avail {
            return Self::none();
        }
        let bw_log2 = mi_bw.trailing_zeros() as usize;
        let bh_log2 = mi_bh.trailing_zeros() as usize;
        let blk_row = (mi_row & (sb_mi - 1)) >> bh_log2;
        let blk_col = (mi_col & (sb_mi - 1)) >> bw_log2;
        // `plane_bw_unit`/`plane_bh_unit` (`reconintra.c:208` / `:414`) in
        // libaom's plane units, restated in the luma pixels of the offsets.
        let plane_bw = (mi_bw >> ss_x).max(1) << (2 + ss_x);
        let plane_bh = (mi_bh >> ss_y).max(1) << (2 + ss_y);
        let (tr, bl) = Self::plane_reach_tables(mi_bw, mi_bh);
        Self {
            above_right: above_avail
                && (if row_off > 0 {
                    col_off + tx_w < plane_bw
                } else if col_off + tx_w < plane_bw {
                    true
                } else if blk_row == 0 {
                    // Top row of superblock: the top-right pixels are in the
                    // superblock above.
                    true
                } else if ((blk_col + 1) << bw_log2) >= sb_mi {
                    // Rightmost column: they fall in the superblock to the
                    // right, which is not decoded yet.
                    false
                } else {
                    Self::bit(tr, (blk_row << (5 - bw_log2)) + blk_col)
                }),
            below_left: below_avail
                && (if col_off > 0 {
                    // Bottom-left pixels are in the bottom-left block.
                    false
                } else if row_off + tx_h < plane_bh {
                    true
                } else if blk_col == 0 {
                    // `reconintra.c:429-435`: `blk_start_row_off + row_off +
                    // count < sb_height_unit`, compared in plane units. Both
                    // `>> ss_y` floors are lifted out of the comparison
                    // instead of being evaluated, so the luma-pixel
                    // restatement is exact.
                    let from_block = (blk_row << bh_log2) >> ss_y;
                    let sb_height = (sb_mi >> ss_y).saturating_sub(from_block);
                    row_off + tx_h < sb_height << (2 + ss_y)
                } else if ((blk_row + 1) << bh_log2) >= sb_mi {
                    // Bottom row (and not the leftmost column): the
                    // superblock below is not decoded yet.
                    false
                } else {
                    Self::bit(bl, (blk_row << (5 - bw_log2)) + blk_col)
                }),
        }
    }

    /// The `has_tr_*`/`has_bl_*` pair for a block of `mi_bw` x `mi_bh` mi --
    /// libaom's `get_has_tr_table`/`get_has_bl_table` indexed by the
    /// PLANE-scaled `BLOCK_SIZE`, so one shape can be a rectangle where
    /// [`Self::table`] would read a square's row (a 4:2:2 chroma BLOCK_8X4,
    /// which is [`Self::of_tu_chroma`]'s whole reason for existing).
    pub(super) fn plane_reach_tables(mi_bw: usize, mi_bh: usize) -> (&'static [u8], &'static [u8]) {
        if mi_bw != mi_bh {
            return rect_reach_tables(mi_bw * 4, mi_bh * 4);
        }
        let row = Self::table(mi_bw * 4);
        if VERT_AB_PARTITION.with(std::cell::Cell::get) && row < HAS_TOP_RIGHT_VERT.len() {
            (HAS_TOP_RIGHT_VERT[row], HAS_BOTTOM_LEFT_VERT[row])
        } else {
            (HAS_TOP_RIGHT[row], HAS_BOTTOM_LEFT[row])
        }
    }

    /// What a block of `side` samples at `(x, y)` may read past its own edges,
    /// in a frame of `width` by `height`.
    pub(crate) fn of(
        side: usize,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        fctx: &crate::decode::FrameCtx,
    ) -> Self {
        Self {
            above_right: y > 0 && x + side < width && Self::top_right(side, x, y, fctx),
            below_left: x > 0 && y + side < height && Self::bottom_left(side, x, y, fctx),
        }
    }

    /// [`Self::of`] for a true `bw`x`bh` rectangular block (lane-intradisp
    /// r1) -- `libaom`'s `has_top_right`/`has_bottom_left` index by the
    /// block's own width for the row stride and by its own height for the
    /// row/column position (`reconintra.c`'s `bw_in_mi_log2`/`bh_in_mi_log2`),
    /// unlike the square [`Self::of`] which uses one `side` for everything.
    pub(crate) fn of_rect(
        bw: usize,
        bh: usize,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        fctx: &crate::decode::FrameCtx,
    ) -> Self {
        Self {
            above_right: y > 0 && x + bw < width && Self::top_right_rect(bw, bh, x, y, fctx),
            below_left: x > 0 && y + bh < height && Self::bottom_left_rect(bw, bh, x, y, fctx),
        }
    }

    /// [`Self::of`] for one transform unit of a square block whose transform
    /// is smaller than itself (libaom `has_top_right`/`has_bottom_left` with
    /// non-zero `row_off`/`col_off`): interior units answer from the block's
    /// own geometry, and only a unit on the block's top row (resp. left
    /// column) reaches the table -- which is the BLOCK's table read at the
    /// BLOCK's position, never a table row for the transform's size. Letting
    /// the unit stand in for a block of its own size agrees with libaom for
    /// the ordinary tables, but the `has_tr_vert_*`/`has_bl_vert_*` pair has
    /// no 4x4 row at all (`has_tr_vert_tables[BLOCK_4X4]` is NULL), so a TX4
    /// unit inside an 8x8 square of a `PARTITION_VERT_A`/`_B` panicked on a
    /// real `--enable-tx-size-search=1` stream (lane-ab16 r2).
    /// libaom `has_top_right` at block granularity (`row_off`/`col_off` 0,
    /// the transform covering the whole block, luma): everything before the
    /// table lookup is that function's own early-exit ladder.
    pub(super) fn top_right_rect(
        bw: usize,
        bh: usize,
        x: usize,
        y: usize,
        fctx: &crate::decode::FrameCtx,
    ) -> bool {
        let sb_mi: usize = reach_sb_px(fctx) / 4;
        let (mi_row, mi_col) = (y / 4, x / 4);
        let bw_log2 = (bw / 4).trailing_zeros() as usize;
        let bh_log2 = (bh / 4).trailing_zeros() as usize;
        let blk_row = (mi_row & (sb_mi - 1)) >> bh_log2;
        let blk_col = (mi_col & (sb_mi - 1)) >> bw_log2;
        // Top row of the superblock: the top-right pixels are in the (already
        // decoded) superblock above.
        if blk_row == 0 {
            return true;
        }
        // Rightmost column: they fall in the superblock to the right.
        if ((blk_col + 1) << bw_log2) >= sb_mi {
            return false;
        }
        let table = rect_reach_tables(bw, bh).0;
        Self::bit(table, Self::rect_table_index(bw_log2, blk_row, blk_col))
    }

    /// libaom `has_top_right`'s own index arithmetic:
    /// `(blk_row << (MAX_MIB_SIZE_LOG2 - bw_in_mi_log2)) + blk_col`, and
    /// `MAX_MIB_SIZE_LOG2` is 5 (`enums.h`: libaom's tables are laid out for a
    /// 128x128 superblock = 32 mi), NOT this crate's 64x64 = 16. lane-rectx r5:
    /// r4 wrote `4 - bw_log2`, halving the row stride -- every table row but
    /// the first then read the wrong byte (16x8 wrong at 80 mi positions in a
    /// 64-wide SB, 8x16 at 16, 32x16 at 64, a regression against main).
    /// [`tests::rect_reach_tables_are_indexed_with_a_32_mi_row_stride`] pins it.
    pub(super) fn rect_table_index(bw_log2: usize, blk_row: usize, blk_col: usize) -> usize {
        (blk_row << (5 - bw_log2)) + blk_col
    }

    /// libaom `has_bottom_left`, same granularity as [`Self::top_right_rect`].
    pub(super) fn bottom_left_rect(
        bw: usize,
        bh: usize,
        x: usize,
        y: usize,
        fctx: &crate::decode::FrameCtx,
    ) -> bool {
        let sb_mi: usize = reach_sb_px(fctx) / 4;
        let (mi_row, mi_col) = (y / 4, x / 4);
        let bw_log2 = (bw / 4).trailing_zeros() as usize;
        let bh_log2 = (bh / 4).trailing_zeros() as usize;
        let blk_row = (mi_row & (sb_mi - 1)) >> bh_log2;
        let blk_col = (mi_col & (sb_mi - 1)) >> bw_log2;
        // Leftmost column of the superblock: the bottom-left pixels are in
        // the left superblock iff they stay inside its height.
        if blk_col == 0 {
            return (blk_row << bh_log2) + (bh / 4) < sb_mi;
        }
        // Bottom row (and not the leftmost column): they fall in the
        // superblock below, which is not decoded yet.
        if ((blk_row + 1) << bh_log2) >= sb_mi {
            return false;
        }
        let table = rect_reach_tables(bw, bh).1;
        Self::bit(table, Self::rect_table_index(bw_log2, blk_row, blk_col))
    }

    /// Marks every [`Reach::of`] made until the returned guard drops as being
    /// inside a `PARTITION_VERT_A`/`_B` (libaom's `get_has_tr_table` /
    /// `get_has_bl_table` partition argument). Rectangular sub-blocks are
    /// unaffected: libaom's own comment says vertical rectangles keep their
    /// non-vert table, which is what [`Reach::of_rect`] already reads.
    /// Whether a `PARTITION_VERT_A`/`_B`'s sub-blocks are being decoded
    /// right now ([`Self::vert_ab_partition`]'s guard is alive), so a caller
    /// can count the sub-block work that partition creates (lane-ab16).
    pub(crate) fn in_vert_ab() -> bool {
        VERT_AB_PARTITION.with(std::cell::Cell::get)
    }

    pub(crate) fn vert_ab_partition() -> VertAbGuard {
        VertAbGuard(VERT_AB_PARTITION.with(|c| c.replace(true)))
    }

    /// Neither, which is all a mode that reads no further than its own edges
    /// needs.
    pub(crate) fn none() -> Self {
        Self {
            above_right: false,
            below_left: false,
        }
    }

    /// `has_top_right` for a block whose transform covers it whole: the top
    /// row of a superblock reads into the superblock above, which is decoded;
    /// the rightmost column would read into the superblock to the right, which
    /// is not; and everything between is the table's to answer.
    pub(super) fn top_right(side: usize, x: usize, y: usize, fctx: &crate::decode::FrameCtx) -> bool {
        let (row, col, per_side) = Self::position(side, x, y, fctx);
        if row == 0 {
            return true;
        }
        if col + 1 == per_side {
            return false;
        }
        // lane-sub8 r4: side 4 now has its own `has_tr_4x4`/`has_bl_4x4` row
        // (the clamp corner-cut that used to live here is gone); sides
        // 16/32/64 share row 0 exactly as libaom's tables repeat.
        // corner-cut: libaom transcribes no vert 4x4 row, so a side-4 block
        // inside a `PARTITION_VERT_A`/`_B` falls back to the ordinary table
        // -- a reach over-estimate (extra above-right reference pixels),
        // never a stream desync. Ceiling: transcribe `has_tr_vert_4x4` /
        // `has_bl_vert_4x4` and extend the VERT tables to four rows.
        // NB: named `table_row`, not `row` -- `row` above is the block's
        // position inside the superblock and is what the bit index below
        // steps by (a merge that shadowed it desynced every block size).
        let table_row = Self::table(side);
        let table = if VERT_AB_PARTITION.with(std::cell::Cell::get)
            && table_row < HAS_TOP_RIGHT_VERT.len()
        {
            HAS_TOP_RIGHT_VERT[table_row]
        } else {
            HAS_TOP_RIGHT[table_row]
        };
        Self::bit(
            table,
            (row * Self::table_stride(side) + col) % (table.len() * 8),
        )
    }

    /// `has_bottom_left` for such a block: the leftmost column of a superblock
    /// reads into the superblock to its left, which is decoded only as far
    /// down as its own bottom; the bottom row would read into the superblock
    /// below, which is not decoded at all; and the table answers the rest.
    pub(super) fn bottom_left(side: usize, x: usize, y: usize, fctx: &crate::decode::FrameCtx) -> bool {
        let (row, col, per_side) = Self::position(side, x, y, fctx);
        if col == 0 {
            return row * side + side < reach_sb_px(fctx);
        }
        if row + 1 == per_side {
            return false;
        }
        // lane-sub8 r4: side 4 now has its own `has_tr_4x4`/`has_bl_4x4` row
        // (the clamp corner-cut that used to live here is gone); sides
        // 16/32/64 share row 0 exactly as libaom's tables repeat.
        // corner-cut: libaom transcribes no vert 4x4 row, so a side-4 block
        // inside a `PARTITION_VERT_A`/`_B` falls back to the ordinary table
        // -- a reach over-estimate (extra above-right reference pixels),
        // never a stream desync. Ceiling: transcribe `has_tr_vert_4x4` /
        // `has_bl_vert_4x4` and extend the VERT tables to four rows.
        // NB: named `table_row`, not `row` -- `row` above is the block's
        // position inside the superblock and is what the bit index below
        // steps by (a merge that shadowed it desynced every block size).
        let table_row = Self::table(side);
        let table = if VERT_AB_PARTITION.with(std::cell::Cell::get)
            && table_row < HAS_BOTTOM_LEFT_VERT.len()
        {
            HAS_BOTTOM_LEFT_VERT[table_row]
        } else {
            HAS_BOTTOM_LEFT[table_row]
        };
        Self::bit(
            table,
            (row * Self::table_stride(side) + col) % (table.len() * 8),
        )
    }

    /// Where a block sits inside its superblock, in blocks of its own size,
    /// and how many of them a superblock is across.
    pub(super) fn position(
        side: usize,
        x: usize,
        y: usize,
        fctx: &crate::decode::FrameCtx,
    ) -> (usize, usize, usize) {
        let sb = reach_sb_px(fctx);
        ((y % sb) / side, (x % sb) / side, sb / side)
    }

    /// The row stride `HAS_TOP_RIGHT`/`HAS_BOTTOM_LEFT` index by: libaom pins
    /// these tables to its compile-time maximum superblock (128, spec
    /// `MAX_MIB_SIZE_LOG2` = 5 in 4-pixel units), not to whichever superblock
    /// size an encode actually uses, so a block's row in the bit index steps
    /// by 128 / `side` even though this crate only ever codes a 64
    /// superblock -- using `per_side` (`SUPERBLOCK` / `side`, 64-relative)
    /// here instead indexes the wrong bit for every block size but the one
    /// where 64 and 128 give the same stride.
    pub(super) fn table_stride(side: usize) -> usize {
        128 / side
    }

    /// Which of the three block sizes the encoder codes a table row is for.
    pub(super) fn table(side: usize) -> usize {
        if side == BLOCK {
            1
        } else if side == 8 {
            2
        } else if side == 4 {
            3
        } else {
            0
        }
    }

    /// One bit of a table, counting from the low end of its first byte.
    pub(super) fn bit(table: &[u8], index: usize) -> bool {
        (table[index / 8] >> (index % 8)) & 1 != 0
    }
}

/// What one symbol costs against a CDF, in bits.
/// Whether local warped motion is on: the `WARPED_CAUSAL` candidate in the
/// search, the frame header's `allow_warped_motion` and the sequence's
/// `enable_warped_motion`, all three together. ON by default since
/// lane-av1rejudge re-measured it at [`LAMBDA_SCALE`] 0.05 (the table at
/// [`warp_prediction`]); `EC_AV1_WARP=0` turns the tool -- and the two header
/// bits -- back off for an A/B on one build.
pub(crate) fn warp_on() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_WARP")
            .ok()
            .map(|v| !matches!(v.as_str(), "0" | "off"))
    });
    ENV.unwrap_or_else(|| crate::speed::at(&crate::speed::WARP))
}

/// The smallest LEAF an OBMC candidate is offered for, so the census can
/// attribute the tool's effect by footprint (`EC_AV1_OBMC_MIN`, default 8 --
/// every leaf; 16 or 32 turn the smaller leaves off). Only read when
/// `EC_AV1_OBMC` is on, and never by the 32x32 whole-block candidate.
///
/// MEASURED at native, `EC_AV1_OBMC=1` throughout, BD vs libaom / vs rav1e
/// (12 frames, four quantizers, the standing keep table):
///
/// | min side | film 1080p | film 2160p | screen |
/// |---|---|---|---|
/// | OBMC off | +18.0 / +0.7 | +47.3 / +19.2 | +59.4 / -10.0 |
/// | 32 (32x32 only) | +18.4 / +1.1 | +47.3 / +19.2 | +59.1 / -10.0 |
/// | 16 | +18.4 / +1.1 | +47.3 / +19.2 | +59.3 / -10.0 |
/// | 8 (every leaf) | +18.7 / +1.4 | +47.3 / +19.2 | +59.3 / -10.0 |
///
/// So the leaves are not the thing that was holding OBMC back: at 8x8 the
/// candidate WINS 9704 of the 11462 leaves that code the symbol on the 1080p
/// film -- 85% of them, far past libaom's own rate -- and costs another 0.3
/// BD points on top of the 0.4 the 32x32 candidate already costs. A tool
/// that wins the local RD on five blocks in six and loses ladder bytes is
/// the `local-RD-on-references` class again, not a missing footprint.
///
/// RE-MEASURED on lane-av1lambda at [`LAMBDA_SCALE`] 0.05 (min side 8):
/// +16.7 / -0.5, +47.1 / +19.2, +51.8 / -14.2 against the same build with
/// the knob off (+17.0 / -0.3, +47.0 / +19.2, +51.6 / -14.2). The 0.7-point
/// loss on the 1080p film became a 0.3-point win and the other two rows went
/// 0.1-0.2 the other way, so OBMC is now neutral rather than a loss -- half
/// of the "local win, ladder loss" it was charged with was the rate weight.
/// It still does not meet the keep rule (two down, one flat, both columns).
/// The tool stays OFF by default; the upgrade path is the price, not the size
/// (the 640x384 arm reads +78.9/+36.4, +94.9/+56.3, +54.2/+0.8 at min side 8
/// against +78.8/+36.1, +95.1/+56.4, +54.5/+1.1 off -- two rows down, the
/// 1080p film up, i.e. the same disagreement between the two gates).
///
/// RE-JUDGED again on lane-av1rejudge against the defaults this lane kept
/// (local warp on, [`LEAF_SECOND_NEW_MARGIN`] 1.2 -- native +16.2/-1.1,
/// +46.6/+18.8, +51.3/-14.4): OBMC at min side 8 on top of that base reads
/// +16.2/-1.0, +46.6/+18.8, +51.3/-14.4, i.e. three rows flat and the 1080p
/// rav1e column 0.1 WORSE, so the tool buys nothing once the second-reference
/// margin is loosened. On warp alone it looked like a keep (+16.3/-0.9 at
/// min 8, +16.6/-0.7 at min 16 against warp's +16.7/-0.5) -- that gain and
/// the margin's gain are the same bytes. It stays OFF.
///
/// RE-MEASURED on lane-obmc3 (2026-09-08) on the REAL film rows -- every
/// judgement above was taken when the gate's "film" rows were the `testsrc2`
/// COLOUR BARS fixtures (class `gate film rows are colour bars`), so this is
/// the first OBMC measurement on film. BD vs libaom / vs rav1e, 12 pictures,
/// the standing native keep table (`bd_rate_screen_native`):
///
/// | arm | film A | film B | screen | OBMC fire A/B |
/// |---|---|---|---|---|
/// | off (control) | +21.7 / -4.4 | +26.9 / -0.6 | +20.1 / -30.4 | 0% / 0% |
/// | min side 8 | +21.7 / -4.3 | +27.2 / -0.3 | +20.0 / -30.4 | 15.5% / 9.7% |
/// | min side 16 | +21.5 / -4.5 | +27.6 / +0.1 | +20.1 / -30.4 | 10.0% / 6.9% |
///
/// and on the LONG-GOP gate (`bd_rate_film_long_gop`, 48 pictures, film B
/// only), where the anchors' non-skip area is the known gap:
///
/// | arm | film B long GOP | wall ours |
/// |---|---|---|
/// | off (control) | +89.8 / +9.1 | 477.5s |
/// | min side 8 | +89.6 / +9.0 | 514.1s |
///
/// The two gates disagree in SIGN on film B -- 0.3/0.3 worse over 12
/// pictures, 0.2/0.1 better over 48 -- and film A moves 0.2/0.1 the good way
/// only at min side 16, which is exactly where film B loses 0.7 on both
/// columns. Nothing here meets the keep rule (both films down on both
/// columns, or one down >=0.5 with the other flat), and the long-GOP gain
/// costs +7.7% wall. OBMC stays OFF at every preset.
pub(super) fn obmc_min_side() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("EC_AV1_OBMC_MIN")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8)
    })
}

/// What the `motion_mode`/`obmc` symbol costs (spec 5.11.24), off the tables
/// this frame's writer really starts from (`tile::obmc_symbol_bits`) -- the
/// same frame-CDF price the coefficients of a non-screen frame are estimated
/// with, not the static default table.
/// The same price for whichever alphabet the writer will really use for this
/// block: `warp_alphabet` is `motion_mode_allowed`'s 3-symbol choice
/// (`allow_warped_motion` and at least one warp sample), otherwise the
/// 2-symbol `obmc` one. Both sides of every motion-mode comparison in the
/// search pay it, so the alphabet cancels out of a comparison and only its
/// per-value spread decides.
pub(super) fn motion_mode_bits(write_w: usize, write_h: usize, motion: u8, warp_alphabet: bool) -> f64 {
    let Some(row) = crate::decode::motion_mode_cdf_row(write_w, write_h) else {
        return 0.0;
    };
    match warp_alphabet {
        true => crate::tile::motion_mode_symbol_bits(row, motion),
        false => crate::tile::obmc_symbol_bits(row, motion == 1),
    }
}

/// MEASURED 2026-09-06 and NOT KEPT ON BY DEFAULT (`EC_AV1_WARP=1` turns the
/// candidate, the header's `allow_warped_motion` and the sequence's
/// `enable_warped_motion` on together). BD vs libaom / vs rav1e, the standing
/// native keep table, against the same build with the knob off:
///
/// | clip | warp on | off |
/// |---|---|---|
/// | film 1080p | +18.7 / +1.6 | +18.0 / +0.7 |
/// | film 2160p | +47.3 / +19.2 | +47.3 / +19.2 |
/// | screen | +59.2 / -9.9 | +59.4 / -10.0 |
///
/// One row flat, one 0.2 down against libaom and 0.1 up against rav1e, and
/// the 1080p film 0.7/0.9 UP -- the keep rule wants two down and one flat, so
/// it stays off. The 640x384 arm reads +78.7/+36.5, +95.1/+56.4, +54.7/+1.2
/// against +78.8/+36.1, +95.1/+56.4, +54.5/+1.1. The census says the tool
/// fires: 7.0% / 2.7% / 2.4% of the blocks that code a motion_mode symbol
/// take WARPED_CAUSAL at native (197+91+2441 blocks on the 1080p film), so
/// this is a measured loss, not an inert knob. Same shape as the OBMC
/// candidate above it (`obmc_min_side`): a local RD win that does not convert
/// into ladder bytes.
///
/// RE-MEASURED on lane-av1lambda, after [`LAMBDA_SCALE`] moved 0.1 -> 0.05,
/// because a tool whose gain is local and whose cost is the rate it adds is
/// exactly what a halved rate weight re-judges. The verdict FLIPS SIGN:
///
/// | clip | warp on | off (both at lambda 0.05) |
/// |---|---|---|
/// | film 1080p | +16.7 / -0.5 | +17.0 / -0.3 |
/// | film 2160p | +47.0 / +19.1 | +47.0 / +19.2 |
/// | screen | +51.3 / -14.3 | +51.6 / -14.2 |
///
/// two rows down against libaom (0.3 each) with the third flat, and the rav1e
/// column inside 0.1 everywhere -- which reads as a keep rather than the
/// 0.7/0.9 loss it was.
///
/// KEPT ON BY DEFAULT on lane-av1rejudge, which reproduced that table
/// exactly (base +17.0/-0.3, +47.0/+19.2, +51.6/-14.2; warp +16.7/-0.5,
/// +47.0/+19.1, +51.3/-14.3 -- two rows down on both columns, one flat) and
/// ran the conformance shape the lambda lane deferred: every warp stream
/// decodes three-way exact (our decoder and ffmpeg, `EC_COMP_MISMATCH=1`
/// clean), the native gate runs clean under `EC_AV1_TILES=2:2` (warp inside
/// a 16-tile frame, sample walk and all), and tile bytes still do not depend
/// on the thread count. The tool fires on 5.4% / 3.3% / 3.3% of the blocks
/// that code a motion_mode symbol. `EC_AV1_WARP=0` ([`warp_on`]) turns it,
/// `allow_warped_motion` and `enable_warped_motion` back off together.
///
/// The WARPED_CAUSAL prediction of one square single-reference block: the
/// decoder's own warp-sample walk (`decode::find_samples` +
/// `warp::select_samples`), least-squares model (`warp::find_projection`) and
/// affine predictor (`warp::warp_affine`), driven off the encoder's own mi
/// grid -- never a second implementation of any of the three. `None` when the
/// block would code no motion_mode symbol at all, when it has no warp sample
/// (the writer then codes the 2-symbol alphabet, which has no WARPED value),
/// or when the projection is not filter-representable (the decoder would then
/// predict translationally, so the mode is pure cost).
#[allow(clippy::too_many_arguments)]
pub(super) fn warp_prediction(
    grid: &MiGrid,
    (mi_row, mi_col): (usize, usize),
    (mi_rows, mi_cols): (usize, usize),
    (x, y): (usize, usize),
    side: usize,
    mv: (i32, i32),
    ref_frame: i8,
    ref_planes: [(&[u16], usize, usize, usize); 3],
    fctx: &crate::decode::FrameCtx,
) -> Option<[Vec<u8>; 3]> {
    let n4 = side / 4;
    if !crate::decode::has_overlappable_neighbour(grid, mi_row, mi_col, n4, n4, mi_cols, mi_rows) {
        return None;
    }
    let mut samples = crate::decode::find_samples(
        grid, mi_row, mi_col, n4, n4, mi_cols, mi_rows, ref_frame, fctx,
    );
    if samples.is_empty() {
        return None;
    }
    if samples.len() > 1 {
        crate::warp::select_samples(mv, &mut samples, side as i32, side as i32);
    }
    let params = crate::warp::find_projection(
        &samples,
        side as i32,
        side as i32,
        mv.1,
        mv.0,
        mi_row as i32,
        mi_col as i32,
    )?;
    let mut pred = [
        vec![0u16; side * side],
        vec![0u16; side * side / 4],
        vec![0u16; side * side / 4],
    ];
    // The translational prediction first, exactly as the decoder builds it,
    // then the affine one over the planes whose OWN block is at least 8x8
    // (libaom `av1_init_warp_params` bails below that, so an 8x8 luma block's
    // 4x4 chroma stays translational).
    for (i, r) in ref_planes.into_iter().enumerate() {
        let luma_plane = i == 0;
        let (px, py) = if luma_plane { (x, y) } else { (x / 2, y / 2) };
        let s = if luma_plane { side } else { side / 2 };
        mc::predict(
            r.0,
            r.1,
            r.2,
            r.3,
            mv_to_q4(px, mv.1, luma_plane),
            mv_to_q4(py, mv.0, luma_plane),
            s,
            s,
            &mut pred[i],
            fctx,
        );
        if crate::decode::warp_plane_allowed(s, s) {
            let (sub, cside) = (usize::from(!luma_plane) as i32, s as i32);
            crate::warp::warp_affine(
                &params,
                r.0,
                r.2 as i32,
                r.3 as i32,
                r.1 as i32,
                &mut pred[i],
                px as i32,
                py as i32,
                cside,
                cside,
                cside,
                sub,
                sub,
                fctx,
            );
        }
    }
    let [py_buf, pu_buf, pv_buf] = pred;
    let u8s = |b: &[u16]| b.iter().map(|&v| v as u8).collect::<Vec<u8>>();
    Some([u8s(&py_buf), u8s(&pu_buf), u8s(&pv_buf)])
}

/// What tile.rs' `write_compound_ref_frames` pays for the pair
/// `(ref0, ref1)`, symbol for symbol, each at the context its own
/// `single_ref_p*_ctx`/`uni_comp_ref_p1_ctx`/`comp_reference_type_ctx`
/// derives from the block's above/left neighbours -- plus the `comp_mode`
/// symbol itself at `reference_mode_ctx`.
///
/// Before lane-ctx this was four constants at CDF row 0 that assumed the pair
/// was LAST+GOLDEN or LAST+ALTREF: `comp_bwdref[0][0] = 1` prices a BWDREF or
/// ALTREF2 partner as ALTREF and drops its `p6` symbol entirely, and the
/// `uni_comp_ref` leaves price a LAST+LAST2 pair as LAST+GOLDEN.
pub(crate) fn compound_ref_bits((ref0, ref1): (i8, i8), stack: &crate::mvstack::MvStack) -> f64 {
    use crate::mvstack::{
        ALTREF_FRAME, ALTREF2_FRAME, BWDREF_FRAME, GOLDEN_FRAME, LAST2_FRAME, LAST3_FRAME,
        comp_reference_type_ctx, is_uni_comp_ref, single_ref_p1_ctx, single_ref_p2_ctx,
        single_ref_p3_ctx, single_ref_p4_ctx, single_ref_p5_ctx, single_ref_p6_ctx,
        uni_comp_ref_p1_ctx,
    };
    let (a, a1) = stack.above_refs;
    let (l, l1) = stack.left_refs;
    let unidir = is_uni_comp_ref(ref0, ref1);
    let comp_mode = symbol_bits(
        &cdf::COMP_MODE[crate::mvstack::reference_mode_ctx(stack.above_nbr, stack.left_nbr)],
        1,
    );
    let ref_type = symbol_bits(
        &cdf::COMP_REF_TYPE[comp_reference_type_ctx(stack.above_nbr, stack.left_nbr)],
        usize::from(!unidir),
    );
    if unidir {
        let both_backward = (ref0, ref1) == (BWDREF_FRAME, ALTREF_FRAME);
        let p0 = symbol_bits(
            &cdf::UNI_COMP_REF[single_ref_p1_ctx(a, a1, l, l1)][0],
            usize::from(both_backward),
        );
        if both_backward {
            return comp_mode + ref_type + p0;
        }
        let past_last2 = ref1 != LAST2_FRAME;
        let p1 = symbol_bits(
            &cdf::UNI_COMP_REF[uni_comp_ref_p1_ctx(a, a1, l, l1)][1],
            usize::from(past_last2),
        );
        let p2 = if past_last2 {
            symbol_bits(
                &cdf::UNI_COMP_REF[single_ref_p5_ctx(a, a1, l, l1)][2],
                usize::from(ref1 == GOLDEN_FRAME),
            )
        } else {
            0.0
        };
        return comp_mode + ref_type + p0 + p1 + p2;
    }
    let far = ref0 == LAST3_FRAME || ref0 == GOLDEN_FRAME;
    let fwd = symbol_bits(
        &cdf::COMP_REF[single_ref_p3_ctx(a, a1, l, l1)][0],
        usize::from(far),
    ) + if far {
        symbol_bits(
            &cdf::COMP_REF[single_ref_p5_ctx(a, a1, l, l1)][2],
            usize::from(ref0 == GOLDEN_FRAME),
        )
    } else {
        symbol_bits(
            &cdf::COMP_REF[single_ref_p4_ctx(a, a1, l, l1)][1],
            usize::from(ref0 == LAST2_FRAME),
        )
    };
    let is_altref = ref1 == ALTREF_FRAME;
    let bwd = symbol_bits(
        &cdf::COMP_BWDREF[single_ref_p2_ctx(a, a1, l, l1)][0],
        usize::from(is_altref),
    ) + if is_altref {
        0.0
    } else {
        symbol_bits(
            &cdf::COMP_BWDREF[single_ref_p6_ctx(a, a1, l, l1)][1],
            usize::from(ref1 == ALTREF2_FRAME),
        )
    };
    comp_mode + ref_type + fwd + bwd
}

/// What the `skip` symbol costs at the context tile.rs really codes it with
/// (`above_skip[mi_c] + left_skip[mi_r]`, the two cells the MV stack now
/// carries) instead of row 0 -- the pricer's skip-versus-residual margin is
/// exactly the difference between this symbol's two values, so the row it is
/// read from moves that decision (class pricer-context-zero, lane-ctx).
pub(crate) fn skip_bits_at(stack: &crate::mvstack::MvStack, skip: bool) -> f64 {
    let ctx = usize::from(stack.above_skip) + usize::from(stack.left_skip);
    symbol_bits(&cdf::SKIP[ctx], usize::from(skip))
}

/// What the `is_inter` symbol costs at tile.rs' own `intra_inter_ctx` -- the
/// neighbours' intra/inter state, with an out-of-tile side contributing
/// nothing, off the same two cells (lane-ctx).
pub(crate) fn intra_inter_bits_at(stack: &crate::mvstack::MvStack, inter: bool) -> f64 {
    let ctx = crate::tile::intra_inter_ctx(
        stack.above_nbr.is_some(),
        stack.left_nbr.is_some(),
        stack.above_nbr.is_some_and(|n| n.is_inter),
        stack.left_nbr.is_some_and(|n| n.is_inter),
    );
    symbol_bits(&cdf::INTRA_INTER[ctx], usize::from(inter))
}

/// The `comp_mode = 0` symbol tile.rs' `write_comp_mode` codes ahead of a
/// SINGLE-reference block's reference syntax, at `reference_mode_ctx`'s own
/// context -- zero on a frame that carries no `reference_select` bit, where
/// the writer codes no symbol at all.
pub(super) fn comp_mode_single_bits(stack: &crate::mvstack::MvStack) -> f64 {
    if !reference_select() {
        return 0.0;
    }
    symbol_bits(
        &cdf::COMP_MODE[crate::mvstack::reference_mode_ctx(stack.above_nbr, stack.left_nbr)],
        0,
    )
}

/// What tile.rs' `write_single_ref` pays for `ref_frame`, symbol for symbol,
/// each at the context that symbol's own `single_ref_p*_ctx` derives from the
/// block's above/left neighbours -- plus the `comp_mode = 0` symbol
/// `write_comp_mode` codes ahead of it on a `reference_select` frame.
/// LAST is p1=0,p3=0,p4=0,
/// LAST2 p1=0,p3=0,p4=1, LAST3 p1=0,p3=1,p5=0, GOLDEN p1=0,p3=1,p5=1,
/// BWDREF p1=1,p2=0,p6=0, ALTREF2 p1=1,p2=0,p6=1 and ALTREF p1=1,p2=1.
/// Before lane-pricer every BACKWARD reference was priced as LAST -- three
/// symbols off the wrong CDFs -- which is what made the search believe an
/// ALTREF block cost the same syntax as a LAST one. The `comp_mode` term is
/// lane-ctx2's: a compound candidate paid `comp_mode = 1` while a single one
/// paid nothing at all, so compound was overcharged by the whole symbol.
pub(crate) fn single_ref_bits(ref_frame: i8, stack: &crate::mvstack::MvStack) -> f64 {
    use crate::mvstack::{
        ALTREF_FRAME, ALTREF2_FRAME, BWDREF_FRAME, GOLDEN_FRAME, LAST2_FRAME, LAST3_FRAME,
        single_ref_p1_ctx, single_ref_p2_ctx, single_ref_p3_ctx, single_ref_p4_ctx,
        single_ref_p5_ctx, single_ref_p6_ctx,
    };
    let (a, a1) = stack.above_refs;
    let (l, l1) = stack.left_refs;
    let comp_mode = comp_mode_single_bits(stack);
    let backward = ref_frame >= BWDREF_FRAME;
    let p1 = symbol_bits(
        &cdf::SINGLE_REF[single_ref_p1_ctx(a, a1, l, l1)][0],
        usize::from(backward),
    );
    if backward {
        let is_altref = ref_frame == ALTREF_FRAME;
        let p2 = symbol_bits(
            &cdf::SINGLE_REF[single_ref_p2_ctx(a, a1, l, l1)][1],
            usize::from(is_altref),
        );
        return comp_mode
            + p1
            + p2
            + if is_altref {
                0.0
            } else {
                symbol_bits(
                    &cdf::SINGLE_REF[single_ref_p6_ctx(a, a1, l, l1)][5],
                    usize::from(ref_frame == ALTREF2_FRAME),
                )
            };
    }
    let far = ref_frame == LAST3_FRAME || ref_frame == GOLDEN_FRAME;
    comp_mode
        + p1
        + symbol_bits(
            &cdf::SINGLE_REF[single_ref_p3_ctx(a, a1, l, l1)][2],
            usize::from(far),
        )
        + if far {
            symbol_bits(
                &cdf::SINGLE_REF[single_ref_p5_ctx(a, a1, l, l1)][4],
                usize::from(ref_frame == GOLDEN_FRAME),
            )
        } else {
            symbol_bits(
                &cdf::SINGLE_REF[single_ref_p4_ctx(a, a1, l, l1)][3],
                usize::from(ref_frame == LAST2_FRAME),
            )
        }
}

pub(crate) fn symbol_bits(cdf: &[u16], symbol: usize) -> f64 {
    let low = if symbol == 0 { 0 } else { cdf[symbol - 1] };
    let width = cdf[symbol] - low;
    // lane-av1speed: a CDF interval is a 15-bit width, so there are only
    // 32769 prices a symbol can carry; the `log2` behind each is computed
    // once instead of per call (libm's `__log2_fma` was 5% of the encoder's
    // profile -- this function is called for every symbol of every candidate
    // the RD search prices). Each entry is exactly the expression this used
    // to evaluate, so every price is bit-identical.
    static PRICES: std::sync::OnceLock<Vec<f64>> = std::sync::OnceLock::new();
    PRICES.get_or_init(|| {
        (0..=32768u32)
            .map(|w| -(f64::from(w) / 32768.0).log2())
            .collect()
    })[usize::from(width)]
}

/// What the tile writer spends to say a block is coded in each of the thirteen
/// modes, given the modes of the blocks above it and to its left: the luma mode
/// symbol against the CDF those two neighbours pick, the angle delta a
/// directional mode carries, and the chroma mode symbol, whose CDF the luma
/// mode itself indexes.
///
/// Without this the search is blind to what a mode costs to name, which is not
/// a rounding error: a directional mode on a picture that does not run that way
/// is a rate loss the squared error never sees.
/// Codes one square of the picture as a single block: searches the luma mode,
/// codes both chroma planes DC, and hands back the block and what it cost --
/// squared error plus lambda times the bits its symbols spend.
/// How many halvings of a `side`-square block's transform the `tx_depth`
/// symbol can name (libaom's per-size depth cap, `cdf::TX_SIZE_CAT0`'s two
/// symbols against `CAT1`..`CAT3`'s three), floored at the 4x4 transform.
pub(super) fn max_tx_depth(side: usize) -> usize {
    match side {
        8 => 1,
        16 | 32 => 2,
        _ => 0,
    }
}

/// What the `tx_depth` symbol itself costs, off the size category's own CDF.
/// Priced at context row 0 -- the writer picks the real row from its
/// neighbours' transform sizes, which the search has no view of; the term is
/// the same for every depth of one block either way, so it only shifts the
/// depth-0-versus-split margin, never the ordering between two splits.
pub(super) fn tx_depth_bits(side: usize, depth: usize) -> f64 {
    match side {
        8 => symbol_bits(&cdf::TX_SIZE_CAT0[0], depth),
        16 => symbol_bits(&cdf::TX_SIZE_CAT1[0], depth),
        32 => symbol_bits(&cdf::TX_SIZE_CAT2[0], depth),
        _ => symbol_bits(&cdf::TX_SIZE_CAT3[0], depth),
    }
}

/// lane-split: a multiplier on the RATE term of the transform-SPLIT trial
/// only -- the intra `tx_depth` depth>0 candidates in [`code_square`] and the
/// inter var-tx depth-1 trial in [`commit_inter_luma`]. `1.0` is the control
/// (the mode search's own weight); below 1 the split trial is charged less
/// for its bits and fires more often.
///
/// The knob the class `rd-rate-term-calibration` asks for: lane-txd made the
/// `tx_depth` SYMBOL price correct and every film row got worse, because the
/// correct row is a one-sided push away from splitting (depth 0 up to 1.5
/// bits cheaper at ctx 2, which is 66% of an inter frame's intra blocks) --
/// so what is mis-calibrated is the weight the split's rate is judged under,
/// not the price. `EC_AV1_SPLIT_LAMBDA`.
pub(crate) fn split_lambda_mult() -> f64 {
    static M: std::sync::LazyLock<f64> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_SPLIT_LAMBDA")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(SPLIT_LAMBDA)
    });
    *M
}

/// The shipped value of [`split_lambda_mult`]. See lanes/split.report.md for
/// the sweep that chose it.
pub(super) const SPLIT_LAMBDA: f64 = 1.0;
