//! [scratch] Tile-layout diagnosis for a VP9 IVF: print the parsed header
//! sizes and the tile size prefixes at the header's end.
//! `TILE_SRC=<ivf> cargo test -p ec-vp9 --test scratch_tiledbg -- --nocapture`
mod ivf;

use ec_vp9_syntax::Vp9Parser;

#[test]
fn tile_dbg() {
    let Ok(src) = std::env::var("TILE_SRC") else {
        println!("SKIP: set TILE_SRC=<ivf>");
        return;
    };
    let bytes = std::fs::read(&src).unwrap();
    let (_f, w, h, frames) = ivf::parse_ivf(&bytes);
    println!("ivf {w}x{h} frames {}", frames.len());
    for (i, fr) in frames.iter().enumerate().take(2) {
        let mut p = Vp9Parser::new();
        match p.parse_frame(&fr.data) {
            Ok(hdr) => {
                // Tile data starts after BOTH headers (libvpx: `data +=
                // vpx_rb_bytes_read(&rb)` then the compressed header).
                let off = hdr.uncompressed_header_size as usize + hdr.header_size_in_bytes as usize;
                println!(
                    "frame {i}: len {} uhs {} hsib {} tile cols {} rows {} mi {}x{}",
                    fr.data.len(),
                    hdr.uncompressed_header_size,
                    hdr.header_size_in_bytes,
                    hdr.tile_info.cols_log2,
                    hdr.tile_info.rows_log2,
                    hdr.mi_cols(),
                    hdr.mi_rows()
                );
                let tail = &fr.data[off..];
                println!(
                    "  tail len {} first16 {:?}",
                    tail.len(),
                    &tail[..16.min(tail.len())]
                );
                if tail.len() >= 4 {
                    let sz = u32::from_le_bytes(tail[..4].try_into().unwrap()) as usize;
                    println!(
                        "  first tile size prefix {sz} (tail-4 = {})",
                        tail.len() - 4
                    );
                }
            }
            Err(e) => println!("frame {i}: parse ERR {e}"),
        }
    }
}
