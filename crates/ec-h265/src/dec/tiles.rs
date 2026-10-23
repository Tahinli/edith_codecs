//! The tile scan of a picture (H.265 6.5.1): the column and row boundaries of
//! the tiles, the conversion between CTB raster and tile scan addresses, and the
//! tile each CTB belongs to. A picture without tiles is one tile whose scan is
//! the raster scan.

use ec_core::error::{Error, Result};
use ec_h265_syntax::Pps;

/// The CTB addressing of one picture.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TileScan {
    /// `CtbAddrRsToTs`, indexed by raster address.
    pub rs_to_ts: Vec<usize>,
    /// `CtbAddrTsToRs`, indexed by tile scan address.
    pub ts_to_rs: Vec<usize>,
    /// `TileId` of each CTB, indexed by raster address.
    pub tile_of_rs: Vec<u16>,
}

impl TileScan {
    /// One tile covering `cols` by `rows` CTBs: the raster scan.
    pub fn single(cols: usize, rows: usize) -> TileScan {
        let total = cols * rows;
        TileScan {
            rs_to_ts: (0..total).collect(),
            ts_to_rs: (0..total).collect(),
            tile_of_rs: vec![0; total],
        }
    }

    /// The tile scan of a `cols` by `rows` CTB picture under `pps` (6.5.1).
    pub fn from_pps(pps: &Pps, cols: usize, rows: usize) -> Result<TileScan> {
        if !pps.tiles_enabled {
            return Ok(TileScan::single(cols, rows));
        }
        let n_cols = pps.num_tile_columns_minus1 as usize + 1;
        let n_rows = pps.num_tile_rows_minus1 as usize + 1;
        if n_cols > cols || n_rows > rows {
            return Err(Error::corrupt(format!(
                "HEVC PPS: {n_cols}x{n_rows} tiles in a {cols}x{rows} CTB picture"
            )));
        }
        let col_widths = if pps.uniform_spacing {
            uniform(cols, n_cols)
        } else {
            explicit(cols, &pps.column_width_minus1, n_cols)?
        };
        let row_heights = if pps.uniform_spacing {
            uniform(rows, n_rows)
        } else {
            explicit(rows, &pps.row_height_minus1, n_rows)?
        };
        Self::new(cols, rows, &col_widths, &row_heights)
    }

    /// The tile scan for the given tile column widths and row heights (in CTBs).
    pub fn new(
        cols: usize,
        rows: usize,
        col_widths: &[usize],
        row_heights: &[usize],
    ) -> Result<TileScan> {
        let total = cols * rows;
        let mut ts_to_rs = Vec::with_capacity(total);
        let mut tile_of_rs = vec![0u16; total];
        let mut tile: u16 = 0;
        let mut y0 = 0;
        for &h in row_heights {
            let mut x0 = 0;
            for &w in col_widths {
                for y in y0..y0 + h {
                    for x in x0..x0 + w {
                        let rs = y * cols + x;
                        ts_to_rs.push(rs);
                        if let Some(id) = tile_of_rs.get_mut(rs) {
                            *id = tile;
                        }
                    }
                }
                x0 += w;
                tile = tile.saturating_add(1);
            }
            y0 += h;
        }
        if ts_to_rs.len() != total || sizes_mismatch(col_widths, cols, row_heights, rows) {
            return Err(Error::corrupt(
                "HEVC PPS: tile sizes do not cover the picture".to_string(),
            ));
        }
        let mut rs_to_ts = vec![0usize; total];
        for (ts, &rs) in ts_to_rs.iter().enumerate() {
            if let Some(slot) = rs_to_ts.get_mut(rs) {
                *slot = ts;
            }
        }
        Ok(TileScan {
            rs_to_ts,
            ts_to_rs,
            tile_of_rs,
        })
    }

    /// Whether tile scan address `ts` is the first CTB of a tile.
    pub fn is_tile_start(&self, ts: usize) -> bool {
        ts == 0 || self.tile_at_ts(ts) != self.tile_at_ts(ts - 1)
    }

    /// Whether raster address `rs` starts a CTB row within its tile (the first
    /// CTB of a row of the picture, or of a tile row).
    pub fn is_row_start(&self, rs: usize, cols: usize) -> bool {
        rs.is_multiple_of(cols) || self.tile_of_rs.get(rs) != self.tile_of_rs.get(rs - 1)
    }

    /// `TileId` of the CTB at tile scan address `ts`.
    fn tile_at_ts(&self, ts: usize) -> Option<u16> {
        self.ts_to_rs
            .get(ts)
            .and_then(|&rs| self.tile_of_rs.get(rs).copied())
    }
}

/// Column widths (or row heights) of `n` uniformly spaced tiles over `total` CTBs (6-3).
fn uniform(total: usize, n: usize) -> Vec<usize> {
    (0..n)
        .map(|i| ((i + 1) * total) / n - (i * total) / n)
        .collect()
}

/// Explicit widths: the first `n - 1` from `minus1`, the last takes the rest (6-4, 6-5).
fn explicit(total: usize, minus1: &[u32], n: usize) -> Result<Vec<usize>> {
    let mut sizes = Vec::with_capacity(n);
    let mut used = 0usize;
    for i in 0..n - 1 {
        let v = minus1.get(i).map_or(0, |&v| v as usize) + 1;
        used += v;
        sizes.push(v);
    }
    if used >= total {
        return Err(Error::corrupt(
            "HEVC PPS: explicit tile sizes leave no room for the last tile".to_string(),
        ));
    }
    sizes.push(total - used);
    Ok(sizes)
}

/// Whether the sizes fail to add up to the picture's CTB dimensions.
fn sizes_mismatch(cols_w: &[usize], cols: usize, rows_h: &[usize], rows: usize) -> bool {
    cols_w.iter().sum::<usize>() != cols || rows_h.iter().sum::<usize>() != rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_split_matches_6_3() {
        // 10 CTBs over 3 tiles: 3, 3, 4.
        assert_eq!(uniform(10, 3), vec![3, 3, 4]);
    }

    #[test]
    fn two_by_two_tiles_scan_tile_by_tile() {
        // A 4x2 CTB picture with a 2+2 column split and a single row split.
        let t = TileScan::new(4, 2, &[2, 2], &[2]).expect("valid tiles");
        assert_eq!(t.ts_to_rs, vec![0, 1, 4, 5, 2, 3, 6, 7]);
        assert_eq!(t.tile_of_rs, vec![0, 0, 1, 1, 0, 0, 1, 1]);
        assert_eq!(t.rs_to_ts[4], 2);
        assert!(t.is_tile_start(0));
        // Tile 1 starts at tile scan address 4 (raster CTB 2).
        assert!(t.is_tile_start(4));
        assert!(!t.is_tile_start(1));
        assert!(!t.is_tile_start(2));
    }

    #[test]
    fn single_tile_is_raster() {
        let t = TileScan::single(3, 2);
        assert_eq!(t.ts_to_rs, (0..6).collect::<Vec<_>>());
        assert!(t.is_row_start(3, 3));
        assert!(!t.is_row_start(4, 3));
    }
}
