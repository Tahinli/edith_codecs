//! Coefficient readers, the Neighbours context state, filter-intra and palette helpers. //! av1split: moved out of `decode/mod.rs`; the items are unchanged apart from visibility. (D20)

use super::*;

/// lane-coefread: the per-coefficient trace sites, one `#[cold]` call each.
/// `eprintln!` builds its `fmt::Arguments` on the stack AT THE CALL SITE, so
/// nine of them inside the base/sign loops pinned nine sets of values in the
/// hot loop's frame (the annotated profile was full of `mov ...(%rsp)` spill
/// traffic). Out of line, the loop keeps its values in registers and the
/// trace output is character-for-character what it was.
#[cold]
#[inline(never)]
fn trace_base_eob(scan_idx: usize, pos: usize, row: usize, col: usize, ctx: usize, v: i32) {
    eprintln!(
        "TRACE base_eob scan_idx={scan_idx} pos={pos} row={row} col={col} ctx={ctx} value={v}"
    );
}

#[cold]
#[inline(never)]
fn trace_base(scan_idx: usize, pos: usize, row: usize, col: usize, ctx: usize, v: i32) {
    eprintln!("TRACE base scan_idx={scan_idx} pos={pos} row={row} col={col} ctx={ctx} value={v}");
}

#[cold]
#[inline(never)]
fn trace_br(scan_idx: usize, pos: usize, row: usize, col: usize, ctx: usize, k: i32) {
    eprintln!("TRACE br scan_idx={scan_idx} pos={pos} row={row} col={col} ctx={ctx} value={k}");
}

#[cold]
#[inline(never)]
fn coeff_step_base(dec: &SymbolDecoder, scan_idx: usize, apos: usize, ctx: usize, v: i32) {
    let (rng, _) = dec.debug_state();
    eprintln!("EC_COEFF_STEP tag=base c={scan_idx} pos={apos} ctx={ctx} level={v} rng={rng}");
}

#[cold]
#[inline(never)]
fn coeff_step_br(dec: &SymbolDecoder, scan_idx: usize, apos: usize, ctx: usize, k: i32) {
    let (rng, _) = dec.debug_state();
    eprintln!("EC_COEFF_STEP tag=br c={scan_idx} pos={apos} ctx={ctx} k={k} rng={rng}");
}

#[cold]
#[inline(never)]
fn coeff_step_sign(dec: &SymbolDecoder, c: usize, negative: bool) {
    let (rng, _) = dec.debug_state();
    eprintln!(
        "EC_COEFF_STEP tag=sign c={c} sign={} rng={rng}",
        negative as i32
    );
}

#[cold]
#[inline(never)]
fn coeff_step_post_golomb(dec: &SymbolDecoder, c: usize, level: i32) {
    let (rng, _) = dec.debug_state();
    eprintln!("EC_COEFF_STEP tag=post_golomb c={c} level={level} rng={rng}");
}

#[cold]
#[inline(never)]
fn coeff_step_base_eob_level(dec: &SymbolDecoder, level: i32) {
    let (rng, _) = dec.debug_state();
    eprintln!("EC_COEFF_STEP tag=base_eob level={level} rng={rng}");
}

// lane-ibc444c: see `read_coeffs_rect`'s `#[track_caller]`.
#[track_caller]
pub(super) fn read_coeffs(
    dec: &mut SymbolDecoder,
    coding: &mut TxbTables,
    scan: &[u16],
    skip_ctx: usize,
    sign_ctx: usize,
    // The chroma default (spec `Intra_Mode_To_Tx_Type`, see
    // [`default_intra_tx_type`]) for the sizes where `coding.tx_type` codes
    // nothing at all; `DctDct` for every luma call and every chroma call at
    // a size the spec forces to `DCT_DCT` regardless of mode (32-point and
    // up, `EXT_TX_SET_DCTONLY`) -- the caller already folds that in.
    default_tx_type: TxType,
    // `Some((w, h))` with `w != h` when this read is a superblock-level
    // HORZ/VERT strip's truncated luma corner: the CDF *set* resolves square
    // (`side`/`coding` above), but `av1_nz_map_ctx_offset`'s position table
    // is indexed by the real, un-adjusted rectangular shape (lane-sbpart r8
    // root cause, see [`base_ctx`]). `None` everywhere else -- every other
    // caller's block genuinely is `side` x `side`.
    rect_shape: Option<(usize, usize)>,
) -> Result<(Grid, TxType)> {
    let trace = crate::envflags::env_flag!("EC_AV1_TRACE");
    let side = coding.side;
    // lane-av1readcensus: this unit's own entry state, so the census line
    // carries the pre-read rng and pairs against the oracle's `EC_COEFF`
    // (which prints `r->ec.rng` before the same `txb_skip` read).
    let census_entry_rng = dec.debug_state().0;
    let (census_w, census_h) = rect_shape.unwrap_or((side, side));
    // lane-av1ibcfork: the per-unit BIT INTERVAL, the twin of the oracle's
    // `EC_ECDUMP_IN` / `_OUT` / `_END`. The census plane, not a fresh label:
    // it is the same source `census_unit` reads, so the interval line and the
    // census line can never disagree about which plane a unit is on.
    let iv_plane = CENSUS_PLANE.with(|c| c.get()).min(2);
    let iv_bit = unit_iv_in(iv_plane, census_w, census_h, skip_ctx, dec);
    if crate::envflags::env_flag!("EC_AV1_EOBPT_CDF") {
        let (range, value) = dec.debug_state();
        eprintln!("EC_AV1_STATE_BEFORE_TXBSKIP range={range} value={value}");
    }
    if crate::envflags::env_flag!("EC_ECDUMP_IN") {
        let (range, value) = dec.debug_state();
        let bit = dec.debug_bitpos();
        eprintln!(
            "EC_ECDUMP_IN side={side} ctx={skip_ctx} dcctx={sign_ctx} ECIN=({value},{range},{bit})"
        );
    }
    let dbg_txbskip = if coeff_trace_on() {
        coding.txb_skip[skip_ctx]
    } else {
        [0; 3]
    };
    let sq_bit = if coeff_trace_on() {
        dec.debug_bitpos()
    } else {
        0
    };
    let all_zero = dec.symbol(&mut coding.txb_skip[skip_ctx]) == 1;
    unit_iv_skip(
        dec, iv_bit, iv_plane, census_w, census_h, skip_ctx, all_zero,
    );
    // lane-ibc444c: this reader logs an `all_zero` step even for a read that
    // consumes ZERO bits, so `post_bit == bit` means "no symbol was read" --
    // filter on it before pairing anything against the oracle.
    if crate::envflags::env_flag!("EC_IBCTX") {
        eprintln!(
            "EC_IBCTX2 side={} skip_ctx={skip_ctx} sign_ctx={sign_ctx} bit={sq_bit} at {}",
            coding.side,
            std::panic::Location::caller(),
        );
    }
    if coeff_trace_on() {
        let (rng, _) = dec.debug_state();
        eprintln!(
            "EC_COEFF_STEP tag=all_zero side={} ctx={skip_ctx} dcctx={sign_ctx} cdf={dbg_txbskip:?} all_zero={} rng={rng} bit={sq_bit} post_bit={}",
            coding.side,
            all_zero as i32,
            dec.debug_bitpos()
        );
    }
    if crate::envflags::env_flag!("EC_AV1_TELL") {
        eprintln!(
            "TELL label=post_txb_skip ctx={skip_ctx} all_zero={} tell={} range={}",
            all_zero as u8,
            dec.debug_bitpos(),
            dec.debug_state().0
        );
    }
    if trace {
        eprintln!("TRACE all_zero ctx={skip_ctx} value={}", all_zero as i32);
    }
    if crate::envflags::env_flag!("EC_AV1_EOBPT_CDF") {
        let (range, value) = dec.debug_state();
        eprintln!("EC_AV1_STATE_AFTER_TXBSKIP range={range} value={value}");
    }
    if all_zero {
        // lane-coefgrid: the all-zero unit (60% of the 4K segment's) names the
        // shared zero grid instead of allocating and zeroing its own.
        census_unit(
            census_w,
            census_h,
            TxType::DctDct,
            0,
            true,
            census_entry_rng,
            dec,
        );
        unit_iv_end(dec, iv_plane, census_w, census_h, skip_ctx);
        return Ok((Grid::Zero(side * side), TxType::DctDct));
    }
    let mut grid = vec![0i32; side * side];
    let mut tx_type = default_tx_type;
    if let Some(tx_type_cdf) = coding.tx_type.as_deref_mut() {
        // The CDF row's own width names its set (lane-cdffwd2: extended
        // past the original two-way intra split once wider `reduced_tx_set
        // == 0` inter sets joined the table): 17 slots (16 symbols) is
        // `EXT_TX_SET_ALL16`, 13 (12 symbols) `EXT_TX_SET_DTT9_IDTX_1DDCT`,
        // 8 (7 symbols) `TX_SET_INTRA_1`, everything else (6 slots, 5
        // symbols) the reduced `TX_SET_INTRA_2`/`TX_SET_INTER_3` two sets
        // share their symbol order with (`Tx_Type_Inter_Inv_Set3`'s two
        // members are a prefix of `Tx_Type_Intra_Inv_Set2`'s five).
        let len = tx_type_cdf.len();
        if crate::envflags::env_flag!("EC_AV1_EOBPT_CDF") && len == 3 {
            eprintln!("EC_AV1_TXTYPE32_CDF {tx_type_cdf:?}");
        }
        crate::msac::SymbolDecoder::set_symr_cdf("tx_type");
        let t = dec.symbol(tx_type_cdf);
        crate::census::tx_type(len, t);
        if coeff_trace_on() {
            let (rng, _) = dec.debug_state();
            eprintln!("EC_COEFF_STEP tag=tx_type len={len} rng={rng}");
        }
        if crate::envflags::env_flag!("EC_AV1_EOBPT_CDF") {
            let (range, value) = dec.debug_state();
            eprintln!("EC_AV1_STATE_AFTER_TXTYPE range={range} value={value}");
        }
        if trace {
            eprintln!("TRACE tx_type value={t} len={len}");
        }
        tx_type = tx_type_from_symbol(len, t).ok_or_else(|| {
            unsupported(format!("a tx_type symbol outside its CDF's own set: {t}"))
        })?;
    }

    let class = TxClass::of(tx_type);
    if class != TxClass::TwoD {
        hit!(TX_CLASS1_HITS);
        hit!(RECT_CLASS1_HITS);
    }
    let eob = read_eob(dec, coding, class);
    if trace {
        eprintln!("TRACE eob value={eob}");
    }
    let ec_trace_coeff = coeff_trace_on();
    if ec_trace_coeff {
        let (rng, _) = dec.debug_state();
        let cls = match class {
            TxClass::TwoD => "2d",
            TxClass::Horiz => "horiz",
            TxClass::Vert => "vert",
        };
        eprintln!("EC_COEFF_STEP tag=eob eob={eob} tx={tx_type:?} class={cls} rng={rng}");
    }
    let class_scan;
    let scan: &[u16] = if class == TxClass::TwoD {
        scan
    } else {
        class_scan = class_scan_table(side, class);
        &class_scan
    };
    // `side` is one of 4/8/16/32 (every `TxbTables` row), so the per-
    // coefficient `pos / side`, `pos % side` was a hardware divide where a
    // shift and a mask say the same thing, and the level buffer fits a fixed
    // 32x32 frame of stack instead of a heap allocation per transform block.
    debug_assert!(side.is_power_of_two() && side <= 32);
    let sh = side.trailing_zeros();
    let col_mask = side - 1;
    // lane-coef2: padded like libaom's own level buffer (see [`neighbour`]):
    // `stride = side + 4` columns and four spare rows, so the widest read
    // `base_ctx` makes -- `(row + 4, col)` / `(row, col + 4)` with
    // `row, col <= side - 1` -- stays inside `(side + 4) * (side + 4)`, at
    // most `36 * 36`. Only `col < side`, `row < side` cells are ever written.
    let stride = side + 4;
    let mut levels_buf = [0u8; 36 * 36];
    let levels = &mut levels_buf[..stride * stride];
    // lane-coefread: the sign pass used to re-walk `scan[..eob]`, redo the
    // `pos >> sh` / `& mask` decomposition and load the level back out of the
    // padded frame just to skip the zeros. The base pass already knows both,
    // so it records `(scan_idx << 4) | level` for each NON-ZERO coefficient
    // (`scan_idx < 1024`, `level <= 15` -- both loops assert it), and the sign
    // pass walks that list in reverse (the base pass runs in reverse scan
    // order, so reversing it is forward scan order: the same coefficients, in
    // the same order, with the same `c` the trace prints).
    let mut nz_buf = [0u16; 1024];
    let mut nz_n = 0usize;
    let (base_taps, br_taps) = flat_taps(stride, class);
    for scan_idx in (0..eob).rev() {
        let pos = scan[scan_idx] as usize;
        let (row, col) = (pos >> sh, pos & col_mask);
        let idx = row * stride + col;
        let level = if scan_idx == eob - 1 {
            let ctx = eob_coeff_ctx(scan_idx, side * side);
            let v = dec.symbol(&mut coding.base_eob[ctx]) as i32 + 1;
            if trace {
                trace_base_eob(scan_idx, pos, row, col, ctx, v);
            }
            v
        } else {
            let ctx = if class == TxClass::TwoD && rect_shape.is_none() {
                if row == 0 && col == 0 {
                    0
                } else {
                    base_mag(levels, idx, &base_taps)
                        + cdf::NZ_MAP_CTX_OFFSET_32[row.min(4)][col.min(4)] as usize
                }
            } else {
                base_ctx(levels, stride, row, col, class, rect_shape)
            };
            let v = dec.symbol(&mut coding.base[ctx]) as i32;
            if ec_trace_coeff {
                // `pos` in libaom's column-major `coeff_idx` convention, so
                // the ladder lines up with instrumented `aomdec`.
                coeff_step_base(dec, scan_idx, col * side + row, ctx, v);
            }
            if trace {
                trace_base(scan_idx, pos, row, col, ctx, v);
            }
            v
        };
        let level = if level > NUM_BASE_LEVELS {
            let ctx = {
                let mag = br_mag(levels, idx, &br_taps);
                if row == 0 && col == 0 {
                    mag
                } else {
                    let near_origin = match class {
                        TxClass::TwoD => row < 2 && col < 2,
                        TxClass::Horiz => col == 0,
                        TxClass::Vert => row == 0,
                    };
                    if near_origin { mag + 7 } else { mag + 14 }
                }
            };
            let mut level = level;
            let mut sent = 0;
            loop {
                let k = dec.symbol(&mut coding.br[ctx]) as i32;
                if ec_trace_coeff {
                    // lane-leaf8tx r3: the `br` rung was missing from THIS
                    // path's trace, so the cross-decoder range ladder was
                    // blind to every base-range symbol and mis-reported the
                    // first divergence (class [[gate-blind-to-feature]]).
                    coeff_step_br(dec, scan_idx, col * side + row, ctx, k);
                }
                if trace {
                    trace_br(scan_idx, pos, row, col, ctx, k);
                }
                level += k;
                sent += BR_STEP;
                if k < BR_STEP || sent >= COEFF_BASE_RANGE {
                    break;
                }
            }
            level
        } else {
            level
        };
        if ec_trace_coeff && scan_idx == eob - 1 {
            coeff_step_base_eob_level(dec, level);
        }
        // Every coded level here is a magnitude in `0..=15` (base <= 3 plus
        // at most `COEFF_BASE_RANGE` from the `br` loop; the Golomb tail is
        // added in the sign pass below, straight into `grid`), so the context
        // frame is bytes -- the five-tap sum in [`base_ctx`]/[`br_ctx`] is a
        // byte gather (lane-coefbuf).
        debug_assert!((0..=15).contains(&level), "level {level} past the u8 frame");
        levels[idx] = level as u8;
        if level != 0 {
            nz_buf[nz_n] = ((scan_idx as u16) << 4) | level as u16;
            nz_n += 1;
        }
    }
    if ec_trace_coeff {
        let (rng, _) = dec.debug_state();
        eprintln!("EC_COEFF_STEP tag=after_bases rng={rng}");
    }

    for &packed in nz_buf[..nz_n].iter().rev() {
        let c = (packed >> 4) as usize;
        let pos = scan[c] as usize;
        let level = (packed & 15) as i32;
        let negative = if pos == 0 {
            let v = dec.symbol(&mut coding.dc_sign[sign_ctx]) == 1;
            if trace {
                eprintln!("TRACE dc_sign ctx={sign_ctx} value={}", v as i32);
            }
            v
        } else {
            dec.literal(1) == 1
        };
        if ec_trace_coeff {
            coeff_step_sign(dec, c, negative);
        }
        let level = if level > MAX_BR_LEVEL {
            let g = read_golomb(dec)?;
            if trace {
                eprintln!("TRACE golomb pos={pos} value={g}");
            }
            level + g as i32
        } else {
            level
        };
        if ec_trace_coeff {
            coeff_step_post_golomb(dec, c, level);
        }
        grid[pos] = if negative { -level } else { level };
    }
    census_unit(
        census_w,
        census_h,
        tx_type,
        eob,
        false,
        census_entry_rng,
        dec,
    );
    unit_iv_end(dec, iv_plane, census_w, census_h, skip_ctx);
    Ok((Grid::Own(grid), tx_type))
}

/// [`read_coeffs`] widened to `(w, h)` (lane-rectwire; lane-rectx added the
/// `tx_type` read [`TxbSet::LumaRect16x8`] needs), restricted to
/// `TxClass::TwoD` -- the only class any rect size pair this decoder codes
/// can ever produce, since every `tx_type` this decoder's rect sets can read
/// (`EXT_TX_SET_DTT4_IDTX`'s five members) is itself 2D: refuses by name
/// rather than guess-decode if a non-2D class ever shows up here, since
/// neither `base_ctx_rect`/`br_ctx_rect` nor [`class_scan_table`] have a rect
/// form for those.
// lane-ibc444c: `#[track_caller]` so the EC_IBCTX rung below names the CALLER
// of this reader -- the only thing that identifies a unit's route. No plane is
// printed here: the plane is the ORACLE's side of the pairing, and a label
// here is one more thing that can disagree with it.
#[track_caller]
// Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
#[allow(clippy::too_many_arguments)]
pub(super) fn read_coeffs_rect(
    dec: &mut SymbolDecoder,
    coding: &mut TxbTables,
    scan: &[u16],
    w: usize,
    h: usize,
    skip_ctx: usize,
    sign_ctx: usize,
    default_tx_type: TxType,
    // lane-av1chrtx: the plane this unit belongs to (0 luma, 1 U, 2 V) --
    // printed by the two `EC_COEFF_STEP` traces below, which hardcoded
    // `plane=0` and so mis-attributed a CHROMA read as a luma one in two
    // independent lane reports.
    plane: usize,
) -> Result<(Grid, TxType)> {
    let rect_trace = coeff_trace_on();
    let entry_rng = dec.debug_state().0;
    // lane-av1readcensus: this reader is handed the real plane (lane-av1chrtx
    // threaded it in for the `EC_COEFF_STEP` traces), so it stamps the census
    // plane itself rather than inheriting whichever reader did last. The
    // luma-only readers (`decode_rect_split`, `decode_block_rect64`,
    // `read_inter_plane_rect`'s corner) are all plane 0 -- the stamp's initial
    // value -- and the other plane-owning readers stamp on entry.
    set_census_plane(plane);
    let ibctx_on = crate::envflags::env_flag!("EC_IBCTX");
    let entry_bit = if ibctx_on { dec.debug_bitpos() } else { 0 };
    let iv_bit = unit_iv_in(plane, w, h, skip_ctx, dec);
    let all_zero = dec.symbol(&mut coding.txb_skip[skip_ctx]) == 1;
    unit_iv_skip(dec, iv_bit, plane, w, h, skip_ctx, all_zero);
    if rect_trace {
        eprintln!(
            "EC_COEFF_STEP tag=all_zero plane={plane} ctx={skip_ctx} entry={entry_rng} all_zero={} rng={} bit={entry_bit} post_bit={}",
            all_zero as i32,
            dec.debug_state().0,
            dec.debug_bitpos()
        );
    }
    if ibctx_on {
        // lane-ibc444c. `bit`/`post_bit` are the STREAM POSITION, not the coder
        // range: the only collision-free key for pairing this unit against the
        // oracle's `EC_ECDUMP_IN`. And `post_bit != bit` is how a caller tells
        // a real symbol read from a 0-bit one -- the SQUARE reader logs a step
        // either way, which is exactly the artifact that made an earlier
        // round of this lane claim a false exactness.
        eprintln!(
            "EC_IBCTX w={w} h={h} skip_ctx={skip_ctx} sign_ctx={sign_ctx} bit={entry_bit} post_bit={} at {}",
            dec.debug_bitpos(),
            std::panic::Location::caller(),
        );
    }
    if all_zero {
        census_unit(w, h, TxType::DctDct, 0, true, entry_rng, dec);
        unit_iv_end(dec, plane, w, h, skip_ctx);
        return Ok((Grid::Zero(w * h), TxType::DctDct));
    }
    let mut grid = vec![0i32; w * h];
    hit!(RECT_COEFF_TU_HITS);
    // lane-tx4x8: a 4x8/8x4 luma TU DOES carry a `tx_type` symbol (its set is
    // `EXT_TX_SET_DTT4_IDTX(_1DDCT)`, `tx_size_sqr_up == TX_8X8`), including
    // the `V_DCT`/`H_DCT` members whose 1D tx class needs its own scan and
    // context offsets -- both of which now have rect forms
    // ([`class_scan_table_wh`], [`base_ctx_rect`]/[`br_ctx_rect`]'s `class`).
    // The rect sets that carry no symbol at all still pass `None` here.
    let mut tx_type = default_tx_type;
    if let Some(tx_type_cdf) = coding.tx_type.as_deref_mut() {
        let len = tx_type_cdf.len();
        let t = dec.symbol(tx_type_cdf);
        crate::census::tx_type(len, t);
        tx_type = tx_type_from_symbol(len, t).ok_or_else(|| {
            unsupported(format!("a tx_type symbol outside its CDF's own set: {t}"))
        })?;
        if rect_trace {
            eprintln!(
                "EC_COEFF_STEP tag=tx_type plane={plane} rng={}",
                dec.debug_state().0
            );
        }
    }
    let class = TxClass::of(tx_type);
    if class != TxClass::TwoD {
        hit!(TX_CLASS1_HITS);
    }
    let eob = read_eob(dec, coding, class);
    if rect_trace {
        eprintln!(
            "EC_COEFF_STEP tag=eob eob={eob} rng={}",
            dec.debug_state().0
        );
    }
    let class_scan;
    let scan: &[u16] = if class == TxClass::TwoD {
        scan
    } else {
        class_scan = class_scan_table_wh(w, h, class);
        &class_scan
    };
    // lane-coef2: a heap `vec![0i32; w * h]` per rect transform unit became a
    // stack frame sized to the unit -- same reasoning as [`read_coeffs`]:
    // `neighbour_rect` returns 0 outside `(w, h)` and every write is
    // `levels[pos]` with `pos < w * h`. `w` is a power of two at every AV1
    // rect size, so the per-coefficient `pos / w`, `pos % w` is a shift and a
    // mask rather than a hardware divide.
    let stride = w + 4;
    let n = stride * (h + 4);
    if !(w.is_power_of_two() && n <= 36 * 36) {
        panic!("rect coeff scratch w={w} h={h} n={n}");
    }
    let (mut b96, mut b256, mut b768);
    // Byte levels, same `0..=15` bound [`read_coeffs`] states.
    let levels: &mut [u8] = if n <= 96 {
        b96 = [0u8; 96];
        &mut b96[..n]
    } else if n <= 256 {
        b256 = [0u8; 256];
        &mut b256[..n]
    } else {
        b768 = [0u8; 768];
        &mut b768[..n]
    };
    let wsh = w.trailing_zeros();
    let wmask = w - 1;
    // lane-coefread: the non-zero list [`read_coeffs`] documents.
    let mut nz_buf = [0u16; 1024];
    let mut nz_n = 0usize;
    let (base_taps, br_taps) = flat_taps(stride, class);
    let (wlt, wgt) = (w < h, w > h);
    for scan_idx in (0..eob).rev() {
        let pos = scan[scan_idx] as usize;
        let (row, col) = (pos >> wsh, pos & wmask);
        let idx = row * stride + col;
        // libaom's column-major `coeff_idx`, for ladder comparison.
        let _apos = col * h + row;
        let level = if scan_idx == eob - 1 {
            let ctx = eob_coeff_ctx(scan_idx, w * h);
            let v = dec.symbol(&mut coding.base_eob[ctx]) as i32 + 1;
            // lane-av1oddheightfork2: the `base_eob` rung moved to the end of
            // the loop body (below). libaom prints its `tag=base_eob` AFTER
            // that coefficient's own br loop (decodetxb.c:317-338), so the rung
            // has to carry the POST-br level and rng to be the oracle's twin;
            // printed here it labelled the pre-br read and every rect unit's
            // ladder disagreed with aomdec by exactly its br tail.
            v
        } else {
            let ctx = if class == TxClass::TwoD {
                if row == 0 && col == 0 {
                    0
                } else {
                    let m = base_mag(levels, idx, &base_taps);
                    if wlt && row < 2 {
                        m + 11
                    } else if wgt && col < 2 {
                        m + 16
                    } else {
                        m + cdf::NZ_MAP_CTX_OFFSET_32[row.min(4)][col.min(4)] as usize
                    }
                }
            } else {
                base_ctx_rect(levels, stride, w, h, row, col, class)
            };
            let v = dec.symbol(&mut coding.base[ctx]) as i32;
            if rect_trace {
                eprintln!(
                    "EC_COEFF_STEP tag=base c={scan_idx} pos={pos} ctx={ctx} level={v} rng={}",
                    dec.debug_state().0
                );
            }
            v
        };
        let level = if level > NUM_BASE_LEVELS {
            let ctx = {
                let mag = br_mag(levels, idx, &br_taps);
                if row == 0 && col == 0 {
                    mag
                } else {
                    let near_origin = match class {
                        TxClass::TwoD => row < 2 && col < 2,
                        TxClass::Horiz => col == 0,
                        TxClass::Vert => row == 0,
                    };
                    if near_origin { mag + 7 } else { mag + 14 }
                }
            };
            let mut level = level;
            let mut sent = 0;
            loop {
                let k = dec.symbol(&mut coding.br[ctx]) as i32;
                if rect_trace {
                    eprintln!(
                        "EC_COEFF_STEP tag=br c={scan_idx} pos={pos} ctx={ctx} k={k} rng={}",
                        dec.debug_state().0
                    );
                }
                level += k;
                sent += BR_STEP;
                if k < BR_STEP || sent >= COEFF_BASE_RANGE {
                    break;
                }
            }
            level
        } else {
            level
        };
        debug_assert!((0..=15).contains(&level), "level {level} past the u8 frame");
        levels[idx] = level as u8;
        if level != 0 {
            nz_buf[nz_n] = ((scan_idx as u16) << 4) | level as u16;
            nz_n += 1;
        }
        if rect_trace && scan_idx == eob - 1 {
            // the oracle's `tag=base_eob`: POST-br level, POST-br rng, with the
            // same `get_lower_levels_ctx_eob` ctx (the same `eob_coeff_ctx`).
            let ctx = eob_coeff_ctx(scan_idx, w * h);
            let (rng, _) = dec.debug_state();
            eprintln!(
                "EC_COEFF_STEP tag=base_eob c={scan_idx} pos={pos} ctx={ctx} level={level} rng={rng}"
            );
        }
    }
    if rect_trace {
        let (rng, _) = dec.debug_state();
        eprintln!("EC_COEFF_STEP tag=after_bases rng={rng}");
    }
    for &packed in nz_buf[..nz_n].iter().rev() {
        let pos = scan[(packed >> 4) as usize] as usize;
        let level = (packed & 15) as i32;
        let negative = if pos == 0 {
            dec.symbol(&mut coding.dc_sign[sign_ctx]) == 1
        } else {
            dec.literal(1) == 1
        };
        if rect_trace {
            let (rng, _) = dec.debug_state();
            eprintln!(
                "EC_COEFF_STEP tag=sign_rect pos={pos} sign={} dcctx={sign_ctx} rng={rng}",
                u8::from(negative)
            );
        }
        let level = if level > MAX_BR_LEVEL {
            let g = read_golomb(dec)?;
            level + g as i32
        } else {
            level
        };
        if rect_trace {
            // lane-av1oddheightfork2: the twin of the square reader's
            // `coeff_step_post_golomb`. The oracle's `EC_COEFF_STEP
            // tag=post_golomb` (decodetxb.c, after read_golomb) fires for a RECT
            // unit exactly as it does for a square one, but this reader had no
            // rung for it, so every rect unit's ladder was one step short and
            // the pairing had to guess whether a golomb had been read. This
            // rung reads no bits: it prints the state after the read it labels.
            let (rng, _) = dec.debug_state();
            eprintln!(
                "EC_COEFF_STEP tag=post_golomb c={} pos={pos} level={level} rng={rng}",
                (packed >> 4)
            );
        }
        grid[pos] = if negative { -level } else { level };
    }
    // lane-d792: the rect unit's LEVEL grid in row-major (w) order, the
    // twin of the oracle's `EC_DQCOEFF` rung -- a residual mismatch whose
    // grids differ is a reader defect, one whose grids agree is a transform
    // defect.
    if crate::envflags::env_flag!("EC_DQCOEFF") {
        let nz: Vec<String> = grid
            .iter()
            .enumerate()
            .filter(|&(_, &v)| v != 0)
            .map(|(i, &v)| format!("{}/{}:{v}", i / w, i % w))
            .collect();
        eprintln!(
            "OUR_LEVELS w={w} h={h} eob={eob} tx={tx_type:?} nz={}",
            nz.join(",")
        );
    }
    census_unit(w, h, tx_type, eob, false, entry_rng, dec);
    unit_iv_end(dec, plane, w, h, skip_ctx);
    Ok((Grid::Own(grid), tx_type))
}

/// [`read_coeffs_rect`] for a CHROMA plane, carrying the monochrome gate the
/// [`read_plane`] family already has.
///
/// A monochrome frame has `NumPlanes == 1`, so libaom's coefficient loop
/// (`for (plane = 0; plane < av1_num_planes(cm); ++plane)`) never reaches a
/// chroma plane: no u/v coefficient symbol is coded, and consuming one moves
/// the arithmetic decoder off the stream for the rest of the tile. [`read_plane`]
/// enforces that for every strip that reads its chroma through it, but the rect
/// strip paths ([`decode_block_rect`], [`decode_leaf_rect`],
/// [`decode_block_rect4`], [`decode_rect4_16_strip`], [`decode_block_rect64`])
/// call `read_coeffs_rect` on planes 1/2 DIRECTLY, so each of those sites takes
/// this gate instead (class `reader-gate-not-on-every-path`). One un-gated
/// chroma read desyncs at the FIRST rect leaf of the frame and every later
/// symbol -- and therefore every later pixel -- is decoded from the wrong
/// place, without ever tripping a refusal (the desynced values stay inside
/// every coded alphabet).
// Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
#[allow(clippy::too_many_arguments)]
pub(super) fn read_chroma_coeffs_rect(
    dec: &mut SymbolDecoder,
    coding: &mut TxbTables,
    scan: &[u16],
    w: usize,
    h: usize,
    skip_ctx: usize,
    sign_ctx: usize,
    default_tx_type: TxType,
    fctx: &crate::decode::FrameCtx,
    // lane-av1chrtx: the plane index, threaded to `read_coeffs_rect`'s
    // `EC_COEFF_STEP` traces (they used to print a hardcoded `plane=0`, so a
    // V-plane read was indistinguishable from a luma one in the log).
    plane: usize,
) -> Result<(Grid, TxType)> {
    if mono(fctx) {
        return Ok((Grid::Zero(w * h), TxType::DctDct));
    }
    read_coeffs_rect(
        dec,
        coding,
        scan,
        w,
        h,
        skip_ctx,
        sign_ctx,
        default_tx_type,
        plane,
    )
}

/// What one coded block leaves behind for the blocks that read it as a
/// neighbour: whether it coded anything at all, and the sign of its DC —
/// [`crate::tile`]'s own private `Neighbour`.
#[derive(Clone, Copy, Default)]
pub(super) struct Neighbour {
    /// The transform unit's cumulative coefficient level (spec `cul_level`,
    /// `decodetxb.c`'s `read_coeffs_txb`): the sum of every coded level's
    /// magnitude, clamped to `COEFF_CONTEXT_MASK` (7). A plain "coded or not"
    /// boolean (`level != 0`) is enough for every context this decoder read
    /// until now, but the *luma* `txb_skip_ctx` of a transform unit smaller
    /// than its own block (`TxMode::Select`, spec `get_txb_ctx_general`)
    /// needs the actual magnitude tier of its above/left neighbours, not
    /// just whether they coded anything.
    pub(super) level: u8,
    pub(super) dc: Option<bool>,
}

/// `dst[start .. start + n] = v`, with the same per-cell bounds behaviour the
/// per-4x4-cell `get_mut` loops it replaces had: a cell past the end of the
/// grid is dropped, never a panic. One `memset` per grid row instead of a
/// bounds check per cell (lane-mi) -- the index span a row of a mi rect covers
/// is contiguous, so this writes exactly the cells the cell loop did,
/// including the ones a `mi_c + cc` past `skip_grid_cols_mi` folded into the
/// next row.
#[inline]
pub(super) fn fill_span<T: Copy>(dst: &mut [T], start: usize, n: usize, v: T) {
    let end = (start + n).min(dst.len());
    if start < end {
        dst[start..end].fill(v);
    }
}

pub(super) fn neighbour_state(grid: &[i32]) -> Neighbour {
    // lane-coefgrid: the shared zero grid sums to zero by construction; this
    // sum has no early exit, so the 60% of units that code nothing used to
    // walk their whole grid here.
    if is_zero_grid(grid) {
        return Neighbour::default();
    }
    let cul: u32 = grid.iter().map(|&l| l.unsigned_abs()).sum();
    Neighbour {
        level: cul.min(7) as u8,
        dc: (grid[0] != 0).then_some(grid[0] < 0),
    }
}

/// spec `get_txb_ctx_general`'s `skip_contexts[top][left]` table (plane 0,
/// transform unit smaller than the block it sits in): `top`/`left` are the
/// above/left neighbours' magnitude tiers, each already clamped to 4.
pub(crate) const SKIP_CONTEXTS: [[usize; 5]; 5] = [
    [1, 2, 2, 2, 3],
    [2, 4, 4, 4, 5],
    [2, 4, 4, 4, 5],
    [2, 4, 4, 4, 5],
    [3, 5, 5, 5, 6],
];

/// The `skip` flag, its written-instrument bit, and the covering coded
/// block's origin and dims, packed into ONE 8-byte cell (lane-publish): the
/// four maps are written together by `Neighbours::fill_skip_grid_rect` and by
/// nothing else, so a block's row is one fill instead of four, and the
/// deblocker's `pu_edge` reads origin and dims off one cache line.
#[derive(Clone, Copy)]
pub(super) struct BlkCell {
    pub(super) skip: bool,
    pub(super) written: bool,
    pub(super) org: [u16; 2],
    pub(super) dim: [u8; 2],
}

/// The neighbour state a block's contexts are read from — a duplicate of
/// [`crate::tile`]'s own private `Neighbours` (that module is another lane's
/// territory this round), kept at the same two granularities the real writer
/// tracks: `above_mode`/`left_mode`/`above_side`/`left_side` on the 16x16
/// (`SUB`) grid the partition/mode symbols read, and `above`/`left`'s
/// per-plane coded/DC-sign state on the finer 4x4 (`MI`) grid the coefficient
/// contexts read (spec `av1_set_entropy_contexts`) — which is what lets a
/// chroma transform straddling the true, odd-sized frame edge see its
/// neighbour's state clamped at its own subsampled edge rather than luma's.
#[derive(Clone)]
pub(super) struct Neighbours {
    pub(super) above: Vec<[Neighbour; 3]>,
    pub(super) left: Vec<[Neighbour; 3]>,
    pub(super) above_mode: Vec<usize>,
    pub(super) left_mode: Vec<usize>,
    /// Chroma's own `uv_mode` companion to `above_mode`/`left_mode` (lane-chroma
    /// r3): a smooth/paeth `uv_mode`'s edge-filter strength (spec
    /// `get_intra_edge_filter_type`) reads the CHROMA neighbour's mode, not the
    /// luma one -- the same block can (and often does) code a different
    /// `uv_mode` than its own `mode`. Coarse [`SUB`]-grid like `above_mode`;
    /// [`Self::decode_leaf8`]'s per-leaf `prev_leaf` override (a real
    /// within-block neighbour finer than this grid) is luma-only -- a known,
    /// unexercised-by-gate corner-cut (every real-encoder gate in `stream.rs`
    /// uses `--min-partition-size=16`, so no leaf8 8x8 split ever reaches this).
    pub(super) above_uv_mode: Vec<usize>,
    pub(super) left_uv_mode: Vec<usize>,
    pub(super) above_side: Vec<usize>,
    pub(super) left_side: Vec<usize>,
    /// The same as `above_side`/`left_side`, but kept at the finer mi (4x4)
    /// granularity `above`/`left` are, rather than [`SUB`] -- [`crate::tile`]'s
    /// `above_side_mi`/`left_side_mi`: two 8x8 leaves of one straddling 16x16
    /// block (lane-av1-rect) share a single [`SUB`]-grid cell, so a coarse
    /// array cannot tell the second leaf's partition symbol that the first
    /// one was coded at 8x8.
    /// lane-publish2: `u8` (a block side is at most 128 px, and so is
    /// [`NO_NEIGHBOUR_SIDE`]) -- every block stamps these per mi.
    pub(super) above_side_mi: Vec<u8>,
    pub(super) left_side_mi: Vec<u8>,
    /// The luma `mode` of the last `BLOCK_4X4` leaf decoded at each mi column
    /// / row, with the mi position it was written at
    /// (`(usize::MAX, _)` = never written). `above_mode`/`left_mode` are on
    /// the coarse 16x16 grid, which cannot distinguish the two 4x4 leaves an
    /// 8x8 `PARTITION_SPLIT` puts in one mi column; libaom's intra-mode
    /// context reads `mi(row, col-1)`/`mi(row-1, col)` exactly, so a sub-8x8
    /// leaf whose neighbour is another split group needs that group's leaf 3
    /// (bottom-right), not the leaf the coarse slot happens to hold. Written
    /// and read only by [`decode_leaf_split4`]; the position guard makes it
    /// fall back to the coarse slot whenever the real neighbour was a block
    /// of 8x8 or larger (whose one mode is correct for every row it spans).
    /// lane-publish2: `(mi position, mode)` narrowed from `(usize, usize)` to
    /// `(u16, u8)` -- every block stamps these mi-granular maps, and 16 bytes
    /// per mi per side of publish traffic was pure width. `u16::MAX` is the
    /// "never written" sentinel (an mi index cannot reach it: AV1's largest
    /// frame is 65536 px = 16384 mi).
    pub(super) sub8_mode_col: Vec<(u16, u8)>,
    pub(super) sub8_mode_row: Vec<(u16, u8)>,
    /// The `uv_mode` twin of `sub8_mode_col`/`sub8_mode_row` (lane-sub8 r5):
    /// chroma's edge-filter type reads the CHROMA neighbour's `uv_mode`
    /// (`get_intra_edge_filter_type`, reconintra.c:974), and the coarse
    /// [`SUB`] slots cannot name it once a 16x16 is split into 8x8 leaves --
    /// the leaf above writes the same row slot the block to the left owns.
    pub(super) uv_mode_col: Vec<(u16, u8)>,
    pub(super) uv_mode_row: Vec<(u16, u8)>,
    /// Every mi cell's own `uv_mode`, the mi grid libaom's
    /// `chroma_above_mbmi`/`chroma_left_mbmi` read directly (`set_mi_row_col`,
    /// blockd.h). lane-t900 r12: the one-slot-per-column
    /// `uv_mode_col`/`uv_mode_row` maps above hold only the LAST block written
    /// in that column, so a sub-8x8 pair's own first (non-chroma-ref) half
    /// overwrote the row the pair's chroma-ref half then wanted to read --
    /// the mi-exact match failed and the coarse [`SUB`] band answered instead.
    /// `u8::MAX` = no block has written this cell yet (fall back to the maps).
    pub(super) uv_mode_grid: Vec<u8>,
    /// [`Self::uv_mode_grid`]'s stride, in mi units (superblock-padded, like
    /// every other band here).
    pub(super) uv_grid_cols: usize,
    /// lane-av1leftwit: which BLOCK wrote each cell of [`Self::uv_mode_grid`],
    /// as `(epoch << 24) | (mi_r << 12) | mi_c` of the writing block's own
    /// top-left mi. The `uv_mode` value alone cannot answer "are the two cells
    /// the `+ss_y` read compares written by the same block?", which is the
    /// whole question the `+ss_y` term turns on: the term is only observable
    /// when the left neighbour is 1 mi tall, i.e. when the cell this decoder
    /// used to read and the cell libaom reads hold DIFFERENT blocks. `epoch`
    /// is [`UV_OWNER_EPOCH`], bumped once per tile, so a cell no block wrote
    /// in this tile can never be mistaken for a written one.
    pub(super) uv_owner: Vec<u32>,
    /// Whether the block above/left of this column/row was coded `skip` --
    /// an inter frame's own `skip` context (spec `SkipContext`), which the
    /// key-frame writer never tracks (its skip context is always zero); a
    /// duplicate of [`crate::tile`]'s private `Neighbours::above_skip`/
    /// `left_skip`.
    pub(super) above_skip: Vec<bool>,
    pub(super) left_skip: Vec<bool>,
    /// libaom's `above_txfm_context`/`left_txfm_context` (`TXFM_CONTEXT`), at
    /// 4x4 mi granularity: the transform *width* (above) / *height* (left) in
    /// pixels last written over each unit by `set_txfm_ctxs`/
    /// `txfm_partition_update`, which is what an inter block's `txfm_split`
    /// symbol reads its context from (lane-txselect, spec 5.11.17).
    /// [`TXFM_CTX_INIT`] until a block writes one; a fresh full-height `left`
    /// array is exactly libaom's per-superblock-row reset, since each mi row
    /// is only ever visited inside one superblock row.
    pub(super) above_txfm: Vec<u8>,
    pub(super) left_txfm: Vec<u8>,
    /// Whether the block above/left of this column/row coded `skip_mode`
    /// (spec 5.11.23) -- `av1_get_skip_mode_context`'s own neighbour lookup,
    /// lane-av1comp round 14.
    pub(super) above_skip_mode: Vec<bool>,
    pub(super) left_skip_mode: Vec<bool>,
    /// Whether the block above/left was coded inter -- the `is_inter`
    /// context (spec `av1_get_intra_inter_context`), a duplicate of
    /// [`crate::tile`]'s private `Neighbours::above_inter`/`left_inter`.
    pub(super) above_inter: Vec<bool>,
    pub(super) left_inter: Vec<bool>,
    /// The actual reference frame (`1..=7`, spec's `MV_REFERENCE_FRAME`
    /// alphabet) the block above/left of this column/row coded, or `-1` for
    /// an intra neighbour/frame border -- the `single_ref_p1`..`p6` context
    /// functions (`mvstack::ref_ctx` family) need the real reference, not
    /// just "was it inter" (lane-av1refs: every inter block used to be
    /// `LAST_FRAME` by construction, so `above_inter`/`left_inter` alone was
    /// enough).
    pub(super) above_ref: Vec<i8>,
    pub(super) left_ref: Vec<i8>,
    /// The block above/left's *second* reference frame when it was coded
    /// compound (spec `RefFrames[1]`), or `None` for single-ref/intra --
    /// lane-av1comp: `mvstack::reference_mode_ctx`/`comp_reference_type_ctx`
    /// need a full [`crate::mvstack::NeighbourRef`], not just `above_ref`'s
    /// `i8`, once a compound `read_ref_frames` lands.
    /// lane-publish2 audit: the "always `None` today" note this doc used to
    /// carry is long stale -- compound blocks do occur, and
    /// [`Self::record_compound_ctx_rect_mi`] stamps the real second reference
    /// over their span right after [`Self::record_inter_rect_mi`]'s reset.
    /// The array IS read: `single_ref_p*`/`comp_ref` contexts and every
    /// [`crate::mvstack::NeighbourRef`] construction (class
    /// [[branch-dropped-as-unreachable]] -- the note, not the code, was wrong).
    pub(super) above_ref1: Vec<Option<i8>>,
    pub(super) left_ref1: Vec<Option<i8>>,
    /// The precomputed `get_comp_group_idx_context`/`get_comp_index_context`
    /// neighbour contribution (spec 5.11.25, libaom `pred_common.h`):
    /// a compound neighbour's own `comp_group_idx`/`compound_idx` bit, or
    /// the "ref_frame[0] == ALTREF_FRAME" special case (`3`/`1`) for a
    /// single-ref one, or `0` for intra/unavailable -- lane-av1comp.
    pub(super) above_comp_group_idx: Vec<u8>,
    pub(super) left_comp_group_idx: Vec<u8>,
    pub(super) above_compound_idx: Vec<u8>,
    pub(super) left_compound_idx: Vec<u8>,
    /// `interp_filter[0]`/`[1]` (horizontal/vertical kernel index, 0..=2, or
    /// `3` = "no info": an intra neighbour or the frame's own border) of the
    /// block above/left of this column/row -- spec
    /// `av1_get_pred_context_switchable_interp`'s own neighbour lookup.
    pub(super) above_filter: Vec<[u8; 2]>,
    pub(super) left_filter: Vec<[u8; 2]>,
    pub(super) mi_cols: usize,
    pub(super) mi_rows: usize,
    /// Per-8x8 (2x2 mi cells) `skip_txfm` flag over the whole padded frame,
    /// spec `av1_cdef_compute_sb_list`'s `is_8x8_block_skip`: CDEF (spec
    /// 7.15) never filters an 8x8 luma block whose covering coded block read
    /// `skip` -- one flag per 4x4 mi cell (finer than needed, but matches the
    /// grain [`Self::record_mi`] already writes at), read back at 8x8
    /// granularity by [`apply_cdef`] since `skip` is constant across every
    /// mi cell one coded block covers.
    /// The four per-mi maps [`Neighbours::blk_grid`] packs, in one cell.
    /// lane-cdefstrip r1 instrument: whether any coded block actually WROTE
    /// the `skip` bit at this mi cell during this frame. A fresh
    /// `Neighbours` seeds `skip_grid` all-`false` ("not skip"), so a decode
    /// arm that forgets [`Self::fill_skip_grid_rect`] silently makes
    /// [`apply_cdef`] filter an 8x8 unit libaom's `is_8x8_block_skip`
    /// (`cdef.c:29-38`) excludes -- exactly the lane-cdef defect, and
    /// unobservable from the pixels alone. Counted by
    /// [`cdef_unwritten_skip_units`].
    /// Per-4x4-mi origin `(mi_row, mi_col)` of the coded (prediction) block
    /// covering that cell -- spec 7.14.2's `isBlockEdge`/libaom `pu_edge`,
    /// which re-enables the loop filter at a prediction boundary between two
    /// skipped inter blocks (lane-mergefix r4). Written by
    /// [`Self::fill_skip_grid_rect`], whose `at_mi` IS that origin.
    /// Per-4x4-mi `(width, height)` in mi units of the coded block covering
    /// that cell -- the other half of `pu_edge`: libaom tests the edge
    /// coordinate against the block's size in the FILTERED plane's own
    /// pixels, which for chroma is `max(luma_dim / 2, 4)` (lane-t900 r30b).
    pub(super) blk_grid: Vec<BlkCell>,
    pub(super) skip_grid_cols_mi: usize,
    /// Per-4x4-mi `is_inter_block` of the coded block COVERING that cell --
    /// lane-av1txctxband. libaom's `get_tx_size_context`
    /// (`av1/common/pred_common.h:361-370`) gates its "an inter neighbour
    /// contributes its own BLOCK size" override on `xd->above_mbmi` /
    /// `xd->left_mbmi`, which `set_mi_offsets` takes as `xd->mi[-mi_stride]`
    /// / `xd->mi[-1]`: the block whose bottom (left) edge abuts THIS block's
    /// top (right) edge at THIS cell. `setup_mbs` (`decodeframe.c:355-361`)
    /// writes that pointer into EVERY mi cell a block covers, so libaom's
    /// granularity is the covering block PER CELL -- not one slot per column.
    /// [`Self::above_inter`]/[`Self::left_inter`] are per-COLUMN bands holding
    /// the LAST block written in that column, which is the covering block only
    /// for the block immediately below it: on a 4:4:4 lossy `--sb-size=128`
    /// cq-62 stream the 8x4 intra leaf at mi(57,52) read `above_inter` as
    /// `true` (stale, from an intrabc block earlier in column 52) with
    /// `above_side_mi` 4, took the override, and read `tx_size_cat0[1]` where
    /// libaom reads row 2 -- the `EC_SYMR` fork at read 32813.
    /// Written by exactly the three places that already write the bands:
    /// [`Self::record_inter_rect_mi`], `record_intrabc_mi_rect` and
    /// `fill_lf_grid_rect`'s intra-only clear.
    pub(super) inter_grid: Vec<bool>,
    /// Per-4x4-mi luma transform width in pixels (8/16/32/64) -- this decoder
    /// never splits a coded block's transform below its own size (round 2:
    /// `TxMode::Select`/`tx_depth` refused), so a block's own side *is* its
    /// transform size, spec 7.14's `get_transform_size`/`get_filter_level`
    /// edge-length lookup (`apply_deblock`).
    pub(super) tx_grid: Vec<u8>,
    /// The transform HEIGHT companion to [`Self::tx_grid`]'s width --
    /// lane-rect r2: TX_32X16/TX_16X32 strips deblock their horizontal
    /// edges by tx height (spec 7.14.2 `set_lpf_parameters` dir==VERT).
    pub(super) tx_h_grid: Vec<u8>,
    /// Per-4x4-mi CHROMA transform width/height in pixels -- lane-tiny r4.
    /// A block's chroma transform is `av1_get_max_uv_txsize(bsize)`: the
    /// largest transform covering the subsampled BLOCK, capped at 32x32. It
    /// does NOT follow luma's `tx_depth` split, so deriving it as
    /// `tx_grid / 2` invented chroma transform edges the deblocker then
    /// filtered (32x16 frames, U plane, seeds 42/47). Written from the
    /// block's own mi span in [`Self::fill_lf_grid_rect`], the one place
    /// every caller routes through.
    pub(super) uv_tx_grid: Vec<u8>,
    pub(super) uv_tx_h_grid: Vec<u8>,
    /// Per-4x4-mi reference frame (`0` = intra, magnitude `1..=7` =
    /// `MV_REFERENCE_FRAME`, negative = that reference coded as `GLOBALMV` --
    /// `lf_level`'s `mode_lf_lut` row 0, lane-av1golden2)
    /// -- the loop filter's ref/mode delta lookup (spec 7.14.4
    /// `get_filter_level`, `loop_filter_ref_deltas[ref_frame]`; lane-av1refs:
    /// widened from a bare `is_inter` bool, which collapsed every non-`LAST`
    /// reference onto `LAST_FRAME`'s own delta).
    pub(super) ref_grid: Vec<i8>,
    /// Per-4x4-mi snapshot of [`CURRENT_DELTA_LF`] at the moment this cell's
    /// block was decoded (spec `DeltaLF[i]`, `i` in `0..FRAME_LF_COUNT`,
    /// Y-vertical/Y-horizontal/U/V) -- lane-realworld r5: `lf_level`'s
    /// per-block additive term, mirroring [`Self::ref_grid`]'s own snapshot
    /// pattern. `!delta_lf_multi` mode broadcasts index 0 into all 4 slots
    /// at write time ([`Self::fill_lf_grid_rect`]) so a reader never needs
    /// to know which mode produced the value.
    pub(super) delta_lf_grid: Vec<[i8; 4]>,
    /// The block above/left's own palette-Y size (0 = not a palette block)
    /// and its base colours -- `av1_get_palette_mode_ctx`/
    /// `av1_get_palette_cache` (pred_common.c)'s neighbour lookup, lane-palette
    /// r2. UV has no equivalent yet (stage 2, unreconstructed).
    pub(super) above_palette_size: Vec<u8>,
    pub(super) left_palette_size: Vec<u8>,
    pub(super) above_palette_colors: Vec<[u16; 8]>,
    pub(super) left_palette_colors: Vec<[u16; 8]>,
    /// As the four fields above, for the neighbour's own chroma (U-channel
    /// only -- `av1_get_palette_cache`'s plane-1 lookup, [`Self::palette_uv_cache`])
    /// palette size/colours (lane-palette2 r1).
    pub(super) above_palette_uv_size: Vec<u8>,
    pub(super) left_palette_uv_size: Vec<u8>,
    pub(super) above_palette_uv_colors: Vec<[u16; 8]>,
    pub(super) left_palette_uv_colors: Vec<[u16; 8]>,
    /// This tile's own top-left corner, in 4x4 mode-info units (spec
    /// `MiRowStart`/`MiColStart`) -- lane-tiles: every "is there really a
    /// neighbour" check compares against this instead of literal `0`, since a
    /// block at the tile's own top/left edge has no usable neighbour even
    /// though a decoded block sits there in the picture (spec `decode_tile`'s
    /// per-tile `clear_above_context`/`left_available`/`up_available`).
    /// `0` for a single-tile frame, matching every existing caller exactly.
    pub(super) tile_row0_mi: usize,
    pub(super) tile_col0_mi: usize,
}

#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
impl Neighbours {
    /// `cols`/`rows` are in [`SUB`] units; `mi_cols`/`mi_rows` are the frame's
    /// true (unpadded) size in 4x4 mode-info units.
    pub(super) fn new(
        cols: usize,
        rows: usize,
        mi_cols: usize,
        mi_rows: usize,
        fctx: &crate::decode::FrameCtx,
    ) -> Self {
        // lane-sb128c r7: a 128x64 block at the bottom frame edge records its
        // neighbour state over the whole 64-row extent, past the frame's last
        // mi row (mi_rows 198, block rows 192..208). libaom's own context
        // buffers are superblock-aligned for exactly that reason
        // (`av1_alloc_context_buffers` rounds to `MAX_MIB_SIZE`), so pad the
        // side arrays to a superblock multiple and leave those cells unread --
        // every availability check uses the true `mi_rows`/`mi_cols` below.
        let sb_sub = (sb_mi_cur(fctx) as usize) / (SUB / MI);
        let (cols, rows) = (cols.next_multiple_of(sb_sub), rows.next_multiple_of(sb_sub));
        Self {
            above: vec![[Neighbour::default(); 3]; cols * (SUB / MI)],
            left: vec![[Neighbour::default(); 3]; rows * (SUB / MI)],
            above_mode: vec![DC_PRED; cols],
            left_mode: vec![DC_PRED; rows],
            above_uv_mode: vec![DC_PRED; cols],
            left_uv_mode: vec![DC_PRED; rows],
            above_side: vec![NO_NEIGHBOUR_SIDE; cols],
            left_side: vec![NO_NEIGHBOUR_SIDE; rows],
            above_side_mi: vec![NO_NEIGHBOUR_SIDE as u8; cols * (SUB / MI)],
            left_side_mi: vec![NO_NEIGHBOUR_SIDE as u8; rows * (SUB / MI)],
            sub8_mode_col: vec![(u16::MAX, 0); cols * (SUB / MI)],
            sub8_mode_row: vec![(u16::MAX, 0); rows * (SUB / MI)],
            uv_mode_col: vec![(u16::MAX, 0); cols * (SUB / MI)],
            uv_mode_row: vec![(u16::MAX, 0); rows * (SUB / MI)],
            uv_mode_grid: vec![u8::MAX; cols * (SUB / MI) * rows * (SUB / MI)],
            uv_grid_cols: cols * (SUB / MI),
            uv_owner: vec![0; cols * (SUB / MI) * rows * (SUB / MI)],
            above_txfm: vec![TXFM_CTX_INIT; cols * (SUB / MI)],
            left_txfm: vec![TXFM_CTX_INIT; rows * (SUB / MI)],
            // lane-inter8 r2: mi(4px)-granular, not [`SUB`]-granular -- an
            // 8x8 inter leaf's left neighbour is the OTHER leaf row of the
            // 16x16 block beside it, which a SUB-grid slot cannot name
            // (class context-read-from-one-cell).
            above_skip: vec![false; cols * (SUB / MI)],
            left_skip: vec![false; rows * (SUB / MI)],
            above_skip_mode: vec![false; cols * (SUB / MI)],
            left_skip_mode: vec![false; rows * (SUB / MI)],
            above_inter: vec![false; cols * (SUB / MI)],
            left_inter: vec![false; rows * (SUB / MI)],
            above_ref: vec![-1; cols * (SUB / MI)],
            left_ref: vec![-1; rows * (SUB / MI)],
            above_ref1: vec![None; cols * (SUB / MI)],
            left_ref1: vec![None; rows * (SUB / MI)],
            above_comp_group_idx: vec![0; cols * (SUB / MI)],
            left_comp_group_idx: vec![0; rows * (SUB / MI)],
            above_compound_idx: vec![0; cols * (SUB / MI)],
            left_compound_idx: vec![0; rows * (SUB / MI)],
            above_filter: vec![[3u8; 2]; cols * (SUB / MI)],
            left_filter: vec![[3u8; 2]; rows * (SUB / MI)],
            mi_cols,
            mi_rows,
            blk_grid: vec![
                BlkCell {
                    skip: false,
                    written: false,
                    org: [0; 2],
                    dim: [1; 2]
                };
                cols * (SUB / MI) * rows * (SUB / MI)
            ],
            skip_grid_cols_mi: cols * (SUB / MI),
            inter_grid: vec![false; cols * (SUB / MI) * rows * (SUB / MI)],
            tx_grid: vec![0u8; cols * (SUB / MI) * rows * (SUB / MI)],
            tx_h_grid: vec![0u8; cols * (SUB / MI) * rows * (SUB / MI)],
            uv_tx_grid: vec![0u8; cols * (SUB / MI) * rows * (SUB / MI)],
            uv_tx_h_grid: vec![0u8; cols * (SUB / MI) * rows * (SUB / MI)],
            ref_grid: vec![0i8; cols * (SUB / MI) * rows * (SUB / MI)],
            delta_lf_grid: vec![[0i8; 4]; cols * (SUB / MI) * rows * (SUB / MI)],
            above_palette_size: vec![0; cols * (SUB / MI)],
            left_palette_size: vec![0; rows * (SUB / MI)],
            above_palette_colors: vec![[0u16; 8]; cols * (SUB / MI)],
            left_palette_colors: vec![[0u16; 8]; rows * (SUB / MI)],
            above_palette_uv_size: vec![0; cols * (SUB / MI)],
            left_palette_uv_size: vec![0; rows * (SUB / MI)],
            above_palette_uv_colors: vec![[0u16; 8]; cols * (SUB / MI)],
            left_palette_uv_colors: vec![[0u16; 8]; rows * (SUB / MI)],
            tile_row0_mi: 0,
            tile_col0_mi: 0,
        }
    }

    /// spec `decode_tile`'s per-tile reset: `clear_above_context` over this
    /// tile's own column span (`col1_mi` exclusive), plus recording
    /// `(row0_mi, col0_mi)` as the tile's own top-left mi cell for every
    /// availability check below. Left context resets every superblock row
    /// regardless of tile ([`Self::start_row`], already called once per SB
    /// row by every caller) so it needs no tile-specific reset here.
    pub(super) fn start_tile(
        &mut self,
        row0_mi: usize,
        col0_mi: usize,
        col1_mi: usize,
        fctx: &crate::decode::FrameCtx,
    ) {
        // lane-seg: `AvailU`/`AvailL` for the segment-id neighbour prediction
        // are tile-relative, same as every other neighbour lookup here.
        fctx.seg_tile_origin.with(|c| c.set((row0_mi, col0_mi)));
        // lane-av1leftwit: a fresh epoch stamps every `uv_owner` cell this
        // tile writes, so the `+ss_y` read's "different block?" probe never
        // compares against a cell a previous tile left behind.
        UV_OWNER_EPOCH.with(|c| c.set(c.get().wrapping_add(1) | (1 << 24)));
        self.tile_row0_mi = row0_mi;
        self.tile_col0_mi = col0_mi;
        let end = col1_mi.min(self.above.len());
        for i in col0_mi.min(end)..end {
            self.above[i] = Default::default();
            self.above_side_mi[i] = NO_NEIGHBOUR_SIDE as u8;
        }
        let (sc0, sc1) = (
            col0_mi / (SUB / MI),
            (col1_mi / (SUB / MI)).min(self.above_mode.len()),
        );
        for i in sc0.min(sc1)..sc1 {
            self.above_mode[i] = DC_PRED;
            self.above_side[i] = NO_NEIGHBOUR_SIDE;
        }
        // lane-inter8 r2: the inter bands are mi-granular, so they reset over
        // the tile's own mi column span directly.
        let mend = col1_mi.min(self.above_skip.len());
        for i in col0_mi.min(mend)..mend {
            self.above_skip[i] = false;
            self.above_skip_mode[i] = false;
            self.above_inter[i] = false;
            self.above_ref[i] = -1;
            self.above_ref1[i] = None;
            self.above_comp_group_idx[i] = 0;
            self.above_compound_idx[i] = 0;
            self.above_filter[i] = [3u8; 2];
        }
        // lane-sub8 r6: the mi-granular column maps are per tile too -- a
        // stale `(row, mode)` from the tile to the LEFT names an mi row this
        // tile's first block would read as its own above neighbour.
        let mcol_end = col1_mi.min(self.sub8_mode_col.len());
        for i in col0_mi.min(mcol_end)..mcol_end {
            self.sub8_mode_col[i] = (u16::MAX, 0);
            self.uv_mode_col[i] = (u16::MAX, 0);
        }
        // lane-mergefix r2: `av1_zero_above_context` also memsets the txfm row
        // over the tile's own column span (to `tx_size_wide[TX_SIZES_LARGEST]`,
        // not zero) -- without it a second tile column reads the first one's
        // resolved transform sizes as its own above context.
        let tend = col1_mi.min(self.above_txfm.len());
        for i in col0_mi.min(tend)..tend {
            self.above_txfm[i] = TXFM_CTX_INIT;
        }
    }

    /// Writes a just-decoded block's own palette-Y size/colours (`size == 0`
    /// for a non-palette block, clearing any stale neighbour state) into every
    /// [`SUB`]-grid cell it covers on both the above and left edges --
    /// mirrors [`Self::record_rect`]'s loop shape, square-only (this lane's
    /// call sites are all square blocks).
    pub(super) fn record_palette_y(&mut self, at: (usize, usize), side: usize, size: usize, colors: [u16; 8]) {
        self.record_palette_y_rect(
            (at.0 * (SUB / MI), at.1 * (SUB / MI)),
            side,
            side,
            size,
            colors,
        );
    }

    /// [`Self::record_palette_y`] with independent above (`w`)/left (`h`)
    /// extents (lane-palette2 r4, mirrors [`Self::record_rect`] vs
    /// [`Self::record`]): a true rect strip's above/left neighbour spans
    /// differ.
    pub(super) fn record_palette_y_rect(
        &mut self,
        mi: (usize, usize),
        w: usize,
        h: usize,
        size: usize,
        colors: [u16; 8],
    ) {
        let (r, c) = mi;
        if crate::envflags::env_flag!("EC_PALPUB") {
            eprintln!("EC_PALPUB yrect mi={r},{c} bw={w} bh={h} y={size}");
        }
        // lane-kf900 r6: mi (4px) granularity, libaom's own -- `palette_size`
        // lives per mi cell in `MB_MODE_INFO`, and `xd->left_mbmi` is the mi
        // cell immediately left of the block, not its 16px [`SUB`] cell. The
        // old SUB grid let a 16x8 strip pair share one slot, so the second
        // strip of mi(0,8) read the FIRST strip's palette size and the colour
        // cache came out one entry short (class [[context-read-from-one-cell]]).
        for cell in 0..(w / MI).max(1) {
            if let Some(e) = self.above_palette_size.get_mut(c + cell) {
                *e = size as u8;
                // lane-publish2: spec 5.11.46 `get_palette_cache` reads a
                // neighbour's colours only where its palette size > 0, so a
                // non-palette block need not stamp 16 bytes of zeroes here.
                if size > 0 {
                    self.above_palette_colors[c + cell] = colors;
                }
            }
        }
        for cell in 0..(h / MI).max(1) {
            if let Some(e) = self.left_palette_size.get_mut(r + cell) {
                *e = size as u8;
                if size > 0 {
                    self.left_palette_colors[r + cell] = colors;
                }
            }
        }
    }

    /// `av1_get_palette_mode_ctx` (pred_common.h:197): count of the above/left
    /// neighbours that are themselves a palette-Y block, plus
    /// `av1_get_palette_cache`'s own merged, ascending, deduplicated colour
    /// cache (pred_common.c:73).
    ///
    /// The SB-top-row exclusion of the above neighbour (`row % 64` in
    /// pred_common.c:76, `r % 4 == 0` here in [`SUB`]-grid units) belongs to
    /// the CACHE ALONE -- `av1_get_palette_mode_ctx` reads plain
    /// `xd->above_mbmi` with no such guard (lane-palette2 r7). Applying it to
    /// the ctx too made every superblock in the frame's second SB row read
    /// `palette_y_mode` off CDF row 0 while libaom used row 1, so a real
    /// palette block decoded as "no palette" and the tile desynced from
    /// there (class [[cdf-row-held-constant]]).
    pub(super) fn palette_ctx_and_cache(&self, at: (usize, usize)) -> (usize, Vec<u16>) {
        self.palette_ctx_and_cache_mi((at.0 * (SUB / MI), at.1 * (SUB / MI)))
    }

    /// [`Self::palette_ctx_and_cache`] with the block's REAL mi row, which a
    /// 1:4 strip does not share with its [`SUB`] cell: `av1_get_palette_cache`
    /// drops the above neighbour on a 64-px superblock boundary
    /// (pred_common.c:76, `row % (1 << MIN_SB_SIZE_LOG2)` with `row` the
    /// block's own PIXEL row), and strip 1 of a HORZ_4 sits 8 px into the
    /// cell -- reading `at.0 % 4` there excludes strip 0 from the cache of
    /// every strip in an SB's top cell row, which libaom does not do.
    pub(super) fn palette_ctx_and_cache_mi(&self, mi: (usize, usize)) -> (usize, Vec<u16>) {
        let (r, c) = mi;
        let above_ok = r % 16 != 0;
        // `xd->above_mbmi` exists for every row but the tile's own first.
        let above_is_palette = r > self.tile_row0_mi && self.above_palette_size[c] > 0;
        let (above_n, above_colors) = if above_ok && self.above_palette_size[c] > 0 {
            (
                usize::from(self.above_palette_size[c]),
                self.above_palette_colors[c],
            )
        } else {
            (0, [0u16; 8])
        };
        // lane-tiles r11: `av1_get_palette_cache` takes `xd->left_mbmi`, which
        // is NULL whenever the block sits at its TILE's own left column
        // (`left_available`), not the frame's -- without this guard a second
        // tile column reads the previous tile's palette colours into its cache
        // and its `palette_y_mode` ctx.
        let left_ok = c > self.tile_col0_mi;
        let (left_n, left_colors) = if left_ok && self.left_palette_size[r] > 0 {
            (
                usize::from(self.left_palette_size[r]),
                self.left_palette_colors[r],
            )
        } else {
            (0, [0u16; 8])
        };
        let ctx = usize::from(above_is_palette) + usize::from(left_n > 0);
        let mut cache = Vec::with_capacity(16);
        let push_dedup = |cache: &mut Vec<u16>, v: u16| {
            if cache.last() != Some(&v) {
                cache.push(v);
            }
        };
        let (mut ai, mut li) = (0usize, 0usize);
        while ai < above_n && li < left_n {
            let (va, vl) = (above_colors[ai], left_colors[li]);
            if vl < va {
                push_dedup(&mut cache, vl);
                li += 1;
            } else {
                push_dedup(&mut cache, va);
                ai += 1;
                if vl == va {
                    li += 1;
                }
            }
        }
        while ai < above_n {
            push_dedup(&mut cache, above_colors[ai]);
            ai += 1;
        }
        while li < left_n {
            push_dedup(&mut cache, left_colors[li]);
            li += 1;
        }
        (ctx, cache)
    }

    /// As [`Self::record_palette_y`], for a just-decoded block's own chroma
    /// (U-channel) palette size/colours -- `size == 0` clears stale state the
    /// same way (lane-palette2 r1).
    pub(super) fn record_palette_uv(
        &mut self,
        at: (usize, usize),
        side: usize,
        size: usize,
        colors: [u16; 8],
    ) {
        self.record_palette_uv_rect(
            (at.0 * (SUB / MI), at.1 * (SUB / MI)),
            side,
            side,
            size,
            colors,
        );
    }

    /// [`Self::record_palette_uv`] with independent above (`w`)/left (`h`)
    /// extents, in LUMA pixels (lane-palette2 r4, same convention as
    /// [`Self::record_palette_y_rect`] -- the above/left grids are indexed at
    /// luma resolution regardless of which plane the recorded state is for).
    pub(super) fn record_palette_uv_rect(
        &mut self,
        mi: (usize, usize),
        w: usize,
        h: usize,
        size: usize,
        colors: [u16; 8],
    ) {
        let (r, c) = mi;
        // mi granularity, see [`Self::record_palette_y_rect`].
        for cell in 0..(w / MI).max(1) {
            if let Some(e) = self.above_palette_uv_size.get_mut(c + cell) {
                *e = size as u8;
                if size > 0 {
                    self.above_palette_uv_colors[c + cell] = colors;
                }
            }
        }
        for cell in 0..(h / MI).max(1) {
            if let Some(e) = self.left_palette_uv_size.get_mut(r + cell) {
                *e = size as u8;
                if size > 0 {
                    self.left_palette_uv_colors[r + cell] = colors;
                }
            }
        }
    }

    /// [`Self::palette_ctx_and_cache`]'s cache half, for the U channel
    /// (`av1_get_palette_cache(xd, 1, cache)`) -- `palette_uv_mode_ctx` needs
    /// no neighbour lookup at all (it is this block's own just-decided Y
    /// palette use, `read_palette_mode_info`'s `pmi->palette_size[0] > 0`),
    /// so only the cache is wanted here.
    pub(super) fn palette_uv_cache(&self, at: (usize, usize)) -> Vec<u16> {
        self.palette_uv_cache_mi((at.0 * (SUB / MI), at.1 * (SUB / MI)))
    }

    /// [`Self::palette_uv_cache`] with the block's real mi row -- see
    /// [`Self::palette_ctx_and_cache_mi`].
    pub(super) fn palette_uv_cache_mi(&self, mi: (usize, usize)) -> Vec<u16> {
        let (r, c) = mi;
        let above_ok = r % 16 != 0;
        let (above_n, above_colors) = if above_ok && self.above_palette_uv_size[c] > 0 {
            (
                usize::from(self.above_palette_uv_size[c]),
                self.above_palette_uv_colors[c],
            )
        } else {
            (0, [0u16; 8])
        };
        // Same tile-relative left availability as the luma cache above.
        let left_ok = c > self.tile_col0_mi;
        let (left_n, left_colors) = if left_ok && self.left_palette_uv_size[r] > 0 {
            (
                usize::from(self.left_palette_uv_size[r]),
                self.left_palette_uv_colors[r],
            )
        } else {
            (0, [0u16; 8])
        };
        let mut cache = Vec::with_capacity(16);
        let push_dedup = |cache: &mut Vec<u16>, v: u16| {
            if cache.last() != Some(&v) {
                cache.push(v);
            }
        };
        let (mut ai, mut li) = (0usize, 0usize);
        while ai < above_n && li < left_n {
            let (va, vl) = (above_colors[ai], left_colors[li]);
            if vl < va {
                push_dedup(&mut cache, vl);
                li += 1;
            } else {
                push_dedup(&mut cache, va);
                ai += 1;
                if vl == va {
                    li += 1;
                }
            }
        }
        while ai < above_n {
            push_dedup(&mut cache, above_colors[ai]);
            ai += 1;
        }
        while li < left_n {
            push_dedup(&mut cache, left_colors[li]);
            li += 1;
        }
        cache
    }

    /// Marks every 4x4 mi cell a just-decoded coded block covers with its own
    /// transform width (in pixels) and `is_inter` flag -- the loop filter's
    /// per-edge lookup (`apply_deblock`), filled at the same call sites and
    /// units as [`Self::fill_skip_grid`].
    pub(super) fn fill_lf_grid(
        &mut self,
        at_mi: (usize, usize),
        side_mi: usize,
        tx_px: u8,
        ref_frame: i8,
        fctx: &crate::decode::FrameCtx,
    ) {
        self.fill_lf_grid_rect(at_mi, side_mi, side_mi, tx_px, tx_px, ref_frame, fctx);
    }

    /// [`Self::fill_lf_grid`] with independent row/col extents -- lane-partitions
    /// r1: a true rectangular strip's mi span isn't `side_mi` square (`w_mi`
    /// != `h_mi`); `tx_px` still names a single scalar (this decoder's
    /// existing corner-cut, `Neighbours::tx_grid`'s own doc comment -- a
    /// rectangular strip's transform is `TX_32X16`/`TX_16X32`, not tracked
    /// per-axis, matching lane-warp r5's `HORZ_B` top-strip precedent).
    /// libaom `set_one_param_for_line_luma`/`_chroma`: an edge that is a
    /// transform-unit edge but NOT a prediction-unit edge is not filtered when
    /// `curr_skipped = mbmi->skip_txfm && is_inter_block(mbmi)`. Widening this
    /// block's published transform extent to its own size is exactly that
    /// suppression for the deblocker's `x0 % tx != 0` test -- the only shape
    /// that needs it is the 128 root, whose chroma extent is the one
    /// [`Self::fill_lf_grid_rect`]'s TX_32X32 cap narrows. An INTRA block is
    /// never suppressed (libaom's `is_inter_block` term), so this is called
    /// only from the skipped-inter branch.
    ///
    /// The extent is the block's own CHROMA footprint, `w_mi * MI >> ss_x` per
    /// axis -- `av1_setup_dst_planes`'s `get_plane_block_size(bsize, ss_x,
    /// ss_y)` (`common_data.c:38`, which at ss (0,0) is BLOCK_128X128
    /// itself, so 128 and not 64). This used to be a hardcoded `w_mi * MI / 2`,
    /// correct at 4:2:0 by the `>> 1 == / 2` identity and half the truth at
    /// 4:4:4; lane-av1txsizeaudit r2 replaced it with the per-axis shift, the
    /// form its sibling `fill_lf_grid_rect` already used. The `fctx` is
    /// threaded in for it: `Neighbours` carries no subsampling of its own, so
    /// no format guard was possible inside this method until now.
    ///
    /// **MEASURED INERT, and do not go looking for a pixel regression.** The
    /// widening defeats the deblocker's alignment gate (`x0 & (tx - 1)`), but
    /// every edge it lets through then meets the SAME libaom rule applied at
    /// the correct place, `edge_params_body`'s spec-7.14.2 / libaom
    /// `set_lpf_parameters` term `!pu_edge && cur_ref != 0 && pv_ref != 0 &&
    /// skip_at && skip_at`. Inside one skipped block both cells are that same
    /// block, so all four terms hold and the edge dies there too. The masking
    /// is TOTAL, not incidental: this method is only ever called for a block
    /// that is itself skipped, so no input exists on which one side of an
    /// internal edge is not skipped. Measured over 26 frames / 2 555 904
    /// samples across five 4:4:4 `--sb-size=128` streams that fire this arm
    /// 30 times: `64` and `128` both decode pixel-exact against aomdec, and
    /// differ from each other in not one sample. The gate for this value is
    /// therefore a source scan, not a pixel gate --
    /// `refusal_inventory::the_skipped_128_root_chroma_suppression_publishes_the_blocks_own_per_axis_chroma_extent`.
    pub(super) fn suppress_internal_lf_edges(
        &mut self,
        at_mi: (usize, usize),
        w_mi: usize,
        h_mi: usize,
        fctx: &FrameCtx,
    ) {
        let (mi_r, mi_c) = at_mi;
        let (uv_w, uv_h) = (
            ((w_mi * MI) >> ss_x(fctx)).max(4) as u8,
            ((h_mi * MI) >> ss_y(fctx)).max(4) as u8,
        );
        for rr in 0..h_mi {
            let start = (mi_r + rr) * self.skip_grid_cols_mi + mi_c;
            fill_span(&mut self.uv_tx_grid, start, w_mi, uv_w);
            fill_span(&mut self.uv_tx_h_grid, start, w_mi, uv_h);
        }
    }

    // Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn fill_lf_grid_rect(
        &mut self,
        at_mi: (usize, usize),
        w_mi: usize,
        h_mi: usize,
        tx_px: u8,
        tx_h_px: u8,
        ref_frame: i8,
        fctx: &crate::decode::FrameCtx,
    ) {
        self.fill_lf_grid_rect_inner(at_mi, w_mi, h_mi, tx_px, tx_h_px, ref_frame, true, fctx)
    }

    /// [`Self::fill_lf_grid_rect`] for a block whose transform tree ALREADY
    /// published its own `TXFM_CONTEXT` bands -- every body that ran
    /// [`read_block_tx_size_rect`] / [`read_block_tx_size`] / an inline
    /// var-tx tree of its own.
    ///
    /// libaom's `parse_decode_block` (`decodeframe.c:1227-1236`) is an
    /// EITHER/OR: the var-tx branch calls `read_tx_size_vartx`, which
    /// publishes through `txfm_partition_update` PER LEAF
    /// (`av1_common_int.h:1684-1695`) and never calls `set_txfm_ctxs`; the
    /// `else` branch calls `read_tx_size` + `set_txfm_ctxs` exactly once, over
    /// the whole block. So on the var-tx path there is NO whole-footprint
    /// publish to reproduce, and one written here is not merely redundant --
    /// it OVERWRITES the leaves' sizes with the block's own.
    ///
    /// lane-av1oddheightfork3, measured on the pinned
    /// `420_oddheight_320x236.obu` (then diverging: Y 234349 / U 56957 /
    /// V 52981, 0/16 frames; byte-exact since this lane): the INTRABC 8x16
    /// block at mi(44,58) split into two 8x8
    /// leaves, whose per-leaf writes put `8` into `left_txfm[44..48]`, and
    /// this publish then put the BLOCK's `16` back over those same four
    /// cells. The next block, the INTRABC 8x16 at mi(44,60), read
    /// `txfm_partition_context` with `left = 16` where libaom has `8` (the
    /// `above` operand agreed at 8 on both sides, printed by the oracle rung
    /// `EC_VARTXCTX` in `scripts/instrument-aom-oracle.sh`), took ctx 12
    /// against libaom's 13, decoded `is_split = 0` against libaom's `1`, and
    /// read no children against libaom's two -- the frame's first divergent
    /// read.
    // Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn fill_lf_grid_rect_after_vartx(
        &mut self,
        at_mi: (usize, usize),
        w_mi: usize,
        h_mi: usize,
        tx_px: u8,
        tx_h_px: u8,
        ref_frame: i8,
        fctx: &crate::decode::FrameCtx,
    ) {
        self.fill_lf_grid_rect_inner(at_mi, w_mi, h_mi, tx_px, tx_h_px, ref_frame, false, fctx)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn fill_lf_grid_rect_inner(
        &mut self,
        at_mi: (usize, usize),
        w_mi: usize,
        h_mi: usize,
        tx_px: u8,
        tx_h_px: u8,
        ref_frame: i8,
        publish_txfm_bands: bool,
        fctx: &crate::decode::FrameCtx,
    ) {
        let (mi_r, mi_c) = at_mi;
        // `av1_get_max_uv_txsize` under 4:2:0: the block's chroma extent is
        // half its luma one (min 4 px, the smallest transform), capped at
        // TX_32X32 -- independent of this block's luma `tx_depth`.
        let uv_tx_w = ((w_mi * MI) >> ss_x(fctx)).clamp(4, 32) as u8;
        let uv_tx_h = ((h_mi * MI) >> ss_y(fctx)).clamp(4, 32) as u8;
        if crate::envflags::env_flag!("EC_TXGRID_TRACE") {
            eprintln!(
                "EC_LFGRID mi_row={mi_r} mi_col={mi_c} w_mi={w_mi} h_mi={h_mi} tx_px={tx_px} tx_h_px={tx_h_px}"
            );
        }
        crate::census::tx(tx_px, tx_h_px);
        let cur = fctx.current_delta_lf.with(|c| c.get());
        let snapshot: [i8; 4] = if fctx.delta_lf_multi.with(|c| c.get()) {
            std::array::from_fn(|i| cur[i].clamp(-63, 63) as i8)
        } else {
            [cur[0].clamp(-63, 63) as i8; 4]
        };
        for rr in 0..h_mi {
            let start = (mi_r + rr) * self.skip_grid_cols_mi + mi_c;
            fill_span(&mut self.tx_grid, start, w_mi, tx_px);
            fill_span(&mut self.tx_h_grid, start, w_mi, tx_h_px);
            fill_span(&mut self.uv_tx_grid, start, w_mi, uv_tx_w);
            fill_span(&mut self.uv_tx_h_grid, start, w_mi, uv_tx_h);
            fill_span(&mut self.ref_grid, start, w_mi, ref_frame);
            fill_span(&mut self.delta_lf_grid, start, w_mi, snapshot);
        }
        if publish_txfm_bands && fctx.intra_only.with(std::cell::Cell::get) {
            // libaom's `parse_decode_block` runs `set_txfm_ctxs(mbmi->tx_size,
            // xd->width, xd->height, mbmi->skip_txfm && is_inter_block(mbmi),
            // xd)` for EVERY block, an intra-only frame's included, and
            // `get_tx_size_context` / `txfm_partition_context` read those two
            // `TXFM_CONTEXT` bands. This grid carries the same transform size
            // over the same footprint, so the publish is this write: the
            // `skip && is_inter` term is 0 for every intra-only block that is
            // not an intrabc one, and the bodies that can code an intrabc
            // block publish their own (the term is then the BLOCK size, a value
            // this grid cannot express -- see `decode_block`/`decode_leaf8`).
            // Inter frames publish at their own sites and are excluded here
            // because their skipped-block term needs the block size too.
            txfm_partition_update_rect(
                self,
                at_mi,
                (tx_px as usize, tx_h_px as usize),
                (w_mi * MI, h_mi * MI),
            );
            // libaom `is_inter_block` counts an INTRABC block (`blockd.h:373`),
            // so `get_tx_size_context` reads an intrabc NEIGHBOUR's block size
            // instead of its transform band. Every block stamps its own cells
            // here; the intrabc bodies re-stamp theirs `true` afterwards
            // (`record_intrabc_inter_bands`).
            for i in 0..w_mi {
                if let Some(cell) = self.above_inter.get_mut(mi_c + i) {
                    *cell = false;
                }
            }
            for i in 0..h_mi {
                if let Some(cell) = self.left_inter.get_mut(mi_r + i) {
                    *cell = false;
                }
            }
            // lane-av1txctxband: the same clear at libaom's own per-CELL
            // granularity. The band above clears one slot per column, so a
            // block that covered column 52 and a later block in the SAME
            // column but a different row leave the band describing whichever
            // wrote last; the grid clears exactly the cells this block owns.
            self.mark_inter_grid_rect(at_mi, w_mi, h_mi, false);
        }
        // lane-av1-intrabc r4: publish this coded block's own mi footprint
        // into the intrabc MV grid's block-COVERAGE map -- every coded block,
        // not just the intrabc ones. A later intrabc block's DV predictor is
        // built by libaom's own row/col scans over `xd->mi[]`, which holds an
        // `MB_MODE_INFO` for EVERY block and steps by
        // `mi_size_wide/height[candidate->bsize]`; without this footprint a
        // non-intrabc neighbour reads as uncovered and the scan advances it
        // by one cell instead of by its real width.
        fctx.intrabc_mi_grid.with(|g| {
            if let Some((grid, _, _)) = g.borrow_mut().as_mut() {
                grid.cover_rect(mi_r, mi_c, w_mi, h_mi);
            }
        });
    }

    /// One var-tx / split-tx LEAF's own LUMA transform dims over its mi span.
    /// libaom `get_transform_size` (av1_loopfilter.c:207) applies the split
    /// override only `if ((plane == AOM_PLANE_Y) && ...)` -- chroma keeps
    /// `av1_get_max_uv_txsize(mbmi->bsize)`, the whole BLOCK's uv transform.
    /// Routing a leaf through [`Self::fill_lf_grid_rect`] recomputed the uv
    /// dims from the LEAF span and so invented chroma transform edges the
    /// deblocker then filtered (lane-golomb r6). `ref_grid`/`delta_lf_grid`
    /// are already correct from the block's own fill over the same span.
    pub(super) fn fill_lf_grid_leaf_luma(
        &mut self,
        at_mi: (usize, usize),
        w_mi: usize,
        h_mi: usize,
        tx_px: u8,
        tx_h_px: u8,
    ) {
        let (mi_r, mi_c) = at_mi;
        if crate::envflags::env_flag!("EC_TXGRID_TRACE") {
            eprintln!(
                "EC_LFLEAF mi_row={mi_r} mi_col={mi_c} w_mi={w_mi} h_mi={h_mi} tx_px={tx_px} tx_h_px={tx_h_px}"
            );
        }
        for rr in 0..h_mi {
            let start = (mi_r + rr) * self.skip_grid_cols_mi + mi_c;
            fill_span(&mut self.tx_grid, start, w_mi, tx_px);
            fill_span(&mut self.tx_h_grid, start, w_mi, tx_h_px);
        }
    }

    /// Marks every 4x4 mi cell a just-decoded coded block covers with its
    /// `skip` flag -- `at_mi`/`side_mi` are the block's own position and
    /// width/height in 4x4 mode-info units (mirroring [`Self::record_mi`]'s
    /// own units, not [`Self::record`]'s [`SUB`]-grid ones).
    pub(super) fn fill_skip_grid(&mut self, at_mi: (usize, usize), side_mi: usize, skip: bool) {
        self.fill_skip_grid_rect(at_mi, side_mi, side_mi, skip);
    }

    /// [`Self::fill_skip_grid`] with independent row/col extents (lane-partitions r1).
    pub(super) fn fill_skip_grid_rect(&mut self, at_mi: (usize, usize), w_mi: usize, h_mi: usize, skip: bool) {
        let (mi_r, mi_c) = at_mi;
        if skip && w_mi != h_mi {
            hit!(RECT_SKIP_BAND_HITS);
        }
        // lane-census: an INTRA frame's blocks never reach the inter
        // publisher, and this one is what every coded block writes.
        crate::census::block_intra_frame(w_mi, h_mi, skip);
        for rr in 0..h_mi {
            let start = (mi_r + rr) * self.skip_grid_cols_mi + mi_c;
            fill_span(
                &mut self.blk_grid,
                start,
                w_mi,
                BlkCell {
                    skip,
                    written: true,
                    org: [mi_r as u16, mi_c as u16],
                    dim: [w_mi as u8, h_mi as u8],
                },
            );
        }
    }
    /// Publishes one coded block's `is_inter_block` over EVERY 4x4 mi cell it
    /// covers -- lane-av1txctxband. libaom's `setup_mbs`
    /// (`decodeframe.c:355-361`) writes the block's own `MB_MODE_INFO`
    /// pointer into all of them, so `xd->mi[-xd->mi_stride]` /
    /// `xd->mi[-1]` name the block COVERING the cell, not "the last block
    /// written in this column" ([`Self::above_inter`] /
    /// [`Self::left_inter`], which is a per-column band and therefore stale
    /// for any block below the last writer in its column).
    /// Separate from [`Self::fill_skip_grid_rect`] because the two publishers
    /// run in BOTH orders at different call sites; both write disjoint fields,
    /// so the order does not matter. The value read is libaom's
    /// `is_inter_block` (`blockd.h:372-374`): an INTRABC block counts.
    pub(super) fn mark_inter_grid_rect(
        &mut self,
        at_mi: (usize, usize),
        w_mi: usize,
        h_mi: usize,
        is_inter: bool,
    ) {
        for rr in 0..h_mi {
            let start = (at_mi.0 + rr) * self.skip_grid_cols_mi + at_mi.1;
            fill_span(&mut self.inter_grid, start, w_mi, is_inter);
        }
    }

    /// `is_inter_block` of the coded block covering the mi cell
    /// `(mi_r, mi_c)`, or `false` for a cell no block has written yet (a tile
    /// border or a not-yet-decoded cell) -- lane-av1txctxband. The caller's
    /// `up_available` / `left_available` guard already excludes the borders.
    pub(super) fn inter_at(&self, mi_r: usize, mi_c: usize) -> bool {
        self.inter_grid
            .get(mi_r * self.skip_grid_cols_mi + mi_c)
            .copied()
            .unwrap_or(false)
    }

    /// The covering block's own `(block_size_wide, block_size_high)` in
    /// pixels at the mi cell `(mi_r, mi_c)`, from [`Self::blk_grid`]'s `dim`
    /// (the published mi extent) scaled by [`MI`] -- lane-av1txctxband. This
    /// is libaom's `block_size_wide[above_mbmi->bsize]` /
    /// `block_size_high[left_mbmi->bsize]`, read at the abutting CELL rather
    /// than from the per-column [`Self::above_side_mi`] /
    /// [`Self::left_side_mi`] band.
    pub(super) fn blk_side_at(&self, mi_r: usize, mi_c: usize) -> (usize, usize) {
        self.blk_grid
            .get(mi_r * self.skip_grid_cols_mi + mi_c)
            .filter(|c| c.written)
            .map_or((0, 0), |c| (c.dim[0] as usize * MI, c.dim[1] as usize * MI))
    }

    /// Whether the 8x8 luma block at mi position `(mi_r, mi_c)` (2x2 mi
    /// cells) was `skip_txfm` -- [`apply_cdef`]'s dlist membership test.
    /// Spec/libaom `is_8x8_block_skip` requires *every* one of the four 4x4
    /// mi cells the 8x8 covers to be skip (a smaller-than-8x8 partition can
    /// straddle the region with a mix of skip/non-skip blocks).
    /// A single 4x4 mi cell's own `skip` flag, unaggregated (unlike
    /// [`Self::is_skip_txfm`]'s 8x8 all-four-cells rule) -- what the loop
    /// filter's `curr_skipped`/`pv_skip_txfm` (spec 7.14.2) want, since a
    /// filtered edge's two sides are each exactly one coded block wide.
    pub(super) fn skip_at(&self, mi_r: usize, mi_c: usize) -> bool {
        self.blk_grid
            .get(mi_r * self.skip_grid_cols_mi + mi_c)
            .is_some_and(|c| c.skip)
    }

    /// The `skip_txfm` context of a block whose top-left mi cell is
    /// `(mi_r, mi_c)`: libaom `av1_get_skip_txfm_context`
    /// (`pred_common.h:175-181`) is `above_mi->skip_txfm + left_mi->skip_txfm`,
    /// with an absent neighbour contributing 0. The mi grid is per tile, so
    /// row/column 0 is a tile edge and has no neighbour on that axis.
    pub(super) fn skip_txfm_ctx(&self, mi_r: usize, mi_c: usize) -> usize {
        usize::from(mi_r > self.tile_row0_mi && self.skip_at(mi_r - 1, mi_c))
            + usize::from(mi_c > self.tile_col0_mi && self.skip_at(mi_r, mi_c - 1))
    }

    pub(super) fn is_skip_txfm(&self, mi_r: usize, mi_c: usize) -> bool {
        (0..2).all(|rr| {
            (0..2).all(|cc| {
                self.blk_grid
                    .get((mi_r + rr) * self.skip_grid_cols_mi + (mi_c + cc))
                    .is_some_and(|c| c.skip)
            })
        })
    }

    pub(super) fn start_row(&mut self) {
        self.left.iter_mut().for_each(|l| *l = Default::default());
        self.left_mode.iter_mut().for_each(|m| *m = DC_PRED);
        self.left_uv_mode.iter_mut().for_each(|m| *m = DC_PRED);
        self.left_side
            .iter_mut()
            .for_each(|s| *s = NO_NEIGHBOUR_SIDE);
        self.left_side_mi
            .iter_mut()
            .for_each(|s| *s = NO_NEIGHBOUR_SIDE as u8);
        self.left_skip.iter_mut().for_each(|s| *s = false);
        self.left_skip_mode.iter_mut().for_each(|s| *s = false);
        self.left_inter.iter_mut().for_each(|i| *i = false);
        self.left_ref.iter_mut().for_each(|r| *r = -1);
        self.left_ref1.iter_mut().for_each(|r| *r = None);
        self.left_comp_group_idx.iter_mut().for_each(|r| *r = 0);
        self.left_compound_idx.iter_mut().for_each(|r| *r = 0);
        self.left_filter.iter_mut().for_each(|f| *f = [3u8; 2]);
        // lane-sub8 r6: mi-granular row maps reset with the rest of the left
        // context (a tile/SB-row boundary has no left neighbour).
        self.sub8_mode_row
            .iter_mut()
            .for_each(|s| *s = (u16::MAX, 0));
        self.uv_mode_row.iter_mut().for_each(|s| *s = (u16::MAX, 0));
        // lane-tiles r11 (COMMON's NEIGHBOUR MAPS rule): the palette side bands
        // are per-mi maps like every other one here, so they reset with the
        // row too -- the left guard above is what actually stops a cross-tile
        // read, this keeps a stale colour out of the arrays at all.
        self.left_palette_size.iter_mut().for_each(|s| *s = 0);
        self.left_palette_colors
            .iter_mut()
            .for_each(|c| *c = [0u16; 8]);
        self.left_palette_uv_size.iter_mut().for_each(|s| *s = 0);
        self.left_palette_uv_colors
            .iter_mut()
            .for_each(|c| *c = [0u16; 8]);
        // lane-mergefix r2: `av1_zero_left_context` memsets
        // `left_txfm_context_buffer` to `tx_size_high[TX_SIZES_LARGEST]` at
        // every superblock row of every tile -- the var-tx left context does
        // NOT carry over a tile boundary (its `txfm_partition` ctx read the
        // previous tile column's last block otherwise).
        self.left_txfm.iter_mut().for_each(|t| *t = TXFM_CTX_INIT);
    }

    /// Records a block's `skip`/`is_inter`/`interp_filter` state for the next
    /// block that reads it as a neighbour -- [`crate::tile`]'s
    /// `record_inter`. `filter` is `[3, 3]` for an intra block (spec
    /// `get_ref_filter_type`'s "not this ref frame" case reads the same as
    /// "no neighbour"). `ref_frame` is `-1` for an intra block, else the
    /// `1..=7` reference it coded (lane-av1refs's `single_ref_p*_ctx` needs
    /// the real value, not just `is_inter`).
    #[allow(dead_code)] // read only from the `#[cfg(test)]` gates
    // Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn record_inter(
        &mut self,
        at: (usize, usize),
        side: usize,
        skip: bool,
        is_inter: bool,
        ref_frame: i8,
        filter: [u8; 2],
        skip_mode: bool,
    ) {
        self.record_inter_rect(at, side, side, skip, is_inter, ref_frame, filter, skip_mode);
    }

    /// [`Self::record_inter`] with independent above (`w_sub`, [`SUB`]-grid
    /// columns) / left (`h_sub`, [`SUB`]-grid rows) extents -- lane-partitions
    /// r1: a true rectangular strip's above-context span (its own width) and
    /// left-context span (its own height) differ, unlike every caller before
    /// this round (class neighbour-votes-all-its-fields).
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)] // read only from the `#[cfg(test)]` gates
    pub(super) fn record_inter_rect(
        &mut self,
        at: (usize, usize),
        w_sub: usize,
        h_sub: usize,
        skip: bool,
        is_inter: bool,
        ref_frame: i8,
        filter: [u8; 2],
        skip_mode: bool,
    ) {
        let (r, c) = at;
        self.record_inter_rect_mi(
            (r * (SUB / MI), c * (SUB / MI)),
            w_sub / MI,
            h_sub / MI,
            skip,
            is_inter,
            ref_frame,
            filter,
            skip_mode,
        );
    }

    /// [`Self::record_inter_rect`] in mi (4px) units -- lane-inter8 r2: an
    /// 8x8 leaf covers 2x2 mi, half a [`SUB`] cell in each direction, so the
    /// side bands are written (and read) per mi. Every [`SUB`]-unit caller
    /// routes through the wrapper above.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn record_inter_rect_mi(
        &mut self,
        at_mi: (usize, usize),
        w_mi: usize,
        h_mi: usize,
        skip: bool,
        is_inter: bool,
        ref_frame: i8,
        filter: [u8; 2],
        skip_mode: bool,
    ) {
        let (r, c) = at_mi;
        crate::census::block(w_mi, h_mi, is_inter, ref_frame, skip);
        // lane-t900 r8 (class new-map-ignores-tile-edge's twin: a map NOBODY
        // writes for a whole block class): an INTER block left the luma mode
        // map untouched, so a later intra block's `get_intra_edge_filter_type`
        // (spec 7.11.2, libaom `reconintra.h`'s `is_smooth`) fell back to the
        // coarse SUB-row band, which still held a SMOOTH mode from an earlier
        // block in that pixel row -- the directional edge filter then ran at
        // strength 3 instead of 2. libaom reads `mbmi->mode` of the neighbour,
        // which for an inter block is an INTER mode (>= NEARESTMV) and is
        // never smooth; DC_PRED is this map's equivalent (the sub-8x8 groups
        // already stamped it).
        // The chroma twin: `is_smooth(mbmi, plane > 0)` returns 0 outright for
        // an inter neighbour ("uv_mode is not set for inter blocks"), so the
        // uv map must read DC_PRED over an inter block's cells too.
        if is_inter {
            self.record_mode_mi(r, c, w_mi, h_mi, DC_PRED);
            self.record_uv_mode_mi(r, c, w_mi, h_mi, DC_PRED);
        }
        // lane-av1txctxband: the per-CELL `is_inter_block` the per-column
        // `above_inter` band stands in for, at libaom's own granularity.
        self.mark_inter_grid_rect(at_mi, w_mi, h_mi, is_inter);
        let altref = is_inter && ref_frame == crate::mvstack::ALTREF_FRAME;
        self.above_skip[c..c + w_mi].fill(skip);
        self.above_skip_mode[c..c + w_mi].fill(skip_mode);
        self.above_inter[c..c + w_mi].fill(is_inter);
        self.above_ref[c..c + w_mi].fill(ref_frame);
        // The single-ref default: `record_compound_ctx_rect_mi` overwrites
        // this with the block's real second reference for a compound block,
        // called right after this one (lane-publish2 audit).
        self.above_ref1[c..c + w_mi].fill(None);
        // libaom `get_comp_group_idx_context`/`get_comp_index_context`'s
        // single-ref special case: `ref_frame[0] == ALTREF_FRAME` reads
        // as `3`/`1` even though this block has no second reference.
        // `record_compound_ctx` overwrites this with the real bit for an
        // actual compound block's cells, called right after this one.
        self.above_comp_group_idx[c..c + w_mi].fill(if altref { 3 } else { 0 });
        self.above_compound_idx[c..c + w_mi].fill(u8::from(altref));
        self.above_filter[c..c + w_mi].fill(filter);
        self.left_skip[r..r + h_mi].fill(skip);
        self.left_skip_mode[r..r + h_mi].fill(skip_mode);
        self.left_inter[r..r + h_mi].fill(is_inter);
        self.left_ref[r..r + h_mi].fill(ref_frame);
        self.left_ref1[r..r + h_mi].fill(None);
        self.left_comp_group_idx[r..r + h_mi].fill(if altref { 3 } else { 0 });
        self.left_compound_idx[r..r + h_mi].fill(u8::from(altref));
        self.left_filter[r..r + h_mi].fill(filter);
    }

    /// Overwrites [`Self::record_inter`]'s ALTREF-special-case default with
    /// a real compound block's own `comp_group_idx`/`compound_idx` bits
    /// (libaom `has_second_ref(mbmi)` branch of `get_comp_group_idx_context`/
    /// `get_comp_index_context`) -- called right after `record_inter` for
    /// the same `at`/`side`, only on the `COMPOUND_REFERENCE` path. Also
    /// stamps `above_ref1`/`left_ref1` with the block's real second
    /// reference, which `record_inter` always clears to `None` (lane-av1comp:
    /// this is the one caller that actually knows it) -- so the next block's
    /// [`NeighbourRef`](crate::mvstack::NeighbourRef) sees a real compound
    /// neighbour instead of `record_inter`'s single-ref default.
    #[allow(dead_code)] // read only from the `#[cfg(test)]` gates
    pub(super) fn record_compound_ctx(
        &mut self,
        at: (usize, usize),
        side: usize,
        ref1: i8,
        comp_group_idx: u8,
        compound_idx: u8,
    ) {
        self.record_compound_ctx_rect(at, side, side, ref1, comp_group_idx, compound_idx);
    }

    /// [`Self::record_compound_ctx`] with independent above/left extents,
    /// mirroring [`Self::record_inter_rect`] (lane-partitions r1).
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)] // read only from the `#[cfg(test)]` gates
    pub(super) fn record_compound_ctx_rect(
        &mut self,
        at: (usize, usize),
        w_sub: usize,
        h_sub: usize,
        ref1: i8,
        comp_group_idx: u8,
        compound_idx: u8,
    ) {
        let (r, c) = at;
        self.record_compound_ctx_rect_mi(
            (r * (SUB / MI), c * (SUB / MI)),
            w_sub / MI,
            h_sub / MI,
            ref1,
            comp_group_idx,
            compound_idx,
        );
    }

    /// [`Self::record_compound_ctx_rect`] in mi units (lane-inter8 r2, see
    /// [`Self::record_inter_rect_mi`]).
    pub(super) fn record_compound_ctx_rect_mi(
        &mut self,
        at_mi: (usize, usize),
        w_mi: usize,
        h_mi: usize,
        ref1: i8,
        comp_group_idx: u8,
        compound_idx: u8,
    ) {
        let (r, c) = at_mi;
        crate::census::compound();
        for cell in 0..w_mi {
            self.above_ref1[c + cell] = Some(ref1);
            self.above_comp_group_idx[c + cell] = comp_group_idx;
            self.above_compound_idx[c + cell] = compound_idx;
        }
        for cell in 0..h_mi {
            self.left_ref1[r + cell] = Some(ref1);
            self.left_comp_group_idx[r + cell] = comp_group_idx;
            self.left_compound_idx[r + cell] = compound_idx;
        }
    }

    /// The context of a block's partition symbol: delegates to the mi-precise
    /// reader (same pattern as `around`/`around_mi`) -- `above_side`/
    /// `left_side` are only ever advanced in whole-[`SUB`] steps by
    /// [`Self::record`], so a leaf8's `record_mi` (lane-av1-rect) -- which
    /// only touches the finer mi arrays -- leaves them stale for the *next*
    /// sibling's own partition symbol.
    pub(super) fn partition_ctx(&self, at: (usize, usize), side: usize) -> usize {
        let (r, c) = at;
        self.partition_ctx_mi((r * (SUB / MI), c * (SUB / MI)), side)
    }

    /// [`Self::partition_ctx`] at mi granularity, for an 8x8 leaf of a
    /// straddling 16x16 block: reads the finer `above_side_mi`/`left_side_mi`
    /// arrays so the second leaf sees the first leaf's own 8x8 side rather
    /// than the enclosing 16x16 slot's stale, shared state.
    pub(super) fn partition_ctx_mi(&self, (mi_r, mi_c): (usize, usize), side: usize) -> usize {
        2 * usize::from(usize::from(self.left_side_mi[mi_r]) * 2 <= side)
            + usize::from(usize::from(self.above_side_mi[mi_c]) * 2 <= side)
    }

    /// The gathered coded/DC-sign state of the blocks above and to the left
    /// of a block `side` samples across, per plane.
    pub(super) fn around(&self, (r, c): (usize, usize), side: usize) -> [(bool, bool, i32); 3] {
        self.around_mi((r * (SUB / MI), c * (SUB / MI)), side)
    }

    /// [`Self::around`] taking the block's position directly in 4x4 mode-info
    /// units, for the same reason [`Self::record_mi`] does.
    pub(super) fn around_mi(&self, (mi_r, mi_c): (usize, usize), side: usize) -> [(bool, bool, i32); 3] {
        let side_mi = side / MI;
        // One pass over the cells accumulating all three planes, not one pass
        // per plane: the three planes of a cell live in the same
        // `[Neighbour; 3]` (6 bytes), so the per-plane form loaded the same
        // cache lines three times (lane-publish). Clamp each edge to the true
        // frame extent: a block at the right/bottom edge may be padded beyond
        // `mi_cols`/`mi_rows`, and libaom's unavailable context is zero there.
        let above_mi = side_mi.min(self.mi_cols.saturating_sub(mi_c));
        let left_mi = side_mi.min(self.mi_rows.saturating_sub(mi_r));
        let mut out = [(false, false, 0i32); 3];
        for cell in 0..above_mi {
            let above = &self.above[mi_c + cell];
            for (plane, o) in out.iter_mut().enumerate() {
                o.0 |= above[plane].level != 0;
                o.2 += dc_vote(above[plane].dc);
            }
        }
        for cell in 0..left_mi {
            let left = &self.left[mi_r + cell];
            for (plane, o) in out.iter_mut().enumerate() {
                o.1 |= left[plane].level != 0;
                o.2 += dc_vote(left[plane].dc);
            }
        }
        out
    }

    /// [`Self::around_mi_rect`] for the CHROMA planes of a 4:2:2 frame
    /// (lane-av1-422f): libaom `get_txb_ctx_general` reads its above votes
    /// over `txb_w_unit` CHROMA 4-px cells, and at ss_x 1 one chroma cell
    /// spans TWO luma mi columns. Both cells of a column carry the covering
    /// unit's whole-unit dc sign (the same value libaom stores), so the
    /// per-luma-mi sum counts each chroma column twice -- and
    /// `dc_sign_ctx`'s signum flips whenever the above and left votes have
    /// opposite signs with equal magnitude (measured at mi(4,4) plane 1 of
    /// the t422 fixture: ours read `dc_sign_cdf[1]`, the oracle row 0).
    /// Sampling every second above cell counts each chroma column once,
    /// which IS libaom's sum. The left extent is unsubsquared at ss_y 0, so
    /// its per-mi cells already line up one-to-one. The level booleans are
    /// ORs over duplicated cells, so the sampling cannot change them.
    pub(super) fn around_mi_422_chroma(
        &self,
        (mi_r, mi_c): (usize, usize),
        w: usize,
        h: usize,
    ) -> [(bool, bool, i32); 3] {
        let (w_mi, h_mi) = (w / MI, h / MI);
        let above_mi = w_mi.min(self.mi_cols.saturating_sub(mi_c));
        let left_mi = h_mi.min(self.mi_rows.saturating_sub(mi_r));
        let mut out = [(false, false, 0i32); 3];
        for cell in (0..above_mi).step_by(2) {
            let above = &self.above[mi_c + cell];
            for (plane, o) in out.iter_mut().enumerate() {
                o.0 |= above[plane].level != 0;
                o.2 += dc_vote(above[plane].dc);
            }
        }
        for cell in 0..left_mi {
            let left = &self.left[mi_r + cell];
            for (plane, o) in out.iter_mut().enumerate() {
                o.1 |= left[plane].level != 0;
                o.2 += dc_vote(left[plane].dc);
            }
        }
        if crate::envflags::env_flag!("EC_DCDUMP") {
            // lane-av1422mi24: same dump [`Self::around_mi_rect`] prints, so
            // a rerouted 4:2:2 chroma gather stays visible under EC_DCDUMP=1.
            // The above rows are the SAMPLED (every-second) cells.
            for (plane, o) in out.iter().enumerate() {
                let vote = o.2;
                let ab: Vec<String> = (0..above_mi)
                    .step_by(2)
                    .map(|k| {
                        format!(
                            "{:?}/{}",
                            self.above[mi_c + k][plane].dc,
                            self.above[mi_c + k][plane].level
                        )
                    })
                    .collect();
                let lf: Vec<String> = (0..left_mi)
                    .map(|k| {
                        format!(
                            "{:?}/{}",
                            self.left[mi_r + k][plane].dc,
                            self.left[mi_r + k][plane].level
                        )
                    })
                    .collect();
                eprintln!(
                    "EC_DCDUMP422 mi=({mi_r},{mi_c}) plane={plane} wh=({w},{h}) vote={vote} above=[{}] left=[{}]",
                    ab.join(","),
                    lf.join(",")
                );
            }
        }
        out
    }

    /// [`Self::around`] with independent above (`w`)/left (`h`) extents, in
    /// pixels -- lane-rectwire, mirrors [`Self::record_rect`]'s asymmetric
    /// extent pattern for a true rectangular strip's own coefficient context.
    pub(super) fn around_rect(&self, (r, c): (usize, usize), w: usize, h: usize) -> [(bool, bool, i32); 3] {
        self.around_mi_rect((r * (SUB / MI), c * (SUB / MI)), w, h)
    }

    /// [`Self::around_rect`] taking the block's position directly in 4x4
    /// mode-info units, mirroring [`Self::around_mi`].
    pub(super) fn around_mi_rect(
        &self,
        (mi_r, mi_c): (usize, usize),
        w: usize,
        h: usize,
    ) -> [(bool, bool, i32); 3] {
        let (w_mi, h_mi) = (w / MI, h / MI);
        let above_mi = w_mi.min(self.mi_cols.saturating_sub(mi_c));
        let left_mi = h_mi.min(self.mi_rows.saturating_sub(mi_r));
        // [`Self::around_mi`]'s single pass, with the two extents split.
        let mut out = [(false, false, 0i32); 3];
        for cell in 0..above_mi {
            let above = &self.above[mi_c + cell];
            for (plane, o) in out.iter_mut().enumerate() {
                o.0 |= above[plane].level != 0;
                o.2 += dc_vote(above[plane].dc);
            }
        }
        for cell in 0..left_mi {
            let left = &self.left[mi_r + cell];
            for (plane, o) in out.iter_mut().enumerate() {
                o.1 |= left[plane].level != 0;
                o.2 += dc_vote(left[plane].dc);
            }
        }
        if crate::envflags::env_flag!("EC_DCDUMP") {
            for (plane, o) in out.iter().enumerate() {
                let vote = o.2;
                let ab: Vec<String> = (0..above_mi)
                    .map(|k| {
                        format!(
                            "{:?}/{}",
                            self.above[mi_c + k][plane].dc,
                            self.above[mi_c + k][plane].level
                        )
                    })
                    .collect();
                let lf: Vec<String> = (0..left_mi)
                    .map(|k| {
                        format!(
                            "{:?}/{}",
                            self.left[mi_r + k][plane].dc,
                            self.left[mi_r + k][plane].level
                        )
                    })
                    .collect();
                eprintln!(
                    "EC_DCDUMP mi=({mi_r},{mi_c}) plane={plane} wh=({w},{h}) vote={vote} above=[{}] left=[{}]",
                    ab.join(","),
                    lf.join(",")
                );
            }
        }
        out
    }

    /// The luma `txb_skip_ctx` of a transform unit smaller than its own block
    /// (spec `get_txb_ctx_general`, plane 0, `plane_bsize != tx_size`): the
    /// above/left neighbours' magnitude tiers, OR-reduced over the unit's own
    /// span and each clamped to 4, indexed into [`SKIP_CONTEXTS`]. Unlike
    /// [`Self::around_mi`]'s plain coded/uncoded bit, this needs the real
    /// cumulative level -- a lone-TU block never calls this (its `txb_skip_ctx`
    /// is always 0, spec's `plane_bsize == tx_size` branch).
    pub(super) fn luma_skip_ctx(&self, at_mi: (usize, usize), side_mi: usize) -> usize {
        self.luma_skip_ctx_rect(at_mi, side_mi, side_mi)
    }

    /// [`Self::luma_skip_ctx`] with the unit's own width and height in mi
    /// units (lane-rectsplitx r1) -- `get_txb_ctx_general` ORs the above
    /// context over `tx_size_wide` and the left one over `tx_size_high`.
    pub(super) fn luma_skip_ctx_rect(&self, (mi_r, mi_c): (usize, usize), w_mi: usize, h_mi: usize) -> usize {
        let side_mi = w_mi;
        let above_mi = w_mi.min(self.mi_cols.saturating_sub(mi_c));
        let left_mi = h_mi.min(self.mi_rows.saturating_sub(mi_r));
        let mut top = 0u8;
        let mut left = 0u8;
        for cell in 0..above_mi {
            top |= self.above[mi_c + cell][0].level;
        }
        for cell in 0..left_mi {
            left |= self.left[mi_r + cell][0].level;
        }
        let ctx = SKIP_CONTEXTS[(top as usize).min(4)][(left as usize).min(4)];
        if crate::envflags::env_flag!("EC_ECDUMP") {
            let ab: Vec<String> = (0..above_mi)
                .map(|k| self.above[mi_c + k][0].level.to_string())
                .collect();
            let lf: Vec<String> = (0..left_mi)
                .map(|k| self.left[mi_r + k][0].level.to_string())
                .collect();
            // lane-av1mix: the last few msac register states, so the
            // byte-position of the `all_zero` read can be matched against
            // aomdec's `EC=(value,rng,bytepos)` dump (decodetxb.c) without a
            // second run.
            let recent = crate::msac::symtrace::ecdump_recent(4)
                .iter()
                .map(|(_low, value, rng, bytepos)| format!("({value},{rng},{bytepos})"))
                .collect::<Vec<_>>()
                .join(" ");
            eprintln!(
                "EC_ECDUMP plane=0 mi=({mi_r},{mi_c}) wh=({w_mi},{h_mi}) ctx={ctx} above=[{},] left=[{},] recentEC=[{}]",
                ab.join(","),
                lf.join(","),
                recent
            );
        }
        if crate::envflags::env_flag!("EC_AV1_TRACE") {
            eprintln!(
                "TRACE luma_skip_ctx mi=({mi_r},{mi_c}) side_mi={side_mi} top={top} left={left} ctx={ctx} band={:?}",
                self.left.iter().map(|e| e[0].level).collect::<Vec<_>>()
            );
        }
        ctx
    }

    /// Writes one coded block into every cell it covers, on both grids: the
    /// mode/side arrays up to `side`'s own width, and the coefficient-context
    /// arrays up to the true frame edge — the units past it are left at their
    /// default (uncoded), even mid-cell, exactly as [`crate::tile`]'s own
    /// `record` leaves them (spec `av1_set_entropy_contexts`, which clamps to
    /// `blocks_wide`/`blocks_high` derived from the true `mi_cols`/`mi_rows`,
    /// not from this block's own side).
    pub(super) fn record(
        &mut self,
        at: (usize, usize),
        side: usize,
        mode: usize,
        uv_mode: usize,
        grids: &[&[i32]; 3],
    ) {
        self.record_rect(at, side, side, mode, uv_mode, grids);
    }

    /// [`Self::record`] with independent above (`w`)/left (`h`) extents, in
    /// pixels -- lane-partitions r1: `above_side`/`left_side` (read back by
    /// [`Self::partition_ctx`]) are the neighbour's own width/height per the
    /// spec's per-edge partition-context rule, so a true rectangular block
    /// naturally wants two different values here rather than one.
    /// Writes this block's luma `mode` into the mi-granular
    /// [`Self::sub8_mode_col`]/[`Self::sub8_mode_row`] maps, keyed by the mi
    /// position a *later* neighbour would read it from: the block's LAST mi
    /// row for every column it spans (what the block below reads via
    /// `mi(row-1, col)`) and its LAST mi column for every row (what the
    /// block to the right reads via `mi(row, col-1)`). Only sub-8x8 leaves
    /// read these back, and only when the recorded position is exactly their
    /// own neighbour -- see [`Self::sub8_mode_col`]'s doc.
    pub(super) fn record_mode_mi(&mut self, mi_r: usize, mi_c: usize, mi_w: usize, mi_h: usize, mode: usize) {
        let (last_r, last_c) = ((mi_r + mi_h - 1) as u16, (mi_c + mi_w - 1) as u16);
        let mode = mode as u8;
        for cell in 0..mi_w {
            if let Some(slot) = self.sub8_mode_col.get_mut(mi_c + cell) {
                *slot = (last_r, mode);
            }
        }
        for cell in 0..mi_h {
            if let Some(slot) = self.sub8_mode_row.get_mut(mi_r + cell) {
                *slot = (last_c, mode);
            }
        }
    }

    /// The mi-exact above/left neighbour luma mode for a block whose top-left
    /// mi is `(mi_r, mi_c)`, or `None` when no block has recorded that exact
    /// mi position (then the coarse [`SUB`]-grid slot is right by
    /// construction). See [`Self::record_mode_mi`].
    pub(super) fn mode_above_mi(&self, mi_r: usize, mi_c: usize) -> Option<usize> {
        match self.sub8_mode_col.get(mi_c) {
            Some(&(row, m)) if mi_r > self.tile_row0_mi && usize::from(row) == mi_r - 1 => {
                Some(usize::from(m))
            }
            _ => None,
        }
    }

    pub(super) fn mode_left_mi(&self, mi_r: usize, mi_c: usize) -> Option<usize> {
        match self.sub8_mode_row.get(mi_r) {
            Some(&(col, m)) if mi_c > self.tile_col0_mi && usize::from(col) == mi_c - 1 => {
                Some(usize::from(m))
            }
            _ => None,
        }
    }

    /// [`Self::record_mode_mi`] for the block's `uv_mode`, into
    /// [`Self::uv_mode_col`]/[`Self::uv_mode_row`].
    pub(super) fn record_uv_mode_mi(
        &mut self,
        mi_r: usize,
        mi_c: usize,
        mi_w: usize,
        mi_h: usize,
        uv_mode: usize,
    ) {
        let (last_r, last_c) = (mi_r + mi_h - 1, mi_c + mi_w - 1);
        fill_span(
            &mut self.uv_mode_col,
            mi_c,
            mi_w,
            (last_r as u16, uv_mode as u8),
        );
        fill_span(
            &mut self.uv_mode_row,
            mi_r,
            mi_h,
            (last_c as u16, uv_mode as u8),
        );
        // libaom writes one `MB_MODE_INFO` pointer into every mi cell a block
        // covers, so the chroma neighbour read below is a plain grid read.
        let stride = self.uv_grid_cols;
        let n = stride.saturating_sub(mi_c).min(last_c + 1 - mi_c);
        for row in mi_r..=last_r {
            fill_span(
                &mut self.uv_mode_grid,
                row * stride + mi_c,
                n,
                uv_mode as u8,
            );
        }
        // lane-av1leftwit: stamp the writing block's own mi over its extent,
        // the same cells and the same clipping as `uv_mode_grid` above.
        let owner = UV_OWNER_EPOCH.with(|c| c.get()) | ((mi_r as u32) << 12) | mi_c as u32;
        for row in mi_r..=last_r {
            fill_span(&mut self.uv_owner, row * stride + mi_c, n, owner);
        }
        if crate::envflags::env_flag!("EC_LEFTSSY") {
            eprintln!("EC_LEAF mi=({mi_r},{mi_c}) wh=({mi_w},{mi_h}) uv_mode={uv_mode}");
        }
    }

    /// Whether either mi-exact chroma neighbour of the block at `(mi_r, mi_c)`
    /// coded a smooth `uv_mode` -- libaom's `intra_edge_filter_type` for
    /// planes 1/2. Falls back to the coarse [`SUB`] slot on whichever axis no
    /// block has recorded the exact neighbouring mi.
    pub(super) fn smooth_uv_neighbour(
        &self,
        mi_r: usize,
        mi_c: usize,
        r: usize,
        c: usize,
        fctx: &crate::decode::FrameCtx,
    ) -> bool {
        // lane-kf900 r2: libaom reads the chroma neighbours off the CHROMA
        // REFERENCE block, not off the mi that carries the chroma syntax.
        // `set_mi_row_col` (`blockd.h`) takes
        // `base_mi = &xd->mi[-(mi_row & ss_y) * stride - (mi_col & ss_x)]` and
        // only then `chroma_above_mbmi = base_mi[-stride]` /
        // `chroma_left_mbmi = base_mi[-1]`. A sub-8 pair codes its chroma at
        // the ODD mi, so without the snap we looked one mi row/column too far
        // down/right -- the mi-exact slot never matched and the coarse
        // fallback answered instead. Measured on a 4K 10-bit key frame, chroma
        // 8x4 at (688, 100) = mi (51, 344): libaom's
        // `get_intra_edge_filter_type` is 1 (the chroma block above it is
        // `UV_SMOOTH_PRED`), ours was 0, which flips
        // `av1_use_intra_edge_upsample(8, 4, -6, type)` (`blk_wh == 12`:
        // false for type 1, TRUE for type 0) and upsampled an edge libaom
        // leaves alone -- +1 on rows 2/3 of that block and every chroma block
        // predicting from it (class `context-read-from-one-cell`).
        // lane-av1-444: the snap is the SEQUENCE's subsampling, hardcoded
        // `& !1` before -- at subsampling 0/0 libaom's `mi_row & 0` snaps
        // nothing, and the hardcoded snap read a 4x4 leaf's smooth
        // neighbour two mi away (frame 0 of `av1-profile1-444.ivf`, leaf
        // mi(5,15): left neighbour SMOOTH_V at mi(5,14), read at mi(4,13)).
        let snap_y = (1usize << ss_y(fctx)) - 1;
        let snap_x = (1usize << ss_x(fctx)) - 1;
        // The snap IS libaom's `base_mi` de-offsetting (`set_mi_row_col`,
        // `av1_common_int.h:1395-1412`): `base_mi` is the block's own mi moved
        // by `-(mi_row & ss_y) * stride - (mi_col & ss_x)`. The helper applies
        // the two chroma-reference offsets on top of it -- see its own comment
        // for which axis each one moves.
        self.smooth_uv_neighbour_unsnapped(
            mi_r & !snap_y,
            mi_c & !snap_x,
            r,
            c,
            ss_x(fctx),
            ss_y(fctx),
            true,
        )
    }

    /// [`Self::smooth_uv_neighbour`] without the chroma-reference snap, i.e.
    /// read off the mi passed in exactly. Only the 16x4/4x16 pair witness
    /// counter wants this: it measures how often the snap CHANGES the answer,
    /// which is what the lane-kf1200/kf900 chroma-reference fix buys. Every
    /// decode path uses the snapping form.
    /// `decode_path` is `true` only for the snapping form every decode call
    /// site uses; the 16x4/4x16 pair witness's mirror passes `false`, so the
    /// `+ss_y` counters measure a read that actually filters a chroma edge and
    /// not the counter-only one.
    ///
    /// lane-av1leftwit: the mirror's justification used to be "no committed
    /// fixture reaches the shape on the decode path", and that was false --
    /// `UV_LEFT_1MI_SPLIT_HITS` fires on five of them. The flag stays (a
    /// counter-only mirror at an ODD mi row is still not a decode-path read,
    /// and its result feeds only `RECT4_16_UV_PAIR_FILT_HITS`), but it is no
    /// longer evidence about the `+ss_y` term, and nothing may cite it as such.
    /// The decode path's own reach is `uv_left_1mi_split_hits()`.
    // Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn smooth_uv_neighbour_unsnapped(
        &self,
        mi_r: usize,
        mi_c: usize,
        r: usize,
        c: usize,
        ss_x: usize,
        ss_y: usize,
        decode_path: bool,
    ) -> bool {
        // lane-mtfix r1: availability is TILE-relative on BOTH axes and on the
        // coarse fallback too. `above_uv_mode` is a frame-wide [`SUB`]-grid
        // array that `start_tile` does not clear, so a block on a tile's first
        // mi row used to read the uv_mode of the block above it in the tile
        // ABOVE and filter its chroma edge at that neighbour's strength
        // (2x2-tile intra gate, seed 46: chroma-only +-1..2 over the 8x8 block
        // at chroma (120, 32), the top-right tile's first block).
        let have_above = mi_r > self.tile_row0_mi;
        let have_left = mi_c > self.tile_col0_mi;
        if self.tile_row0_mi > 0 && !have_above && self.above_uv_mode[c] != DC_PRED
            || self.tile_col0_mi > 0 && !have_left && self.left_uv_mode[r] != DC_PRED
        {
            hit!(UV_TILE_EDGE_SUPPRESSED_HITS);
        }
        // The mi grid is libaom's own read; the two maps below stay as the
        // fallback for cells no block has written (see [`Self::uv_mode_grid`]).
        let cell = |grid: &Vec<u8>, row: usize, col: usize| -> Option<usize> {
            (col < self.uv_grid_cols)
                .then(|| grid.get(row * self.uv_grid_cols + col).copied())
                .flatten()
                .filter(|&m| m != u8::MAX)
                .map(usize::from)
        };
        // libaom's two chroma-reference mi, from `set_mi_row_col`
        // (`av1_common_int.h:1400-1401`), with `mi_r`/`mi_c` already
        // de-offset by the caller's `(mi_row & ss_y)` / `(mi_col & ss_x)`
        // snap -- i.e. already libaom's `base_mi`:
        //
        //   chroma_above_mi = base_mi[-mi_stride + ss_x]   row -1, col +ss_x
        //   chroma_left_mi  = base_mi[ss_y*mi_stride - 1]  row +ss_y, col -1
        //
        // SHIPPED: the above COLUMN. It is `+ss_x` wherever `ss_x == 1` --
        // 4:2:0, 4:2:2 AND 4:4:0 all carry that column shift, and only 4:4:4
        // (`ss_x == 0`) leaves the read where it was. Measured: folding it in
        // leaves the 4:2:0 control byte-identical to `aomdec --rawvideo`; the
        // boolean it feeds is simply not exercised differently by that stream
        // at its blocks. One chroma column spans two luma mi columns, so the
        // chroma edge reaches into the right-hand luma block, and that is where
        // its reference lives.
        //
        // SHIPPED (lane-av1leftref): the left read's `+ss_y` ROW term -- the
        // last open item lane-av1422warp carried. libaom puts the left
        // reference one mi row DOWN at 4:2:0 and 4:4:0 (`ss_y == 1`) and on
        // its own row at 4:2:2 (`ss_y == 0`); this read its own row at every
        // subsampling. An earlier attempt applied the `ss_y` term to the WRONG
        // read (the above) and regressed the byte-exact 4:2:0 control, which
        // is what exposed the crossing. lane-av1leftwit CORRECTED the claim
        // this comment used to make: "six committed 4:2:0 fixtures reach it,
        // and on `reelb_ss600_key_frame.obu` the boolean flips" was never
        // measured. Measured over all 48 committed fixtures that decode
        // (`lanes/av1leftwit.report.md`), `reelb_ss600_key_frame.obu` reads
        // `mode_diff=0 smooth_diff=0 split1mi=0`, and exactly FIVE fixtures
        // put a 1-mi-tall left neighbour in front of a decode-path read at
        // all -- `gm_small_side_witness` (5), `reelc_sb128_inter_witness` (7),
        // `av112bit-{inter,compound,compound-masked}` (1 each) -- and in every
        // one of those reads the two blocks agree on `uv_mode`. So the SHAPE is
        // reachable (see `UV_LEFT_1MI_SPLIT_HITS`) and the term's VALUE
        // changing is what is still unwitnessed.
        let above_mi = (mi_r.saturating_sub(1), mi_c + ss_x);
        // lane-av1leftref: libaom's LEFT read carries the `+ss_y` ROW term --
        // `base_mi[ss_y * mi_stride - 1]` (`av1_common_int.h:1400-1401`) is
        // one mi row DOWN at 4:2:0 and 4:4:0 (`ss_y == 1`), and on its own
        // row at 4:2:2 (`ss_y == 0`) and 4:4:4. The read below used its own
        // row at every subsampling. The term is only observable when the two
        // cells hold DIFFERENT blocks, i.e. the left neighbour is 1 mi tall or
        // shorter -- then `(row, col-1)` and `(row+1, col-1)` are two blocks
        // and only libaom's is the chroma reference. Three counters measure
        // that, and the third is the one the other two need: a read can only
        // differ if the two cells are written by DIFFERENT blocks, which
        // `UV_LEFT_1MI_SPLIT_HITS` counts off the per-cell owner stamp
        // (`uv_owner`, below). A DIFF bucket that reads 0 while SPLIT reads 0
        // was never in contention; a DIFF bucket that reads 0 while SPLIT
        // reads > 0 is the term being unobservable on real material, not
        // unreachable. `..._MODE_DIFF_HITS` / `..._SMOOTH_DIFF_HITS` then say
        // whether the mode value, and the boolean this feeds
        // (`is_smooth_mode` -> `av1_get_intra_edge_filter_type` -> the chroma
        // edge filter), actually moved.
        let left_mi = (mi_r + ss_y, mi_c.saturating_sub(1));
        let read_left = |row: usize| -> usize {
            match self.uv_mode_row.get(row) {
                _ if have_left && let Some(m) = cell(&self.uv_mode_grid, row, left_mi.1) => m,
                Some(&(col, m)) if have_left && usize::from(col) == left_mi.1 => usize::from(m),
                _ if have_left => self.left_uv_mode[r],
                _ => DC_PRED,
            }
        };
        let above = match self.uv_mode_col.get(above_mi.1) {
            _ if have_above && let Some(m) = cell(&self.uv_mode_grid, above_mi.0, above_mi.1) => m,
            Some(&(row, m)) if have_above && usize::from(row) == above_mi.0 => usize::from(m),
            _ if have_above => self.above_uv_mode[c],
            _ => DC_PRED,
        };
        let left = read_left(left_mi.0);
        // lane-av1leftref: the gate's reach + load-bearing counters. `READS` is
        // how many chroma edge-filter-type reads took this path at all, so a
        // gate can tell "the arm never ran" from "the arm ran and the `+ss_y`
        // term did not change the answer". The two DIFF buckets compare
        // libaom's read cell against the pre-lane own-row cell; both read 0 on
        // every committed fixture and every one of the ~1000 encodes swept in
        // `lanes/av1leftref.report.md`, which is what makes the read change a
        // proven no-op on today's material rather than an unmeasured guess.
        if decode_path && have_left && ss_y > 0 {
            hit!(UV_LEFT_SS_Y_READS);
            // The pre-lane read, kept as the counter's own-row arm: it is what
            // this decoder answered before the `+ss_y` term landed, and
            // comparing against it is what proves the term is load-bearing.
            let own_row = read_left(mi_r);
            if own_row != left {
                hit!(UV_LEFT_SS_Y_MODE_DIFF_HITS);
            }
            if is_smooth_mode(own_row) != is_smooth_mode(left) {
                hit!(UV_LEFT_SS_Y_SMOOTH_DIFF_HITS);
            }
            // lane-av1leftwit: the geometry the `+ss_y` term needs. The two
            // cells are one left mi column apart, so they are two different
            // blocks exactly when the left neighbour is 1 mi tall or shorter.
            let col = left_mi.1;
            let epoch = UV_OWNER_EPOCH.with(|c| c.get()) & 0xff00_0000;
            let own = (col < self.uv_grid_cols)
                .then(|| self.uv_owner.get(mi_r * self.uv_grid_cols + col).copied())
                .flatten()
                .filter(|o| o & 0xff00_0000 == epoch)
                .unwrap_or(0);
            let down = self
                .uv_owner
                .get((mi_r + ss_y) * self.uv_grid_cols + col)
                .copied()
                .filter(|o| o & 0xff00_0000 == epoch)
                .unwrap_or(0);
            if own != 0 && down != 0 && own != down {
                hit!(UV_LEFT_1MI_SPLIT_HITS);
                if crate::envflags::env_flag!("EC_LEFTSSY") {
                    eprintln!(
                        "EC_LEFTSSY epoch={} mi=({mi_r},{mi_c}) left_col={col} \
                         owners=({},{})->({},{}) modes={own_row}/{left}",
                        epoch >> 24,
                        own >> 12 & 0xfff,
                        own & 0xfff,
                        down >> 12 & 0xfff,
                        down & 0xfff,
                    );
                }
            }
        }
        if have_above && cell(&self.uv_mode_grid, mi_r - 1, mi_c).is_some_and(|m| {
            !matches!(self.uv_mode_col.get(mi_c), Some(&(row, cm)) if usize::from(row) == mi_r - 1 && usize::from(cm) == m)
        }) || have_left && cell(&self.uv_mode_grid, left_mi.0, left_mi.1).is_some_and(|m| {
            !matches!(self.uv_mode_row.get(left_mi.0), Some(&(col, cm)) if usize::from(col) == left_mi.1 && usize::from(cm) == m)
        }) {
            hit!(UV_MODE_GRID_OVERRIDE_HITS);
        }
        // lane-cfl r1 counter: how often the mi-exact chroma neighbour differs
        // from the coarse [`SUB`] slot -- i.e. how often a chroma edge would
        // have been filtered at the wrong strength by the coarse map alone.
        if above != self.above_uv_mode[c] || left != self.left_uv_mode[r] {
            hit!(UV_MODE_MI_OVERRIDE_HITS);
        }
        if (is_smooth_mode(self.above_uv_mode[c]) && !is_smooth_mode(above))
            || (is_smooth_mode(self.left_uv_mode[r]) && !is_smooth_mode(left))
        {
            hit!(INTER_NEIGHBOUR_NOT_SMOOTH_HITS);
        }
        is_smooth_mode(above) || is_smooth_mode(left)
    }

    /// The above/left luma modes libaom's `above_mi`/`left_mi` really see for a
    /// block at coarse [`SUB`]-grid `(r, c)`. lane-rectx r5: the coarse
    /// `above_mode`/`left_mode` slots are written in 16x16 cells, so a
    /// sub-16x16 neighbour (a 16x8 rect leaf, an 8x8 split cell) leaves them
    /// holding an OLDER, larger block's mode -- it only ever recorded into the
    /// mi-exact map. Every `kf_y_mode` reader goes through here, not just the
    /// strip path ([`decode_leaf_rect`] took this override in r4; a 32x16 strip
    /// whose left neighbour was a 16x8 leaf still read the stale row and picked
    /// a different CDF for the same mode value).
    /// lane-rectsplitx r2: availability-correct above/left luma modes for a
    /// block whose top-left mi is `(mi_r, mi_c)`. libaom reads `above_mbmi`
    /// only when `up_available` and `left_mbmi` only when `left_available`;
    /// the coarse [`SUB`] bands cannot express that, and with 8-px strips they
    /// actively lie -- four 32x8 strips of one `HORZ_4` share ONE 16x16
    /// `left_mode` cell, so the second strip at tile column 0 read the FIRST
    /// strip's mode as its "left neighbour" (measured on the band fixture at
    /// mi(10,0): mode 2 where libaom has DC, a different `kf_y_mode` row).
    pub(super) fn modes_above_left_mi(
        &self,
        mi_r: usize,
        mi_c: usize,
        fallback: (usize, usize),
    ) -> (usize, usize) {
        let above = if mi_r > self.tile_row0_mi {
            self.mode_above_mi(mi_r, mi_c).unwrap_or(fallback.0)
        } else {
            DC_PRED
        };
        let left = if mi_c > self.tile_col0_mi {
            self.mode_left_mi(mi_r, mi_c).unwrap_or(fallback.1)
        } else {
            DC_PRED
        };
        (above, left)
    }

    /// lane-modectx r1: the coarse-[`SUB`]-grid entry point, now availability
    /// correct. The mi-exact lookups already refuse to cross a tile edge, but
    /// their FALLBACK (`above_mode`/`left_mode`) did not: `left_mode` is reset
    /// per superblock row and never per tile, so the first block column of
    /// tile 2..N read the previous tile's mode as its left neighbour, and the
    /// above band survives across a tile ROW boundary the same way lane-mtfix's
    /// `above_uv_mode` did. libaom reads `above_mbmi`/`left_mbmi` only when
    /// `up_available`/`left_available` and uses DC_PRED otherwise
    /// ([`Self::modes_above_left_mi`]).
    pub(super) fn modes_above_left(&self, r: usize, c: usize) -> (usize, usize) {
        let (mi_r, mi_c) = (r * (SUB / MI), c * (SUB / MI));
        let (above, left) =
            self.modes_above_left_mi(mi_r, mi_c, (self.above_mode[c], self.left_mode[r]));
        if above != self.above_mode[c] || left != self.left_mode[r] {
            hit!(MODE_MI_OVERRIDE_HITS);
        }
        if (is_smooth_mode(self.above_mode[c]) && !is_smooth_mode(above))
            || (is_smooth_mode(self.left_mode[r]) && !is_smooth_mode(left))
        {
            hit!(INTER_NEIGHBOUR_NOT_SMOOTH_HITS);
        }
        // A neighbour the coarse band offered where the tile edge makes it
        // unavailable -- must never happen (see `MODE_TILE_EDGE_COARSE_LEAKS`).
        if (mi_r <= self.tile_row0_mi && self.above_mode[c] != DC_PRED)
            || (mi_c <= self.tile_col0_mi && self.left_mode[r] != DC_PRED)
        {
            hit!(MODE_TILE_EDGE_COARSE_LEAKS);
        }
        (above, left)
    }

    pub(super) fn record_rect(
        &mut self,
        at: (usize, usize),
        w: usize,
        h: usize,
        mode: usize,
        uv_mode: usize,
        grids: &[&[i32]; 3],
    ) {
        self.record_rect_mi(sub16_to_mi(at), w, h, mode, uv_mode, grids);
    }

    /// [`Self::record_rect`] in mi (4 px) units -- lane-inter4 r1: a
    /// 32x8/8x32 strip of a 32-level 1:4 partition sits at an 8-px offset the
    /// [`SUB`]-unit `at` cannot name. The coarse 16-px bands are still keyed
    /// by the enclosing cell (a sub-16 extent writes none of them, exactly as
    /// the [`SUB`]-unit path does).
    pub(super) fn record_rect_mi(
        &mut self,
        at_mi: (usize, usize),
        w: usize,
        h: usize,
        mode: usize,
        uv_mode: usize,
        grids: &[&[i32]; 3],
    ) {
        self.record_rect_mi_modes(at_mi, w, h, mode, uv_mode);
        self.record_mi_rect(at_mi, w, h, grids);
    }

    /// [`Self::record_rect_mi`]'s mode and transform-size half, without the
    /// coefficient-context stamp.
    pub(super) fn record_rect_mi_modes(
        &mut self,
        at_mi: (usize, usize),
        w: usize,
        h: usize,
        mode: usize,
        uv_mode: usize,
    ) {
        let (r, c) = (at_mi.0 / (SUB / MI), at_mi.1 / (SUB / MI));
        // lane-inter4 r2: a strip whose own footprint runs past the frame's
        // last 16-px column/row stamps only the cells that exist (libaom's
        // arrays are padded past the frame and those cells are never read).
        for cell in 0..w / SUB {
            if c + cell >= self.above_mode.len() {
                break;
            }
            self.above_mode[c + cell] = mode;
            self.above_uv_mode[c + cell] = uv_mode;
            self.above_side[c + cell] = w;
        }
        for cell in 0..h / SUB {
            if r + cell >= self.left_mode.len() {
                break;
            }
            self.left_mode[r + cell] = mode;
            self.left_uv_mode[r + cell] = uv_mode;
            self.left_side[r + cell] = h;
        }
        self.record_mode_mi(at_mi.0, at_mi.1, w / MI, h / MI, mode);
        self.record_uv_mode_mi(at_mi.0, at_mi.1, w / MI, h / MI, uv_mode);
    }

    /// lane-av1chromarect: [`Self::record_rect_mi`] for a block whose uv
    /// plane block is SEVERAL transform units, each of which stamped its own
    /// span through [`Self::record_mi_chroma`]. Writing one whole-block state
    /// over them is the `override-slot-on-one-arm` class: libaom keeps the
    /// per-unit above/left contexts distinct, and the next block's first
    /// unit reads them. The luma plane has one unit here, so it is stamped
    /// exactly as the whole-block record stamps it.
    pub(super) fn record_rect_mi_luma_only(
        &mut self,
        at_mi: (usize, usize),
        w: usize,
        h: usize,
        mode: usize,
        uv_mode: usize,
        luma_grid: &[i32],
    ) {
        self.record_rect_mi_modes(at_mi, w, h, mode, uv_mode);
        fill_span(&mut self.left_side_mi, at_mi.0, h / MI, h as u8);
        fill_span(&mut self.above_side_mi, at_mi.1, w / MI, w as u8);
        self.record_mi_luma_rect(at_mi, w, h, luma_grid);
    }

    /// The coefficient-context half of [`Self::record`], taking the block's
    /// position directly in 4x4 mode-info units rather than [`SUB`]-grid
    /// `(r, c)`: an 8x8 leaf under a straddling 16x16 (lane-av1-rect) sits at
    /// a mi offset [`Self::record`]'s SUB-unit `at` cannot name.
    pub(super) fn record_mi(&mut self, at_mi: (usize, usize), side: usize, grids: &[&[i32]; 3]) {
        self.record_mi_rect(at_mi, side, side, grids);
    }

    /// [`Self::record_mi`] with independent above (`w`)/left (`h`) extents,
    /// in pixels (lane-partitions r1, mirroring [`Self::record_rect`]).
    pub(super) fn record_mi_rect(&mut self, at_mi: (usize, usize), w: usize, h: usize, grids: &[&[i32]; 3]) {
        let (mi_r, mi_c) = at_mi;
        let states: [Neighbour; 3] = std::array::from_fn(|plane| neighbour_state(grids[plane]));
        let (w_mi, h_mi) = (w / MI, h / MI);
        if crate::envflags::env_flag!("EC_ECPUB") {
            eprintln!(
                "EC_ECPUB blk mi=({mi_r},{mi_c}) wh=({w},{h}) lvl={}",
                states[0].level
            );
        }
        fill_span(&mut self.left_side_mi, mi_r, h_mi, h as u8);
        fill_span(&mut self.above_side_mi, mi_c, w_mi, w as u8);
        // A chroma 4x4 unit straddling the true luma edge is still whole in
        // chroma's own halved grid, so libaom rounds the luma bound up to the
        // plane's own 4x4 unit before clamping a subsampled plane
        // (`av1_write_intra_coeffs_mb`, encodetxb.c).
        let round_up_even = |n: usize| n.div_ceil(2) * 2;
        let bound_h = [
            self.mi_rows,
            round_up_even(self.mi_rows),
            round_up_even(self.mi_rows),
        ];
        let bound_w = [
            self.mi_cols,
            round_up_even(self.mi_cols),
            round_up_even(self.mi_cols),
        ];
        // lane-sb128c r7: a 128x64 at the bottom frame edge names mi rows
        // past the padded side arrays (mi_rows 198, block rows 192..208);
        // libaom's own arrays are superblock-sized, so those cells simply
        // have no reader -- [`fill_span`] drops them, as the `break` did.
        // `bound_h[1] == bound_h[2] >= bound_h[0]`, so the per-cell plane
        // predicate is three contiguous runs: all planes, chroma only, none
        // (lane-mi).
        let luma_h = h_mi.min(bound_h[0].saturating_sub(mi_r));
        let chroma_h = h_mi.min(bound_h[1].saturating_sub(mi_r));
        let uv_only = [Neighbour::default(), states[1], states[2]];
        fill_span(&mut self.left, mi_r, luma_h, states);
        fill_span(&mut self.left, mi_r + luma_h, chroma_h - luma_h, uv_only);
        fill_span(
            &mut self.left,
            mi_r + chroma_h,
            h_mi - chroma_h,
            [Neighbour::default(); 3],
        );
        let luma_w = w_mi.min(bound_w[0].saturating_sub(mi_c));
        let chroma_w = w_mi.min(bound_w[1].saturating_sub(mi_c));
        let uv_only_w = [Neighbour::default(), states[1], states[2]];
        fill_span(&mut self.above, mi_c, luma_w, states);
        fill_span(&mut self.above, mi_c + luma_w, chroma_w - luma_w, uv_only_w);
        fill_span(
            &mut self.above,
            mi_c + chroma_w,
            w_mi - chroma_w,
            [Neighbour::default(); 3],
        );
    }

    /// [`Self::record_mi`]'s plane-0 half only, for one luma transform unit
    /// inside a coded block whose luma transform is smaller than its own
    /// side (`TxMode::Select`): the real per-transform-unit coefficient
    /// context (spec `AboveLevelContext`/`LeftLevelContext`), unlike the
    /// coarse per-block `tx_grid`/`fill_lf_grid` state
    /// [`Self::record_split_luma`] still writes once for the whole block.
    /// Leaves `above_side_mi`/`left_side_mi` and the chroma planes alone --
    /// the caller's own [`Self::record_split_luma`] call sets those once,
    /// at the block's own true side, not this TU's.
    pub(super) fn record_mi_luma(&mut self, at_mi: (usize, usize), tx_px: usize, grid: &[i32]) {
        self.record_mi_luma_rect(at_mi, tx_px, tx_px, grid);
    }

    /// [`Self::record_mi_luma`] for ONE CHROMA transform unit of a block whose
    /// uv plane block is several units (lane-sb128b r2: a 128x128 block's
    /// 64x64 uv plane block is four TX_32X32 units, `av1_get_max_uv_txsize`).
    /// `w_px`/`h_px` are the unit's extent in LUMA pixels -- the chroma
    /// coefficient-context arrays are indexed by luma mi, the convention
    /// [`Self::record_split_luma_rect_mi`] already writes them in, frame-edge
    /// clamp included.
    pub(super) fn record_mi_chroma(
        &mut self,
        (mi_r, mi_c): (usize, usize),
        w_px: usize,
        h_px: usize,
        plane: usize,
        grid: &[i32],
    ) {
        // lane-arfchroma: a 4x4 CHROMA transform unit is `4 << ss` luma px per
        // axis, i.e. `1 << ss` LUMA mi, so its context cell is the step-aligned
        // group of mi a block may only PART of. A block starting mid-cell (a
        // 16x4 / 4x16 1:4 strip, whose mi row/col is ODD at 4:2:0) stamped its
        // own origin plus the mi row/col BELOW it, so the strip above a block
        // handed that block a "left neighbour" libaom never had -- its chroma
        // cell is one row up -- and the block's chroma `txb_skip` context came
        // out one base high (ours 2, libaom 7+1 = 8, at 320x240 frame 2 block
        // mi (4,0) of the 4:2:0 lossless alt-ref witness). Align the origin
        // DOWN to the cell: that is where libaom's per-plane 2-D
        // `left/above_context_map` cell lives, indexed in the PLANE's own 4x4
        // units rather than in luma mi.
        let (step_x, step_y) = (w_px / MI, h_px / MI);
        // the flags read the CALLER's origin, before the shadowing align below
        let (row_mid, col_mid) = (mi_r & (step_y - 1) != 0, mi_c & (step_x - 1) != 0);
        if row_mid || col_mid {
            CHROMA_MIDCELL_PUB_HITS.with(|c| {
                let mut v = c.get();
                v[0] += usize::from(row_mid);
                v[1] += usize::from(col_mid);
                v[2] += usize::from(row_mid && col_mid);
                c.set(v);
            });
        }
        let (mi_r, mi_c) = (mi_r & !(step_y - 1), mi_c & !(step_x - 1));
        let round_up_even = |n: usize| n.div_ceil(2) * 2;
        let (bound_h, bound_w) = (round_up_even(self.mi_rows), round_up_even(self.mi_cols));
        let state = neighbour_state(grid);
        if crate::envflags::env_flag!("EC_CHROMA_PUB") {
            eprintln!(
                "EC_CHROMA_PUB plane={plane} mi=({mi_r},{mi_c}) wh=({w_px},{h_px}) lvl={} dc={}",
                state.level,
                state.dc.map_or(-1, i32::from)
            );
        }
        for cell in 0..(h_px / MI).min(bound_h.saturating_sub(mi_r)) {
            self.left[mi_r + cell][plane] = state;
        }
        for cell in 0..(w_px / MI).min(bound_w.saturating_sub(mi_c)) {
            self.above[mi_c + cell][plane] = state;
        }
    }

    /// [`Self::record_mi_luma`] for a RECTANGULAR transform unit (lane-rectsplitx
    /// r1): the above context spans the unit's width, the left context its
    /// height, which for a square unit is the same loop it always was.
    pub(super) fn record_mi_luma_rect(
        &mut self,
        (mi_r, mi_c): (usize, usize),
        w_px: usize,
        h_px: usize,
        grid: &[i32],
    ) {
        let state = neighbour_state(grid);
        if crate::envflags::env_flag!("EC_ECPUB") {
            eprintln!(
                "EC_ECPUB tu  mi=({mi_r},{mi_c}) wh=({w_px},{h_px}) lvl={}",
                state.level
            );
        }
        for cell in 0..h_px / MI {
            if mi_r + cell < self.mi_rows {
                self.left[mi_r + cell][0] = state;
            }
        }
        for cell in 0..w_px / MI {
            if mi_c + cell < self.mi_cols {
                self.above[mi_c + cell][0] = state;
            }
        }
    }

    /// [`Self::record`]'s mode/side and chroma-context bookkeeping, for a
    /// block whose luma was decoded as several transform units through
    /// [`Self::record_mi_luma`] rather than [`Self::record`]'s own single
    /// whole-block luma write -- everything [`Self::record`] does except
    /// plane 0, which is already correct per-TU by the time this runs.
    pub(super) fn record_split_luma(
        &mut self,
        at: (usize, usize),
        side: usize,
        mode: usize,
        uv_mode: usize,
        chroma_grids: [&[i32]; 2],
    ) {
        self.record_split_luma_rect(at, side, side, mode, uv_mode, chroma_grids);
    }

    /// [`Self::record_split_luma`] with independent above (`w`)/left (`h`)
    /// extents in pixels (lane-rectsplit r1), the same relationship
    /// [`Self::record_rect`] has to [`Self::record`]: a HORZ/VERT strip whose
    /// luma transform split into several square units needs exactly this
    /// bookkeeping, at its own true rectangular extents.
    pub(super) fn record_split_luma_rect(
        &mut self,
        at: (usize, usize),
        w: usize,
        h: usize,
        mode: usize,
        uv_mode: usize,
        chroma_grids: [&[i32]; 2],
    ) {
        let (r, c) = at;
        self.record_split_luma_rect_mi(
            (r * (SUB / MI), c * (SUB / MI)),
            w,
            h,
            mode,
            uv_mode,
            chroma_grids,
        );
    }

    /// [`Self::record_split_luma_rect`] addressed in mi units (lane-rectsplitx
    /// r1) -- a strip 8 px tall starts mid-[`SUB`]-cell, which the coarse
    /// `(r, c)` form cannot name. The coarse mode/side arrays are stamped for
    /// every cell the strip TOUCHES (`div_ceil`), last writer wins, the
    /// convention [`decode_block_rect4`] already established for 1:4 strips.
    pub(super) fn record_split_luma_rect_mi(
        &mut self,
        at_mi: (usize, usize),
        w: usize,
        h: usize,
        mode: usize,
        uv_mode: usize,
        chroma_grids: [&[i32]; 2],
    ) {
        self.record_split_luma_rect_mi_modes(at_mi, w, h, mode, uv_mode);
        let (mi_r, _) = at_mi;
        let (w_mi, h_mi) = (w / MI, h / MI);
        let round_up_even = |n: usize| n.div_ceil(2) * 2;
        let (bound_h, bound_w) = (round_up_even(self.mi_rows), round_up_even(self.mi_cols));
        for (plane_idx, grid) in chroma_grids.into_iter().enumerate() {
            let plane = plane_idx + 1;
            let state = neighbour_state(grid);
            for cell in 0..h_mi {
                if cell < h_mi.min(bound_h.saturating_sub(mi_r)) {
                    self.left[mi_r + cell][plane] = state;
                }
            }
            for cell in 0..w_mi {
                if cell < w_mi.min(bound_w.saturating_sub(at_mi.1)) {
                    self.above[at_mi.1 + cell][plane] = state;
                }
            }
        }
    }

    /// [`Self::record_split_luma_rect_mi`]'s mode and transform-size half,
    /// without the per-plane coefficient stamp -- see
    /// [`Self::record_rect_mi_luma_only`] for why a multi-unit chroma block
    /// must not take the whole-block stamp.
    pub(super) fn record_split_luma_rect_mi_modes(
        &mut self,
        at_mi: (usize, usize),
        w: usize,
        h: usize,
        mode: usize,
        uv_mode: usize,
    ) {
        let (mi_r, mi_c) = at_mi;
        let (r, c) = (mi_r / (SUB / MI), mi_c / (SUB / MI));
        for cell in 0..w.div_ceil(SUB) {
            if c + cell >= self.above_mode.len() {
                break;
            }
            self.above_mode[c + cell] = mode;
            self.above_uv_mode[c + cell] = uv_mode;
            self.above_side[c + cell] = w;
        }
        for cell in 0..h.div_ceil(SUB) {
            if r + cell >= self.left_mode.len() {
                break;
            }
            self.left_mode[r + cell] = mode;
            self.left_uv_mode[r + cell] = uv_mode;
            self.left_side[r + cell] = h;
        }

        let (w_mi, h_mi) = (w / MI, h / MI);
        self.record_mode_mi(mi_r, mi_c, w_mi, h_mi, mode);
        self.record_uv_mode_mi(mi_r, mi_c, w_mi, h_mi, uv_mode);
        fill_span(&mut self.left_side_mi, mi_r, h_mi, h as u8);
        fill_span(&mut self.above_side_mi, mi_c, w_mi, w as u8);
    }
}

/// Reads one coded block's skip flag, luma mode, angle delta (if the mode
/// carries one) and chroma mode, mirroring [`crate::tile`]'s `write_intra_mode`
/// exactly — including its skip context, which the real key-frame writer
/// always writes at a fixed context of zero (it never tracks a neighbour-based
/// one for intra blocks; that is the inter-frame writer's rule instead).
/// Refuses a nonzero angle delta: this decoder reconstructs the thirteen
/// luma modes at their base angle only. Chroma is DC-only or, where CFL is
/// offered (`cfl`, `is_cfl_allowed`, spec 5.11.5), chroma-from-luma -- the
/// third element carries that mode's `(alpha_q3_u, alpha_q3_v)`, `None` for
/// plain DC chroma.
/// spec 5.11.47 / libaom `av1_read_tx_type`'s `fimode_to_intradir`: which
/// `intra_ext_tx_cdf` row a luma transform reads. A filter-intra block's
/// `mode` is always `DC_PRED`, so the row comes from the FILTER-INTRA mode
/// instead (the defect commit 1a89ed9 fixed at the square sites).
pub(crate) fn fi_tx_row(mode: usize, filter_intra: Option<usize>) -> usize {
    const FIMODE_TO_INTRADIR: [usize; 5] = [DC_PRED, V_PRED, H_PRED, D157_PRED, DC_PRED];
    filter_intra.map_or(mode, |fi| FIMODE_TO_INTRADIR[fi])
}

/// A block-size class index into [`cdf::FILTER_INTRA`]/`Cdfs::filter_intra`,
/// or `None` when `side` is past `av1_filter_intra_allowed_bsize`'s <=32
/// bound (spec `filter_intra_mode_info` never reads a symbol there).
pub(crate) fn filter_intra_size_class(side: usize) -> Option<usize> {
    match side {
        4 => Some(0),
        8 => Some(1),
        16 => Some(2),
        32 => Some(3),
        _ => None,
    }
}

/// [`filter_intra_size_class`] for a true `bw`x`bh` rect strip
/// (lane-intradisp r1): `av1_filter_intra_allowed_bsize` permits any bsize
/// with both dims <= 32, which includes `BLOCK_32X16`/`BLOCK_16X32` -- their
/// own distinct `cdf::FILTER_INTRA` rows, classes 4/5.
pub(super) fn filter_intra_size_class_rect(bw: usize, bh: usize) -> Option<usize> {
    match (bw, bh) {
        (32, 16) => Some(4),
        (16, 32) => Some(5),
        // lane-rectx r3: `av1_filter_intra_allowed_bsize` is "both sides <=
        // 32", so a 16x8/8x16 strip reads the flag too -- returning `None`
        // here dropped that symbol on every DC_PRED strip of that size and
        // desynced the tile (caught against an aomdec EC_TRACE_COEFF ladder:
        // identical mode/uv_mode values, range 34808 vs the oracle's 40668).
        (16, 8) => Some(6),
        (8, 16) => Some(7),
        // lane-fi8 r1: the sub-8 rect leaves are allowed too (both sides <=
        // 32), and `decode_leaf_rect8` codes them -- rows 1/2 of
        // `default_filter_intra_cdfs`, NOT BLOCK_4X4's row 0.
        (4, 8) => Some(8),
        (8, 4) => Some(9),
        // lane-fi32x8 r1: same bound, the 32x32-level 1:4 pair
        // (`decode_block_rect4`'s 32x8/8x32 strips). `None` here read no
        // `use_filter_intra` symbol on those strips although aomenc writes
        // one for every DC_PRED block of that shape with
        // `--enable-filter-intra=1` (its default), silently desyncing the
        // tile (class symbol-consumption-gap).
        (32, 8) => Some(10),
        (8, 32) => Some(11),
        // lane-r14: the 16x16-level 1:4 pair (`decode_rect4_16`'s 16x4/4x16
        // strips), rows 17/16 of `default_filter_intra_cdfs`.
        (16, 4) => Some(12),
        (4, 16) => Some(13),
        _ if bw == bh => filter_intra_size_class(bw),
        _ => None,
    }
}

/// `av1_allow_palette`'s size bound (blockd.h: `block_size_wide/high <=
/// MAX_PALETTE_BLOCK_WIDTH/HEIGHT` (64) and `sb_type >= BLOCK_8X8`) plus
/// `av1_get_palette_bsize_ctx` (pred_common.h: `num_pels_log2_lookup[bsize]
/// - num_pels_log2_lookup[BLOCK_8X8]`, i.e. `log2(bw*bh) - 6`) -- `None` past
/// the bound (never gates a palette read), `Some(bsize_ctx)` otherwise.
pub(crate) fn palette_bsize_ctx(side: usize) -> Option<usize> {
    palette_bsize_ctx_wh(side, side)
}

/// [`palette_bsize_ctx`], generalised to a true `bw`x`bh` rect strip
/// (lane-palette2 r4) -- the same `log2(bw*bh) - 6` formula libaom's
/// `num_pels_log2_lookup[bsize] - num_pels_log2_lookup[BLOCK_8X8]` computes
/// for ANY bsize, not just the square ones a hand-picked table happened to
/// enumerate before ([[cdf-row-held-constant]]: a table narrowed to a pinned
/// row breaks the moment its indexing field moves off the diagonal, and a
/// 64x32/32x64 superblock-level strip is exactly such a move). Every AV1
/// block dimension is a power of two, so `bw*bh` always is too and
/// `trailing_zeros` is `log2` exactly. `None` past `av1_allow_palette`'s own
/// bound (`block_size_wide/high <= 64`, `sb_type >= BLOCK_8X8`).
pub(super) fn palette_bsize_ctx_wh(bw: usize, bh: usize) -> Option<usize> {
    // lane-r14: `av1_allow_palette`'s bound is `sb_type >= BLOCK_8X8` by the
    // BLOCK_SIZES_ALL enum INDEX, which the 1:4 shapes BLOCK_4X16 (16) and
    // BLOCK_16X4 (17) clear -- a 64-pixel block, so `av1_get_palette_bsize_ctx`
    // (`num_pels_log2 - num_pels_log2[BLOCK_8X8]`) is 0 for them, exactly as
    // for an 8x8. Reading the dimension bound as "both sides >= 8" dropped
    // `palette_y_mode` on every 16x4/4x16 strip of a screen-content frame.
    if bw > 64 || bh > 64 || bw * bh < 64 {
        return None;
    }
    Some(((bw * bh) as u32).trailing_zeros() as usize - 6)
}

/// A just-decoded palette-Y block's own size, base colours (only the first
/// `size` entries are real), and per-pixel colour-index map (`side*side`,
/// row-major, each entry `< size`).
pub(super) struct PaletteY {
    pub(super) size: usize,
    pub(super) colors: [u16; 8],
    pub(super) map: Vec<u8>,
}

/// As [`PaletteY`], for a just-decoded chroma palette block: one shared
/// per-pixel colour-index map (spec `av1_visit_palette`'s plane-1 pass is
/// shared between U and V, both co-located after 4:2:0 subsampling) plus
/// each plane's own base colours.
pub(super) struct PaletteUv {
    pub(super) size: usize,
    pub(super) u_colors: [u16; 8],
    pub(super) v_colors: [u16; 8],
    pub(super) map: Vec<u8>,
}

/// `aom_ceil_log2` (aom_dsp/bitwriter_buffer.h-adjacent helper, used both by
/// `read_palette_colors_y`'s shrinking `bits` and `av1_read_uniform`'s
/// `get_unsigned_bits`, which is the same formula): smallest `n` with
/// `2^n >= x`, `0` for `x <= 1`.
pub(crate) fn ceil_log2(x: u32) -> u32 {
    if x < 2 {
        0
    } else {
        32 - (x - 1).leading_zeros()
    }
}

/// `av1_read_uniform` (decoder.h:425): a uniform `0..n` value coded as
/// `l-1` raw bits plus a conditional extra bit, `l = get_unsigned_bits(n)`.
fn read_uniform(dec: &mut SymbolDecoder, n: usize) -> usize {
    let l = ceil_log2(n as u32);
    let m = (1usize << l) - n;
    let v = dec.literal(l - 1) as usize;
    if v < m {
        v
    } else {
        (v << 1) - m + dec.literal(1) as usize
    }
}

/// `merge_colors` (decodemv.c:462): interleaves the ascending `cached`
/// colours (already-known, from the above/left neighbours) with the
/// ascending `transmitted` ones (delta-coded off the wire) back into one
/// ascending list of `cached.len() + transmitted.len()` colours -- a tied
/// value picks the cached copy first, matching the C `<=`.
fn merge_colors(transmitted: &[u16], cached: &[u16]) -> Vec<u16> {
    let mut out = Vec::with_capacity(transmitted.len() + cached.len());
    let (mut ci, mut ti) = (0, 0);
    while ci < cached.len() && ti < transmitted.len() {
        if cached[ci] <= transmitted[ti] {
            out.push(cached[ci]);
            ci += 1;
        } else {
            out.push(transmitted[ti]);
            ti += 1;
        }
    }
    out.extend_from_slice(&cached[ci..]);
    out.extend_from_slice(&transmitted[ti..]);
    out
}

/// `read_palette_colors_y` (decodemv.c:478), at the sequence's real
/// `bit_depth` -- lane-kf900 r3: every raw literal here (the first colour,
/// each delta's width, the `range` bound and the clamp) is sized by
/// `bit_depth`, and hardcoding 8 read 2 bits too few per colour on every
/// 10-bit palette block. Raw literals are EQUIPROBABLE, and at the ranges
/// this stream reaches (e.g. 62728) an equiprobable read is a FIXED POINT of
/// the msac range, so a cross-decoder range ladder stays bit-identical
/// through the whole miscount -- only the decoded VALUE differs (class
/// `compare-range-not-tell`'s blind spot). (this
/// decoder is 8-bit only): accepts up to `n` colours straight from `cache`
/// (one flag bit each, in cache order), then delta-codes the remainder
/// (first value raw, each following delta `>= 1` off a shrinking bit width),
/// and merges the two ascending lists back together.
pub(super) fn read_palette_colors_y(
    dec: &mut SymbolDecoder,
    n: usize,
    cache: &[u16],
    fctx: &crate::decode::FrameCtx,
) -> [u16; 8] {
    let mut cached = Vec::with_capacity(n);
    for &c in cache {
        if cached.len() >= n {
            break;
        }
        if dec.literal(1) == 1 {
            cached.push(c);
        }
    }
    let bd = u32::from(bit_depth(fctx));
    let max_val = (1i32 << bd) - 1;
    let mut colors = [0u16; 8];
    let n_cached = cached.len();
    if n_cached < n {
        let mut transmitted = Vec::with_capacity(n - n_cached);
        let first = dec.literal(bd) as u16;
        transmitted.push(first);
        if n_cached + 1 < n {
            let min_bits = bd - 3;
            let mut bits = min_bits + dec.literal(2);
            let mut range = (1i32 << bd) - i32::from(first) - 1;
            for _ in (n_cached + 1)..n {
                let delta = dec.literal(bits) as i32 + 1;
                let prev = *transmitted.last().unwrap();
                let val = (i32::from(prev) + delta).clamp(0, max_val) as u16;
                range -= i32::from(val) - i32::from(prev);
                bits = bits.min(ceil_log2(range.max(0) as u32));
                transmitted.push(val);
            }
        }
        let merged = merge_colors(&transmitted, &cached);
        colors[..n].copy_from_slice(&merged);
    } else {
        colors[..n].copy_from_slice(&cached[..n]);
    }
    colors
}

/// `read_palette_colors_uv` (decodemv.c:509), at the sequence's real
/// `bit_depth` (see [`read_palette_colors_y`]): U reads
/// exactly [`read_palette_colors_y`]'s cache/delta scheme (against the U-only
/// cache, `range` starting at `255 - prev` with no `-1`, and each raw `delta`
/// unbiased by `+1` -- both differences from Y's own reader, matching the C
/// side by side). V never uses a cache: a leading bit picks either `n` raw
/// literals, or a first raw value plus `n-1` signed deltas that wrap mod 256
/// rather than clamp.
pub(super) fn read_palette_colors_uv(
    dec: &mut SymbolDecoder,
    n: usize,
    cache: &[u16],
    fctx: &crate::decode::FrameCtx,
) -> ([u16; 8], [u16; 8]) {
    let mut cached = Vec::with_capacity(n);
    for &c in cache {
        if cached.len() >= n {
            break;
        }
        if dec.literal(1) == 1 {
            cached.push(c);
        }
    }
    let bd = u32::from(bit_depth(fctx));
    let max_val = 1i32 << bd;
    let mut u_colors = [0u16; 8];
    let n_cached = cached.len();
    if n_cached < n {
        let mut transmitted = Vec::with_capacity(n - n_cached);
        let first = dec.literal(bd) as u16;
        transmitted.push(first);
        if n_cached + 1 < n {
            let min_bits = bd - 3;
            let mut bits = min_bits + dec.literal(2);
            let mut range = max_val - i32::from(first);
            for _ in (n_cached + 1)..n {
                let delta = dec.literal(bits) as i32;
                let prev = *transmitted.last().unwrap();
                let val = (i32::from(prev) + delta).clamp(0, max_val - 1) as u16;
                range -= i32::from(val) - i32::from(prev);
                bits = bits.min(ceil_log2(range.max(0) as u32));
                transmitted.push(val);
            }
        }
        let merged = merge_colors(&transmitted, &cached);
        u_colors[..n].copy_from_slice(&merged);
    } else {
        u_colors[..n].copy_from_slice(&cached[..n]);
    }
    let mut v_colors = [0u16; 8];
    if dec.literal(1) == 1 {
        let min_bits_v = bd - 4;
        let bits = min_bits_v + dec.literal(2);
        v_colors[0] = dec.literal(bd) as u16;
        for i in 1..n {
            let mut delta = dec.literal(bits) as i32;
            if delta != 0 && dec.literal(1) == 1 {
                delta = -delta;
            }
            let mut val = i32::from(v_colors[i - 1]) + delta;
            if val < 0 {
                val += max_val;
            }
            if val >= max_val {
                val -= max_val;
            }
            v_colors[i] = val as u16;
        }
    } else {
        // Index loop mirrors the spec's per-position form over several offsets; an iterator would obscure it.
        #[allow(clippy::needless_range_loop)]
        for i in 0..n {
            v_colors[i] = dec.literal(bd) as u16;
        }
    }
    (u_colors, v_colors)
}

/// `av1_palette_color_index_context_lookup` (entropymode.c:889): maps a
/// `NUM_PALETTE_NEIGHBORS`-weighted score hash (`0..=8`) to the real
/// `0..=4` context, `-1` for hashes no real neighbour configuration produces.
pub(super) const PALETTE_COLOR_INDEX_CONTEXT_LOOKUP: [i32; 9] = [-1, -1, 0, -1, -1, 4, 3, 2, 1];

/// `av1_get_palette_color_index_context` (entropymode.c:893): the colour-index
/// map's own per-pixel context and colour-order permutation, from the
/// left/up-left/up already-decoded neighbours (weights `[2, 1, 2]`, top-3
/// selection sort). `map`'s stride is `side` (this decoder never crops a
/// palette block's map to a narrower plane edge, [`decode_color_index_map`]'s
/// own doc). Returns `(ctx, color_order)`; `color_order[symbol]` is the
/// actual colour index to store.
pub(crate) fn palette_color_index_context(
    map: &[u8],
    side: usize,
    row: usize,
    col: usize,
    n: usize,
) -> (usize, [u8; 8]) {
    let left = if col >= 1 {
        Some(map[row * side + col - 1])
    } else {
        None
    };
    let up_left = if col >= 1 && row >= 1 {
        Some(map[(row - 1) * side + col - 1])
    } else {
        None
    };
    let up = if row >= 1 {
        Some(map[(row - 1) * side + col])
    } else {
        None
    };
    let mut scores = [0i32; 8];
    for (neighbour, weight) in [(left, 2), (up_left, 1), (up, 2)] {
        if let Some(v) = neighbour {
            scores[v as usize] += weight;
        }
    }
    let mut color_order = [0u8; 8];
    // Index loop mirrors the spec's per-position form over several offsets; an iterator would obscure it.
    #[allow(clippy::needless_range_loop)]
    for i in 0..8 {
        color_order[i] = i as u8;
    }
    for i in 0..3 {
        let mut max = scores[i];
        let mut max_idx = i;
        // Index loop mirrors the spec's per-position form over several offsets; an iterator would obscure it.
        #[allow(clippy::needless_range_loop)]
        for j in (i + 1)..n {
            if scores[j] > max {
                max = scores[j];
                max_idx = j;
            }
        }
        if max_idx != i {
            let max_score = scores[max_idx];
            let max_color = color_order[max_idx];
            for k in (i + 1..=max_idx).rev() {
                scores[k] = scores[k - 1];
                color_order[k] = color_order[k - 1];
            }
            scores[i] = max_score;
            color_order[i] = max_color;
        }
    }
    let hash = scores[0] + scores[1] * 2 + scores[2] * 2;
    let ctx = PALETTE_COLOR_INDEX_CONTEXT_LOOKUP[hash as usize];
    (ctx as usize, color_order)
}

/// spec 5.11.50 `palette_tokens` / `av1_get_block_dimensions` (blockd.h:1512):
/// the ON-SCREEN part of a block at `(mi_r, mi_c)`, in luma pixels. A block
/// cut by the frame's bottom or right mi edge codes its colour-index map only
/// over this part and replicates it outward -- reading the full `bw x bh` grid
/// consumed symbols libaom never wrote and desynced the rest of the tile
/// (lane-svtinter; class map-read-past-frame-edge).
pub(super) fn palette_onscreen(
    mi_r: usize,
    mi_c: usize,
    bw: usize,
    bh: usize,
    fctx: &crate::decode::FrameCtx,
) -> (usize, usize) {
    // lane-palw: the ENCODER's own in-process reconstruct runs on a
    // `FrameCtx::for_encoder`, which never sees `set_segmentation` -- both
    // frame encoders publish the header's mi dims onto it themselves
    // (`encode.rs`, beside their `true_width`/`true_height`) so that this
    // clamp and `tile::write_color_index_map`'s now agree there too.
    let (mi_rows, mi_cols) = fctx.seg_mi_dims.with(|c| c.get());
    palette_onscreen_dims(mi_r, mi_c, bw, bh, (mi_rows, mi_cols))
}

/// [`palette_onscreen`] off explicit mi dims, which is how the WRITER reaches
/// it (`tile::write_palette_syntax`, armed per frame): the two sides must
/// compute the same on-screen extent or the decoder reads a different number
/// of symbols than the encoder wrote.
pub(crate) fn palette_onscreen_dims(
    mi_r: usize,
    mi_c: usize,
    bw: usize,
    bh: usize,
    (mi_rows, mi_cols): (usize, usize),
) -> (usize, usize) {
    (
        bw.min(mi_cols.saturating_sub(mi_c) * MI),
        bh.min(mi_rows.saturating_sub(mi_r) * MI),
    )
}

/// [`palette_onscreen`] carried into the chroma plane: `>> subsampling` plus
/// the SAME sub-8 bump the caller's own `(bw >> ss).max(4)` applies to the
/// block dimensions (`is_chroma_sub8_x/y`, blockd.h:1533) -- both must move
/// together or a cut 8-wide block reads its chroma map at the wrong width.
/// lane-av1-444: the shifts are the sequence's own subsampling (a 4:4:4
/// intra-in-inter palette read its chroma map at the 4:2:0 extent and
/// panicked feeding the 4:2:0-sized prediction to a full-size reconstruct).
pub(crate) fn palette_onscreen_uv(
    luma: (usize, usize),
    on: (usize, usize),
    chroma: (usize, usize),
    ss: (usize, usize),
) -> (usize, usize) {
    (
        (on.0 >> ss.0) + (chroma.0 - (luma.0 >> ss.0)),
        (on.1 >> ss.1) + (chroma.1 - (luma.1 >> ss.1)),
    )
}

/// `decode_color_map_tokens` (detokenize.c:25): the palette-Y colour-index
/// map's wavefront decode over a `side x side` grid, read over the on-screen
/// `on_side x on_side` corner (see [`palette_onscreen`]).
pub(super) fn decode_color_index_map(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    n: usize,
    side: usize,
    on: (usize, usize),
    uv: bool,
) -> Vec<u8> {
    decode_color_index_map_wh(dec, cdfs, n, side, side, on, uv)
}

/// [`decode_color_index_map`], generalised to a true `bw`x`bh` rect strip
/// (lane-palette2 r4): `decode_color_map_tokens`'s own wavefront diagonal
/// (`row = i - j`, `col = j`) walked over `cols = bw`, `rows = bh`
/// independently rather than one shared `side` -- the square call above is
/// just `bw == bh`.
pub(super) fn decode_color_index_map_wh(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    n: usize,
    bw: usize,
    bh: usize,
    // `(onscreenWidth, onscreenHeight)` in this plane's own units.
    on: (usize, usize),
    uv: bool,
) -> Vec<u8> {
    let trace = crate::envflags::env_flag!("EC_AV1_TRACE");
    let (on_bw, on_bh) = (on.0.clamp(1, bw), on.1.clamp(1, bh));
    if on_bw < bw || on_bh < bh {
        hit!(PALETTE_ONSCREEN_CUT_HITS);
    }
    let mut map = vec![0u8; bw * bh];
    if trace {
        let (rng, _) = dec.debug_state();
        eprintln!("EC_PAL row=0 col=0 ctx=-1 n={n} rng={rng}");
    }
    map[0] = read_uniform(dec, n) as u8;
    if trace {
        let (rng, _) = dec.debug_state();
        eprintln!("EC_PAL_VAL row=0 col=0 color_idx={} rng={rng}", map[0]);
    }
    for i in 1..(on_bw + on_bh - 1) {
        let j_hi = i.min(on_bw - 1);
        let j_lo = i.saturating_sub(on_bh - 1);
        for j in (j_lo..=j_hi).rev() {
            let row = i - j;
            let col = j;
            let (ctx, color_order) = palette_color_index_context(&map, bw, row, col, n);
            if trace {
                let (rng, _) = dec.debug_state();
                let row_cdf = if uv {
                    &cdfs.palette_uv_color_index[n - 2][ctx]
                } else {
                    &cdfs.palette_y_color_index[n - 2][ctx]
                };
                eprintln!(
                    "EC_PAL row={row} col={col} ctx={ctx} n={n} cdf0={} cnt={} rng={rng}",
                    row_cdf[0], row_cdf[n]
                );
            }
            let symbol = if uv {
                dec.symbol(&mut cdfs.palette_uv_color_index[n - 2][ctx][..=n])
            } else {
                dec.symbol(&mut cdfs.palette_y_color_index[n - 2][ctx][..=n])
            };
            map[row * bw + col] = color_order[symbol];
            if trace {
                let (rng, _) = dec.debug_state();
                eprintln!("EC_PAL_VAL row={row} col={col} color_idx={symbol} rng={rng}");
            }
        }
    }
    palette_replicate(&mut map, bw, bh, on);
    map
}

/// `decode_color_map_tokens`'s own two extension loops (detokenize.c:71): the
/// last on-screen column then the last on-screen row are replicated over the
/// off-screen remainder, so the prediction still covers the whole block. The
/// ENCODER runs this over its searched map too (`encode.rs`'s palette arms),
/// so the pixels it prices and reconstructs are the ones the on-screen
/// symbols it writes really decode to.
pub(crate) fn palette_replicate(map: &mut [u8], bw: usize, bh: usize, on: (usize, usize)) {
    let (on_bw, on_bh) = (on.0.clamp(1, bw), on.1.clamp(1, bh));
    for row in 0..on_bh {
        let last = map[row * bw + on_bw - 1];
        map[row * bw + on_bw..row * bw + bw].fill(last);
    }
    for row in on_bh..bh {
        map.copy_within((on_bh - 1) * bw..on_bh * bw, row * bw);
    }
}

/// `read_angle_delta`/`Angle_Delta` (spec 5.11.42 + 9.3): the symbol minus
/// [`ANGLE_DELTA_ZERO`] gives the signed `-MAX_ANGLE_DELTA..=MAX_ANGLE_DELTA`
/// delta [`crate::intra::predict`] wants; bumps [`ANGLE_DELTA_HITS`] whenever
/// it lands away from zero, for gate tests to prove a real stream fired one.
pub(super) fn read_angle_delta(dec: &mut SymbolDecoder, cdf: &mut [u16]) -> i32 {
    let symbol = dec.symbol(cdf);
    if symbol != ANGLE_DELTA_ZERO {
        hit!(ANGLE_DELTA_HITS);
    }
    symbol as i32 - ANGLE_DELTA_ZERO as i32
}

/// lane-perf7 step 1: [`cdef_filter_block`]'s AVX2 kernel -- the scalar
/// filter was 13.1% of a 4K decode's self time. Nothing here is spec logic:
/// the same taps, the same `constrain`, the same `min`/`max` clamp with the
/// `CDEF_VERY_LARGE` sentinel excluded from the max, the same
/// `y + ((8 + sum - (sum < 0)) >> 4)` rounding, done 16 samples (two rows) at
/// a time in 16-bit lanes.
///
/// 16-bit lanes are exact for every value this filter sees: window samples
/// are in `[0, CDEF_VERY_LARGE]` (0x4000), so a tap difference fits in
/// `i16`, a `constrain` result is bounded by the threshold (at most
/// `15 << 4`), and the tap-weighted sum by `24 *` that.
/// Spec 7.14: the in-loop deblocking filter, applied once per frame after
/// tile reconstruction and before [`apply_cdef`]. Uniform-level path only --
/// `stream.rs` already refuses `delta_lf_present`/segmentation upstream, so
/// every block's level comes from [`LoopFilterParams::level`] plus the
/// `ref_deltas`/`mode_deltas` adjustment alone (spec 7.14.4's
/// `get_filter_level`, precomputed per-block here rather than cached in a
/// table the way libaom's `av1_loop_filter_frame_init` does).
///
/// Ported from libaom's `av1/common/av1_loopfilter.c` (edge/level decision)
/// and `aom_dsp/loopfilter.c` (the `aom_lpf_*` pixel kernels).
// Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_deblock(
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    lf: &LoopFilterParams,
    n: &Neighbours,
    frame_width: usize,
    frame_height: usize,
    fctx: &crate::decode::FrameCtx,
) {
    // lane-part32 r2 debug rung, env-gated: bisecting a widespread pixel
    // mismatch between the raw reconstruction and post-filter output.
    if crate::envflags::env_flag!("EC_AV1_DEBUG_SKIP_DEBLOCK") {
        return;
    }
    if lf.level[0] != 0 || lf.level[1] != 0 {
        deblock_plane(y, 0, lf, n, frame_width, frame_height, fctx);
    }
    if lf.level[2] != 0 {
        deblock_plane(u, 1, lf, n, frame_width, frame_height, fctx);
    }
    if lf.level[3] != 0 {
        deblock_plane(v, 2, lf, n, frame_width, frame_height, fctx);
    }
}

/// Luma-mi-grid position (row or col) a plane-local pixel coordinate falls
/// in -- `Neighbours::tx_grid`/`ref_grid` are always indexed at the luma
/// 4x4-mi grain, even when `coord` is a chroma coordinate (2 chroma px per
/// mi unit under this decoder's assumed 4:2:0 subsampling).
///
/// lane-t900 r30b: the chroma arm ORs in the subsampling, exactly as
/// `set_lpf_parameters` does (`av1_loopfilter.c:245`, `mi_col = scale_horz |
/// ((x << scale_horz) >> MI_SIZE_LOG2)`) -- "for sub8x8 block, chroma
/// prediction mode is obtained from the bottom/right mi structure of the
/// co-located 8x8 luma block, so for chroma plane both mi_row and mi_col
/// should be odd". Dropping that `| 1` read the group's TOP-LEFT mi, which
/// is the same cell for every block 8x8 and larger and a DIFFERENT coded
/// block (different tx size, ref frame, skip flag and delta_lf) as soon as
/// the group is split into 4x4 luma blocks (class
/// `context-read-from-one-cell`).
pub(super) fn plane_to_mi(chroma: bool, coord: usize, ss: usize) -> usize {
    // scale | ((coord << scale) >> 2). ss=1 is (coord/2)|1; ss=0 is coord/4.
    if chroma {
        ss | ((coord << ss) >> 2)
    } else {
        coord / 4
    }
}

/// This plane's own transform width in pixels at the covering block, spec
/// `get_transform_size`'s plane-aware lookup: `tx_grid` stores the luma
/// width directly; chroma halves it (this decoder's UV transform is always
/// half its coded block's luma transform, spec's `av1_get_max_uv_txsize`
/// under 4:2:0 and a block that never splits its own transform).
pub(super) fn tx_px_at(n: &Neighbours, chroma: bool, mi_r: usize, mi_c: usize) -> u32 {
    let grid = if chroma { &n.uv_tx_grid } else { &n.tx_grid };
    grid.get(mi_r * n.skip_grid_cols_mi + mi_c)
        .copied()
        .unwrap_or(0) as u32
}

/// [`tx_px_at`]'s HEIGHT twin (lane-rect r2): the deblocker's horizontal
/// edges step and gate by transform height, which differs from width only
/// on rect partition strips.
pub(super) fn tx_h_px_at(n: &Neighbours, chroma: bool, mi_r: usize, mi_c: usize) -> u32 {
    let grid = if chroma {
        &n.uv_tx_h_grid
    } else {
        &n.tx_h_grid
    };
    grid.get(mi_r * n.skip_grid_cols_mi + mi_c)
        .copied()
        .unwrap_or(0) as u32
}

/// `get_tx_size_context` (spec 9.3's `TxDepthCtx`, libaom
/// `TXFM_CONTEXT`/`entropymode.c`): the sum of two boolean terms, each
/// present only when its neighbour exists inside the true frame -- whether
/// the block immediately above (at this block's own leftmost mi column) or
/// to the left (at its own topmost mi row) coded a transform at least as wide
/// (tall) as this block's own `max_txsize_rect_lookup[bsize]`.
///
/// lane-av1txbands: this reads the real `TXFM_CONTEXT` bands
/// ([`Neighbours::above_txfm`]/[`Neighbours::left_txfm`]) for EVERY frame
/// now -- libaom maintains them on intra-only frames too (`parse_decode_block`
/// runs `set_txfm_ctxs` for every block, and
/// [`Neighbours::fill_lf_grid_rect`] publishes them there), except that an
/// *inter* neighbour contributes its own BLOCK size rather than its transform
/// size -- and libaom's `is_inter_block` counts an INTRABC block
/// (`blockd.h:373`), which is what the `above_inter`/`left_inter` bands carry.
/// The previous deblock-grid approximation coincided with this only while
/// every neighbour coded one whole-block transform, which is exactly what
/// `TxMode::Select` ends.
///
/// The rect form (lane-intrasplit r1): `get_tx_size_context` compares the
/// neighbour ABOVE against `tx_size_wide[max_txsize_rect_lookup[bsize]]` and
/// the neighbour to the LEFT against `tx_size_high[...]` (libaom
/// `pred_common.h:342`), which for a 2:1 strip are two different numbers.
pub(super) fn tx_size_context_txfm_rect(
    n: &Neighbours,
    (mi_r, mi_c): (usize, usize),
    own_w: usize,
    own_h: usize,
    fctx: &crate::decode::FrameCtx,
) -> usize {
    hit!(INTRA_IN_INTER_TXCTX_HITS);
    // `max_txsize_rect_lookup[bsize]` tops out at TX_64X64: no transform is
    // wider or taller than 64, so a block with a 128 side resolves against 64
    // (lane-sb128b r1).
    let (own_w, own_h) = (own_w.min(64), own_h.min(64));
    let has_above = mi_r > n.tile_row0_mi;
    let has_left = mi_c > n.tile_col0_mi;
    let mut above = usize::from(n.above_txfm[mi_c]) >= own_w;
    let mut left = usize::from(n.left_txfm[mi_r]) >= own_h;
    // lane-av1txctxband: the override's gate and its size term are read at
    // the ABUTTING CELL -- libaom's `xd->above_mbmi` is `xd->mi[-mi_stride]`
    // and `xd->left_mbmi` is `xd->mi[-1]` (`set_mi_offsets`,
    // av1_common_int.h:1381-1389), which `setup_mbs` fills with the pointer
    // of the block COVERING that cell (decodeframe.c:355-361). The old code
    // read the per-COLUMN `above_inter`/`left_inter` bands and the matching
    // `above_side_mi`/`left_side_mi` bands, i.e. the LAST block written in
    // that column, which names the abutting block only for the block directly
    // below it. Measured: on the 4:4:4 lossy `--sb-size=128` cq-62 stream the
    // 8x4 intra leaf at mi(57,52) saw a stale `above_inter=true` (from an
    // intrabc block earlier in column 52) with `above_side=4`, took the
    // override, and read `tx_size_cat0[1]` where libaom reads row 2.
    if has_above && n.inter_at(mi_r - 1, mi_c) {
        above = n.blk_side_at(mi_r - 1, mi_c).0 >= own_w;
    }
    if has_left && n.inter_at(mi_r, mi_c - 1) {
        left = n.blk_side_at(mi_r, mi_c - 1).1 >= own_h;
    }
    // lane-av1txctxband: the non-vacuity arm. It counts the reads whose
    // RESOLVED context term differs from what the old per-column bands would
    // have produced -- not merely reads where the two `is_inter` booleans
    // disagree, because on the measured witness they AGREE (`above_inter` and
    // the per-cell cover are both true at mi(57,52)) and it is the SIZE term
    // that is stale (band 4 vs the covering block's own 8), which is what put
    // the read on `tx_size_cat0[1]` instead of row 2.
    {
        let band_above = has_above
            && if n.above_inter[mi_c] {
                usize::from(n.above_side_mi[mi_c]) >= own_w
            } else {
                usize::from(n.above_txfm[mi_c]) >= own_w
            };
        let band_left = has_left
            && if n.left_inter[mi_r] {
                usize::from(n.left_side_mi[mi_r]) >= own_h
            } else {
                usize::from(n.left_txfm[mi_r]) >= own_h
            };
        if band_above != (has_above && usize::from(above) != 0)
            || band_left != (has_left && usize::from(left) != 0)
        {
            hit!(TXCTX_INTER_CELL_BAND_DIVERGENCE_HITS);
        }
    }
    if crate::envflags::env_flag!("EC_TXCTX") {
        eprintln!(
            "EC_TXCTX mi={mi_r},{mi_c} own={own_w}x{own_h} ha={has_above} hl={has_left} \
             intra_only={} above_txfm={} left_txfm={} above_inter={} left_inter={} \
             above_side={} left_side={} above={above} left={left} \
             | cell_inter ab={} lb={} ab_side={} lb_side={}",
            fctx.intra_only.with(std::cell::Cell::get),
            n.above_txfm[mi_c],
            n.left_txfm[mi_r],
            n.above_inter[mi_c],
            n.left_inter[mi_r],
            n.above_side_mi[mi_c],
            n.left_side_mi[mi_r],
            // lane-av1txctxband: the per-CELL terms this read now uses, next to
            // the per-column bands it used to use, so a divergence names which
            // of the two disagreed.
            if has_above { n.inter_at(mi_r - 1, mi_c) } else { false },
            if has_left { n.inter_at(mi_r, mi_c - 1) } else { false },
            if has_above { n.blk_side_at(mi_r - 1, mi_c).0 } else { 0 },
            if has_left { n.blk_side_at(mi_r, mi_c - 1).1 } else { 0 },
        );
    }
    let ctx = match (has_above, has_left) {
        (true, true) => usize::from(above) + usize::from(left),
        (true, false) => usize::from(above),
        (false, true) => usize::from(left),
        (false, false) => 0,
    };
    {
        let stale = usize::from(has_above && tx_px_at(n, false, mi_r - 1, mi_c) as usize >= own_w)
            + usize::from(has_left && tx_h_px_at(n, false, mi_r, mi_c - 1) as usize >= own_h);
        if ctx != stale {
            hit!(TXCTX_DEBLOCK_DIVERGENCE_HITS);
        }
    }
    ctx
}

/// lane-intrasplit r3: `above_inter`/`left_inter` are mi-granular bands (they
/// are written and read everywhere else with a raw mi index); the SUB-cell
/// division this pair used read a cell four blocks away, so the inter-neighbour
/// override above fired on the wrong neighbour -- the first divergence of the
/// 10-bit gate's stream was exactly this (`tx_depth` ctx 0 where libaom has 1,
/// at the intra block right of an inter one).
pub(super) fn tx_size_context_txfm(n: &Neighbours, (mi_r, mi_c): (usize, usize), side: usize) -> usize {
    let has_above = mi_r > n.tile_row0_mi;
    let has_left = mi_c > n.tile_col0_mi;
    let mut above = usize::from(n.above_txfm[mi_c]) >= side;
    let mut left = usize::from(n.left_txfm[mi_r]) >= side;
    // lane-angledelta r1 (class [[context-read-from-one-cell]]): both
    // `above_inter`/`left_inter` bands are written per MI unit
    // (`record_inter_rect_mi`), so they are read per MI too -- the `/ (SUB /
    // MI)` here read the 16-px cell's FIRST mi instead, i.e. a different
    // block whenever the neighbour column/row is not the cell's first. A
    // 16x16 intra block whose left neighbour is an inter block at mi row 4
    // then missed libaom's "an inter neighbour contributes its BLOCK size"
    // override and read `tx_size_cat1` row 1 where aomdec reads row 2.
    // lane-angledelta r1 (class [[context-read-from-one-cell]]) fixed the
    // `/ (SUB / MI)` on this pair; lane-av1txctxband fixes what it could not:
    // the bands are per COLUMN, so they name the last block written in the
    // column, not the block covering the abutting cell. libaom's
    // `xd->above_mbmi` is `xd->mi[-xd->mi_stride]` and `xd->left_mbmi` is
    // `xd->mi[-1]` (`set_mi_offsets`, av1_common_int.h:1381-1389), and
    // `setup_mbs` (decodeframe.c:355-361) writes the covering block's own
    // pointer into every mi cell the block covers.
    if has_above && n.inter_at(mi_r - 1, mi_c) {
        above = n.blk_side_at(mi_r - 1, mi_c).0 >= side;
    }
    if has_left && n.inter_at(mi_r, mi_c - 1) {
        left = n.blk_side_at(mi_r, mi_c - 1).1 >= side;
    }
    // lane-av1txctxband: the non-vacuity arm (see the rect form above).
    {
        let band_above = has_above
            && if n.above_inter[mi_c] {
                usize::from(n.above_side_mi[mi_c]) >= side
            } else {
                usize::from(n.above_txfm[mi_c]) >= side
            };
        let band_left = has_left
            && if n.left_inter[mi_r] {
                usize::from(n.left_side_mi[mi_r]) >= side
            } else {
                usize::from(n.left_txfm[mi_r]) >= side
            };
        if band_above != (has_above && usize::from(above) != 0)
            || band_left != (has_left && usize::from(left) != 0)
        {
            hit!(TXCTX_INTER_CELL_BAND_DIVERGENCE_HITS);
        }
    }
    if crate::envflags::env_flag!("EC_TXCTX") {
        eprintln!(
            "EC_TXCTX mi={mi_r},{mi_c} own={side} ha={has_above} hl={has_left} \
             above_txfm={} left_txfm={} above_inter={} left_inter={} above_side={} left_side={} \
             above={above} left={left} | cell_inter ab={} lb={} ab_side={} lb_side={}",
            n.above_txfm[mi_c],
            n.left_txfm[mi_r],
            n.above_inter[mi_c],
            n.left_inter[mi_r],
            n.above_side_mi[mi_c],
            n.left_side_mi[mi_r],
            if has_above { n.inter_at(mi_r - 1, mi_c) } else { false },
            if has_left { n.inter_at(mi_r, mi_c - 1) } else { false },
            if has_above { n.blk_side_at(mi_r - 1, mi_c).0 } else { 0 },
            if has_left { n.blk_side_at(mi_r, mi_c - 1).1 } else { 0 },
        );
    }
    match (has_above, has_left) {
        (true, true) => usize::from(above) + usize::from(left),
        (true, false) => usize::from(above),
        (false, true) => usize::from(left),
        (false, false) => 0,
    }
}

/// `read_tx_size`/`read_selected_tx_size` (spec 5.11.16): under
/// `TxMode::Select`, an intra block's `tx_depth` symbol, resolved straight to
/// the transform's own side in pixels (`side >> depth`) -- this decoder's
/// four square categories (8/16/32/64) each halve at most twice, matching
/// libaom's own per-size depth cap (`cdf::TX_SIZE_CAT0`..`TX_SIZE_CAT3`).
pub(super) fn read_tx_size(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    n: &Neighbours,
    at_mi: (usize, usize),
    side: usize,
    fctx: &crate::decode::FrameCtx,
) -> usize {
    // libaom `read_tx_size`'s FIRST line (decodeframe.c:1203):
    // `if (xd->lossless[xd->mi[0]->segment_id]) return TX_4X4;` -- before the
    // `tx_mode == TX_MODE_SELECT` test and before any symbol. Invisible while
    // every lossless frame was ALSO `CodedLossless` (its `tx_mode` is forced
    // `Only4x4`, so this function never ran); a MIXED frame keeps
    // `TxMode::Select`, so its lossless segments reach the symbol read and
    // must take the early return (lane-av1mixloss).
    if lossless(fctx) {
        return 4;
    }
    // `max_txsize_rect_lookup[bsize]` (libaom `read_selected_tx_size` /
    // `get_tx_size_context`): a block wider than 64 still tops out at
    // TX_64X64, so BLOCK_128X128 reads BLOCK_64X64's own category (its depth
    // chain 64->32->16->8->4 is the same four steps) and resolves against 64,
    // never against its own 128 side (lane-sb128b r1).
    let max_tx = side.min(64);
    let ctx = tx_size_context_txfm(n, at_mi, max_tx);
    let depth = match max_tx {
        8 => dec.symbol(&mut cdfs.tx_size_cat0[ctx]),
        16 => dec.symbol(&mut cdfs.tx_size_cat1[ctx]),
        32 => dec.symbol(&mut cdfs.tx_size_cat2[ctx]),
        64 => dec.symbol(&mut cdfs.tx_size_cat3[ctx]),
        _ => unreachable!("decode_block/decode_leaf8 only call this at 8/16/32/64/128"),
    };
    if crate::envflags::env_flag!("EC_TRACE_MODE_STEP") {
        let (rng, _) = dec.debug_state();
        let (mi_r, mi_c) = at_mi;
        eprintln!(
            "EC_ISTEP mi_row={mi_r} mi_col={mi_c} name=tx_depth val={depth} ctx={ctx} rng={rng}"
        );
    }
    if depth != 0 {
        hit!(TX_DEPTH_HITS);
    }
    max_tx >> depth
}

/// `txfm_partition_context` (libaom `av1_common_int.h`): an inter block's
/// `txfm_split` context -- a category from the block's own largest square
/// transform and whether this unit already sits below it, times three, plus
/// the two neighbour terms (whether the transform last written above/left of
/// this unit is narrower/shorter than the unit itself).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(super) fn txfm_partition_ctx(above_px: u8, left_px: u8, blk_max_px: usize, tx_px: usize) -> usize {
    txfm_partition_ctx_rect(above_px, left_px, blk_max_px, tx_px, tx_px)
}

/// [`txfm_partition_ctx`] for a *rectangular* transform (lane-inter4 r2):
/// libaom's `txfm_partition_context` compares the above context against
/// `tx_size_wide` and the left one against `tx_size_high`, and takes its
/// category from `txsize_sqr_up_map[tx_size]` -- all three collapse to one
/// side for a square transform, which is why the square form above is a
/// special case of this one.
pub(crate) fn txfm_partition_ctx_rect(
    above_px: u8,
    left_px: u8,
    blk_max_px: usize,
    tx_w: usize,
    tx_h: usize,
) -> usize {
    let above = usize::from(usize::from(above_px) < tx_w);
    let left = usize::from(usize::from(left_px) < tx_h);
    let tx_px = tx_w.max(tx_h);
    // `get_sqr_tx_size(max(block_size_wide, block_size_high))`, capped at the
    // largest transform (`TX_64X64`); `TX_4X4`..`TX_64X64` index as
    // `log2(px) - 2`.
    let max_tx = blk_max_px.min(64);
    let max_idx = max_tx.trailing_zeros() as usize - 2;
    let category = usize::from(tx_px != max_tx && max_tx > 8) + (4 - max_idx) * 2;
    category * 3 + above + left
}

/// `txfm_partition_update` (libaom `av1_common_int.h`): once a var-tx unit
/// resolves, every mi cell its *parent* unit (`txb_px`) spans records the
/// resolved transform's own size.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(super) fn txfm_partition_update(n: &mut Neighbours, at_mi: (usize, usize), tx_px: usize, txb_px: usize) {
    txfm_partition_update_rect(n, at_mi, (tx_px, tx_px), (txb_px, txb_px));
}

/// [`txfm_partition_update`] for a rectangular transform (lane-inter4 r2):
/// the left column records `tx_size_high` over the parent's own height and
/// the above row records `tx_size_wide` over its width.
/// libaom `set_txfm_ctxs` + `txfm_partition_update` (`decodeframe.c`, end of
/// `decode_token_recon_block`): EVERY block of an inter frame publishes its
/// TRANSFORM size into both `TXFM_CONTEXT` bands, intra blocks included. The
/// key frame writes neither (its readers use the deblock grid), so the
/// key-frame bodies reused by [`decode_intra_rect_in_inter`] have to do it
/// themselves when they run inside an inter frame. The 2:1 and 16x4 arms do it
/// at the call site (the transform size comes back to them); the 1:4 arms
/// ([`decode_block_rect4`] / [`decode_block_rect64`]) keep their size inside,
/// so they call this where their own depth resolves. A no-op on a key frame
/// ([`INTRA_IN_INTER_MODE`] unset), where those bands are not read at all.
///
/// lane-t900 r1 root cause: the HORZ_4 32x8 intra strip at mi(0,312) of an
/// inter frame left `left_txfm[0]` at a stale 32 where libaom's own
/// `set_txfm_ctxs` writes 8, so the intra 8x16 block at mi(0,320) read
/// `tx_size_cat1` row 1 instead of row 0 -- same depth VALUE, different CDF,
/// range 39156 where the reference has 54736 (class tx-grid-published-block-side
/// / early-return-skips-tail).
///
/// lane-av1-intrabc r3: an `allow_intrabc` KEY frame reads the same bands --
/// `read_intra_frame_mode_info` returns early for an intrabc block, so
/// [`read_block_tx_size_rect`]'s var-tx tree is the intra path's one
/// `txfm_partition_context` reader, and it reads whatever the block ABOVE and
/// the block LEFT published. [`decode_block`] has published on such a frame
/// since lane-t900 r32; the RECT strips did not, so the band still held the
/// value of an older, smaller neighbour -- measured on the 256x192 testsrc2
/// cq45 var-tx arm: the coded 16x8 intrabc strip at mi(38,48) read ctx 14
/// where libaom reads 13 (`left_ctx` 4 vs the 8 the 16x8 intra strip at
/// mi(38,44) should have published), desyncing the tile there.
pub(super) fn publish_txfm_bands_if_in_inter(
    n: &mut Neighbours,
    at_mi: (usize, usize),
    (tx_w, tx_h): (usize, usize),
    (bw, bh): (usize, usize),
    fctx: &crate::decode::FrameCtx,
) {
    if fctx
        .intra_in_inter_mode
        .with(std::cell::Cell::get)
        .is_none()
        && !allow_intrabc_frame(fctx)
    {
        return;
    }
    let (mi_r, mi_c) = at_mi;
    txfm_partition_update_rect(n, at_mi, (tx_w, tx_h), (bw, bh));
    for i in 0..bw / MI {
        if let Some(cell) = n.above_txfm.get_mut(mi_c + i) {
            *cell = tx_w as u8;
        }
    }
    for i in 0..bh / MI {
        if let Some(cell) = n.left_txfm.get_mut(mi_r + i) {
            *cell = tx_h as u8;
        }
    }
}

pub(super) fn txfm_partition_update_rect(
    n: &mut Neighbours,
    (mi_r, mi_c): (usize, usize),
    (tx_w, tx_h): (usize, usize),
    (txb_w, txb_h): (usize, usize),
) {
    if crate::envflags::env_flag!("EC_TXUPD") {
        eprintln!(
            "EC_TXUPD rect mi=({},{}) tx=({tx_w},{tx_h}) txb=({txb_w},{txb_h})",
            mi_r, mi_c
        );
    }
    for i in 0..txb_h / MI {
        if let Some(cell) = n.left_txfm.get_mut(mi_r + i) {
            *cell = tx_h as u8;
        }
    }
    for i in 0..txb_w / MI {
        if let Some(cell) = n.above_txfm.get_mut(mi_c + i) {
            *cell = tx_w as u8;
        }
    }
}

/// `set_txfm_ctxs` (libaom `av1_common_int.h`): the non-var-tx write of the
/// same contexts -- a skipped *inter* block records its own block size rather
/// than its transform size.
pub(super) fn set_txfm_ctxs(
    n: &mut Neighbours,
    (mi_r, mi_c): (usize, usize),
    tx_px: usize,
    w_mi: usize,
    h_mi: usize,
    skip_inter: bool,
) {
    if crate::envflags::env_flag!("EC_TXUPD") {
        eprintln!(
            "EC_TXUPD ctxs mi=({mi_r},{mi_c}) tx={tx_px} wh=({w_mi},{h_mi}) skip_inter={skip_inter}"
        );
    }
    let bw = if skip_inter { w_mi * MI } else { tx_px };
    let bh = if skip_inter { h_mi * MI } else { tx_px };
    for i in 0..w_mi {
        if let Some(cell) = n.above_txfm.get_mut(mi_c + i) {
            *cell = bw as u8;
        }
    }
    for i in 0..h_mi {
        if let Some(cell) = n.left_txfm.get_mut(mi_r + i) {
            *cell = bh as u8;
        }
    }
}

/// The value libaom initialises both transform contexts to -- NOT zero:
/// `av1_zero_above_context` memsets the txfm row to
/// `tx_size_wide[TX_SIZES_LARGEST]` and `av1_zero_left_context` does the same
/// with `tx_size_high[TX_SIZES_LARGEST]` (`av1_common_int.h`), i.e. 64
/// pixels, so an unwritten neighbour reads as the *widest* possible
/// transform, not the narrowest.
pub(crate) const TXFM_CTX_INIT: u8 = 64;

/// libaom's `MAX_VARTX_DEPTH`: an inter block's transform tree halves at most
/// twice below its own largest transform.
pub(super) const MAX_VARTX_DEPTH: usize = 2;

/// libaom `sub_tx_size_map[TX_SIZES_ALL]` (`common_data.c`): the transform
/// one `txfm_split` step below `tx_size`. RECTANGULAR for the 1:4 shapes
/// (`[TX_64X16] == TX_32X16`, `[TX_16X64] == TX_16X32`) and for the 4:1
/// small ones, square everywhere else.
pub(super) fn sub_tx_size_map(tx_w: usize, tx_h: usize) -> (usize, usize) {
    match (tx_w, tx_h) {
        (4, 4) => (4, 4),
        (8, 8) => (4, 4),
        (16, 16) => (8, 8),
        (32, 32) => (16, 16),
        (64, 64) => (32, 32),
        (4, 8) | (8, 4) => (4, 4),
        (8, 16) | (16, 8) => (8, 8),
        (16, 32) | (32, 16) => (16, 16),
        (32, 64) | (64, 32) => (32, 32),
        (4, 16) => (4, 8),
        (16, 4) => (8, 4),
        (8, 32) => (8, 16),
        (32, 8) => (16, 8),
        (16, 64) => (16, 32),
        (64, 16) => (32, 16),
        _ => unreachable!("not a TX_SIZES_ALL shape: {tx_w}x{tx_h}"),
    }
}

#[test]
fn sub_tx_size_map_matches_libaom() {
    // libaom `common_data.c` `sub_tx_size_map`, in TX_SIZES_ALL order.
    const TABLE: [((usize, usize), (usize, usize)); 19] = [
        ((4, 4), (4, 4)),
        ((8, 8), (4, 4)),
        ((16, 16), (8, 8)),
        ((32, 32), (16, 16)),
        ((64, 64), (32, 32)),
        ((4, 8), (4, 4)),
        ((8, 4), (4, 4)),
        ((8, 16), (8, 8)),
        ((16, 8), (8, 8)),
        ((16, 32), (16, 16)),
        ((32, 16), (16, 16)),
        ((32, 64), (32, 32)),
        ((64, 32), (32, 32)),
        ((4, 16), (4, 8)),
        ((16, 4), (8, 4)),
        ((8, 32), (8, 16)),
        ((32, 8), (16, 8)),
        ((16, 64), (16, 32)),
        ((64, 16), (32, 16)),
    ];
    for (tx, want) in TABLE {
        assert_eq!(sub_tx_size_map(tx.0, tx.1), want, "sub_tx_size_map({tx:?})");
    }
}

/// lane-av1oddheightfork3: `txfm_partition_update_rect`'s EXTENT is the
/// transform NODE (`txb`), its VALUE is the resolved leaf -- the rule
/// `txfm_partition_update` states as
/// `bsize = txsize_to_bsize[txb_size]; bh = mi_size_high[bsize];
/// bw = mi_size_wide[bsize]; left_ctx[0..bh] = tx_size_high[tx_size];
/// above_ctx[0..bw] = tx_size_wide[tx_size]` (`av1_common_int.h:1684-1695`).
///
/// The 320x236 ticket named the RECT case as the suspect ("`txsize_to_bsize`
/// for a rect `txb_size` is the awkward part"). It is not: for every
/// `TX_SIZE`, `mi_size_wide[txsize_to_bsize[t]] * MI == tx_size_wide[t]`, which
/// is what makes `txb_w / MI` the right cell count. This test pins that
/// identity from libaom's OWN `mi_size_wide`/`mi_size_high` rows (blockd.h)
/// -- transcribed, not derived from the shape -- and then pins the primitive
/// on a rect node, where an extent taken from the leaf instead of the node
/// would write 1 cell where libaom writes 2 and 1 where libaom writes 4.
#[test]
fn txfm_partition_update_rect_extent_is_the_node_not_the_leaf() {
    // libaom `blockd.h`: (bsize, mi_size_wide, mi_size_high) for every
    // `txsize_to_bsize` image a `TX_SIZE` can name.
    const BSIZES: [(&str, usize, usize); 12] = [
        ("BLOCK_4X4", 1, 1),
        ("BLOCK_4X8", 1, 2),
        ("BLOCK_8X4", 2, 1),
        ("BLOCK_8X8", 2, 2),
        ("BLOCK_8X16", 2, 4),
        ("BLOCK_16X8", 4, 2),
        ("BLOCK_16X16", 4, 4),
        ("BLOCK_16X32", 4, 8),
        ("BLOCK_32X16", 8, 4),
        ("BLOCK_32X32", 8, 8),
        ("BLOCK_4X16", 1, 4),
        ("BLOCK_16X4", 4, 1),
    ];
    for (name, mi_w, mi_h) in BSIZES {
        // `tx_size_wide`/`tx_size_high` of the TX_SIZE that maps to this
        // bsize, and the same quantity as a pixel size.
        let (tx_w, tx_h) = (mi_w * MI, mi_h * MI);
        assert_eq!(tx_w / MI, mi_w, "{name}: width identity");
        assert_eq!(tx_h / MI, mi_h, "{name}: height identity");
    }

    // The primitive on a RECT node: an 8x16 node whose TX_4X4 early-return
    // leaf resolved (libaom `read_tx_size_vartx`'s `sub_txs == TX_4X4` arm,
    // which calls `txfm_partition_update(..., TX_4X4, tx_size)` with the
    // PARENT as the node).
    let fctx = &crate::decode::FrameCtx::new();
    let mut n = Neighbours::new(4, 4, 32, 32, fctx);
    for cell in n.above_txfm.iter_mut() {
        *cell = 0;
    }
    for cell in n.left_txfm.iter_mut() {
        *cell = 0;
    }
    txfm_partition_update_rect(&mut n, (4, 4), (4, 4), (8, 16));
    assert_eq!(
        &n.above_txfm[4..6],
        &[4, 4],
        "the node's OWN width (2 mi) carries the leaf's width"
    );
    assert_eq!(
        &n.left_txfm[4..8],
        &[4, 4, 4, 4],
        "the node's OWN height (4 mi) carries the leaf's height"
    );
    assert_eq!(n.above_txfm[6], 0, "and nothing past the node's extent");
    assert_eq!(n.left_txfm[8], 0, "and nothing past the node's extent");
}

/// The domain of var-tx leaf sizes, enumerated against the two refusals that
/// name a leaf "larger than 32x32" / "larger than 64x64".
///
/// [`read_var_tx_size`] emits a leaf in exactly three places, and each one is
/// either the unit it was entered with or a `sub_tx_size_map` step below it:
/// the depth cap, the `!split` return, and libaom's `sub_txs == TX_4X4` early
/// return. So the leaf set reachable from an entry unit is that unit plus the
/// chain of `sub_tx_size_map` steps under it, capped at `MAX_VARTX_DEPTH` --
/// which this test enumerates, and over which no leaf is ever larger than the
/// entry.
///
/// That closes both refusals:
///
/// * [`read_block_tx_size_rect`]'s `bw.max(bh) > 64` branch enters every unit
///   at TX_64X64 (`max_txsize_rect_lookup[BLOCK_128X64]`), so "larger than
///   64x64" names a leaf the tree cannot produce;
/// * [`read_block_tx_size`] enters at `side.min(64)` with a ceiling of 32
///   below the 128 root. The only entry that exceeds that ceiling is 64x64,
///   reached only when `side == 64`, and then the block is ONE unit whose only
///   above-ceiling leaf is the whole 64x64 unit itself -- which is the `single`
///   case the refusal is guarded by.
#[test]
fn a_var_tx_tree_never_presents_a_leaf_larger_than_the_unit_it_entered() {
    // lane-av1refusalspan: this gate is the PROOF for both var-tx refusals, so
    // it names both and pins both guard sites. The walk below proves the leaf
    // domain; it never said which two strings that domain was about.
    for refusal in [
        "an inter var-tx tree with a leaf transform larger than 32x32",
        "an inter var-tx tree with a leaf transform larger than 64x64",
    ] {
        assert!(
            crate::refusal_inventory::pins_refusal(crate::test_source::module_source("decode"), refusal),
            "this gate proves {refusal:?}, but that string is no longer in decode.rs"
        );
    }
    // Every leaf `read_var_tx_size` can reach from `entry`, as (depth, w, h).
    fn reachable(entry: (usize, usize), depth: usize, out: &mut Vec<(usize, usize, usize)>) {
        out.push((depth, entry.0, entry.1));
        if depth == MAX_VARTX_DEPTH {
            return;
        }
        let sub = sub_tx_size_map(entry.0, entry.1);
        if sub == (4, 4) {
            out.push((depth + 1, 4, 4));
            return;
        }
        reachable(sub, depth + 1, out);
    }

    for entry in [(4, 4), (8, 8), (16, 16), (32, 32), (64, 64)] {
        let mut leaves = Vec::new();
        reachable(entry, 0, &mut leaves);
        for (depth, w, h) in &leaves {
            assert!(
                w.max(h) <= &entry.0.max(entry.1),
                "a {}x{} unit reaches a {w}x{h} leaf at depth {depth}",
                entry.0,
                entry.1
            );
        }
        // Above-ceiling leaves of a 64x64 entry: the entry itself, at depth 0.
        if entry == (64, 64) {
            let above: Vec<&(usize, usize, usize)> =
                leaves.iter().filter(|(_, w, h)| w.max(h) > &32).collect();
            assert_eq!(
                above,
                [&(0usize, 64usize, 64usize)],
                "64x64 entry, leaves above 32"
            );
        }
    }

    // The entry size and ceiling each caller uses, over the block sides that
    // reach them (`read_block_tx_size`'s square path; the 128-root rect path
    // enters at a fixed 64).
    for side in [8usize, 16, 32, 64, 128] {
        let max_tx = side.min(64);
        let ceiling = if side > 64 { 64 } else { 32 };
        let units = (side / max_tx) * (side / max_tx);
        if max_tx <= ceiling {
            continue;
        }
        assert_eq!(
            (side, units),
            (64, 1),
            "a {side}px block enters var-tx at {max_tx} over a ceiling of {ceiling} in \
             {units} units -- an above-ceiling leaf there would NOT be the `single` case"
        );
    }
}

/// `read_var_tx_size` (spec 5.11.17, libaom `decodeframe.c`
/// `read_tx_size_vartx`): the recursive `txfm_split` tree of one
/// max-transform-sized unit of an inter block, collecting the resolved leaf
/// transform units into `out` as `(row offset, col offset, side)` in mi units
/// -- exactly the order `decode_reconstruct_tx` later reads their
/// coefficients in. `blk_row`/`blk_col` are mi offsets inside the block;
/// `max_w_mi`/`max_h_mi` clip it to the true frame edge (`max_block_wide`/
/// `max_block_high`).
#[allow(clippy::too_many_arguments)]
pub(super) fn read_var_tx_size(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    n: &mut Neighbours,
    at_mi: (usize, usize),
    blk_max_px: usize,
    (max_w_mi, max_h_mi): (usize, usize),
    (tx_w, tx_h): (usize, usize),
    depth: usize,
    blk_row: usize,
    blk_col: usize,
    out: &mut Vec<(usize, usize, usize, usize)>,
) {
    if blk_row >= max_h_mi || blk_col >= max_w_mi {
        return;
    }
    let unit = (at_mi.0 + blk_row, at_mi.1 + blk_col);
    if depth == MAX_VARTX_DEPTH {
        out.push((blk_row, blk_col, tx_w, tx_h));
        txfm_partition_update_rect(n, unit, (tx_w, tx_h), (tx_w, tx_h));
        return;
    }
    let ctx = txfm_partition_ctx_rect(
        n.above_txfm[unit.1],
        n.left_txfm[unit.0],
        blk_max_px,
        tx_w,
        tx_h,
    );
    let split = dec.symbol(&mut cdfs.txfm_partition[ctx]) == 1;
    hit!(TXFM_SPLIT_READS);
    if crate::envflags::env_flag!("EC_TRACE_MODE_STEP") {
        let (rng, _) = dec.debug_state();
        eprintln!(
            "EC_ISTEP mi_row={} mi_col={} name=txfm_split val={} ctx={ctx} rng={rng}",
            unit.0,
            unit.1,
            u8::from(split)
        );
    }
    if !split {
        out.push((blk_row, blk_col, tx_w, tx_h));
        txfm_partition_update_rect(n, unit, (tx_w, tx_h), (tx_w, tx_h));
        return;
    }
    hit!(TXFM_SPLIT_HITS);
    let (sub_w, sub_h) = sub_tx_size_map(tx_w, tx_h);
    if (sub_w, sub_h) == (4, 4) {
        // libaom's `sub_txs == TX_4X4` early return: every mi cell of the
        // unit is marked 4x4 (raster order) without recursing.
        for row in 0..tx_h / MI {
            for col in 0..tx_w / MI {
                if blk_row + row < max_h_mi && blk_col + col < max_w_mi {
                    out.push((blk_row + row, blk_col + col, 4, 4));
                }
            }
        }
        txfm_partition_update_rect(n, unit, (4, 4), (tx_w, tx_h));
        return;
    }
    for row in (0..tx_h / MI).step_by(sub_h / MI) {
        for col in (0..tx_w / MI).step_by(sub_w / MI) {
            read_var_tx_size(
                dec,
                cdfs,
                n,
                at_mi,
                blk_max_px,
                (max_w_mi, max_h_mi),
                (sub_w, sub_h),
                depth + 1,
                blk_row + row,
                blk_col + col,
                out,
            );
        }
    }
}
