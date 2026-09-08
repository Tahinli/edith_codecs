//! Syntax census of one AV1 stream: what it actually codes, per frame type.
//!
//! `EC_AV1_BITCENSUS=1 cargo run --release -p ec-av1 --example syntax_census \
//!     -- stream.obu [source.yuv]`
//!
//! The stream may be a raw OBU stream or an IVF file (unwrapped here). The
//! optional second argument is the ENCODER'S INPUT as planar 8-bit yuv420p at
//! the coded size, and turns on a per-displayed-frame PSNR -- computed here,
//! against the decoded pictures, because ffmpeg's own PSNR of an OBU stream
//! reads a constant ~28.8 dB (no timing in an OBU stream; ledger dead-end).
//!
//! Every number comes from `ec_av1::census`, i.e. from the decoder, so our
//! stream and any other encoder's stream are measured by the same reader.

use ec_av1::census::Frame;
use std::collections::BTreeMap;

/// The payloads of an IVF file's frames concatenated, or the data unchanged
/// when it is already a raw OBU stream.
fn unwrap_ivf(data: &[u8]) -> Vec<u8> {
    if data.len() < 32 || &data[..4] != b"DKIF" {
        return data.to_vec();
    }
    let header = u16::from_le_bytes([data[6], data[7]]) as usize;
    let (mut pos, mut out) = (header, Vec::new());
    while pos + 12 <= data.len() {
        let len = u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
            as usize;
        pos += 12;
        if pos + len > data.len() {
            break;
        }
        out.extend_from_slice(&data[pos..pos + len]);
        pos += len;
    }
    out
}

fn psnr(a: &[u16], b: &[u16]) -> f64 {
    let se: f64 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| {
            let d = f64::from(x) - f64::from(y);
            d * d
        })
        .sum();
    match se {
        0.0 => 99.0,
        _ => 10.0 * (255.0 * 255.0 * a.len() as f64 / se).log10(),
    }
}

/// Which broad syntax family a CDF table belongs to.
fn family_group(name: &str) -> &'static str {
    const COEFF: [&str; 7] =
        ["txb_skip", "eob_pt", "eob_extra", "base_", "base_eob", "br_", "dc_sign"];
    if name.contains("tx_type") {
        return "txtype";
    }
    if COEFF.iter().any(|p| name.starts_with(p)) {
        return "coeff";
    }
    if name.starts_with("mv_") || name.starts_with("dv_") {
        return "mv";
    }
    if name.starts_with("partition") || name.starts_with("txfm_partition") {
        return "partition";
    }
    if name.starts_with("tx_size") {
        return "txsize";
    }
    if name == "literal" || name == "unregistered" {
        return "literal";
    }
    "mode"
}

/// Sums a set of frames into one row's worth of census.
fn merge(frames: &[&Frame]) -> Frame {
    let mut out = Frame::default();
    for f in frames {
        out.bytes += f.bytes;
        for (k, v) in &f.blocks {
            *out.blocks.entry(*k).or_default() += v;
        }
        for (k, v) in &f.tx {
            *out.tx.entry(*k).or_default() += v;
        }
        for (k, v) in &f.tx_type {
            *out.tx_type.entry(*k).or_default() += v;
        }
        for (k, v) in &f.families {
            let e = out.families.entry(k).or_default();
            e.0 += v.0;
            e.1 += v.1;
        }
        for i in 0..3 {
            out.area[i] += f.area[i];
        }
        for i in 0..8 {
            out.refs[i] += f.refs[i];
        }
        for i in 0..4 {
            out.modes[i] += f.modes[i];
        }
        for i in 0..6 {
            out.mv[i] += f.mv[i];
        }
        out.skip_area += f.skip_area;
    }
    out
}

fn pct(n: u64, total: u64) -> String {
    format!("{:.1}%", 100.0 * n as f64 / total.max(1) as f64)
}

const REFS: [&str; 8] =
    ["INTRA", "LAST", "LAST2", "LAST3", "GOLDEN", "BWDREF", "ALTREF2", "ALTREF"];

fn report(label: &str, f: &Frame, frames: usize) {
    let area = f.area.iter().sum::<u64>().max(1);
    println!("\n== {label} ({frames} frames, {} bytes) ==", f.bytes);
    println!(
        "  area: intra {} single-ref {} compound {} | skip {}",
        pct(f.area[0], area),
        pct(f.area[1], area),
        pct(f.area[2], area),
        pct(f.skip_area, area),
    );
    println!(
        "  refs (area): {}",
        REFS.iter()
            .enumerate()
            .filter(|(i, _)| f.refs[*i] > 0)
            .map(|(i, n)| format!("{n} {}", pct(f.refs[i], area)))
            .collect::<Vec<_>>()
            .join("  "),
    );
    let modes = f.modes.iter().sum::<u64>();
    println!(
        "  single-ref modes: NEW {} GLOBAL {} NEAREST {} NEAR {} (of {modes})",
        pct(f.modes[0], modes),
        pct(f.modes[1], modes),
        pct(f.modes[2], modes),
        pct(f.modes[3], modes),
    );
    let mvs = f.mv.iter().sum::<u64>();
    println!(
        "  coded mv |max| (1/8 pel): 0 {} <=8 {} <=32 {} <=128 {} <=512 {} >512 {} (of {mvs})",
        pct(f.mv[0], mvs),
        pct(f.mv[1], mvs),
        pct(f.mv[2], mvs),
        pct(f.mv[3], mvs),
        pct(f.mv[4], mvs),
        pct(f.mv[5], mvs),
    );
    let blocks: u64 = f.blocks.values().sum();
    let mut by_size: Vec<_> = f.blocks.iter().collect();
    by_size.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    println!(
        "  blocks ({blocks} total): {}",
        by_size
            .iter()
            .take(10)
            .map(|((w, h), n)| format!("{w}x{h} {}", pct(**n, blocks)))
            .collect::<Vec<_>>()
            .join("  "),
    );
    let txs: u64 = f.tx.values().sum();
    let mut by_tx: Vec<_> = f.tx.iter().collect();
    by_tx.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    println!(
        "  published luma tx ({txs}): {}",
        by_tx
            .iter()
            .take(8)
            .map(|((w, h), n)| format!("{w}x{h} {}", pct(**n, txs)))
            .collect::<Vec<_>>()
            .join("  "),
    );
    let tts: u64 = f.tx_type.values().sum();
    println!(
        "  tx_type symbols ({tts}): {}",
        f.tx_type
            .iter()
            .map(|((len, sym), n)| format!("set{len}/{sym} {}", pct(*n, tts)))
            .collect::<Vec<_>>()
            .join("  "),
    );
    // lane-libcen: the coding TOOLS, by name, so a level row says which of
    // them the stream codes at all -- a table absent here is a tool this
    // encoder never wrote (class `gate-blind-to-feature`).
    const TOOLS: [&str; 16] = [
        "motion_mode",
        "obmc",
        "interintra",
        "wedge_interintra",
        "wedge_idx",
        "compound_type",
        "comp_group_idx",
        "compound_idx",
        "switchable_interp",
        "palette_y_mode",
        "palette_uv_mode",
        "intrabc",
        "filter_intra",
        "cfl_alpha",
        "delta_q",
        "segment_id",
    ];
    println!(
        "  tools (symbols/bits): {}",
        TOOLS
            .iter()
            .map(|t| {
                let (n, bits) = f.families.get(t).copied().unwrap_or((0, 0.0));
                format!("{t} {n}/{bits:.0}")
            })
            .collect::<Vec<_>>()
            .join("  "),
    );

    // Bits by family group, then the ten biggest tables inside them.
    let mut groups: BTreeMap<&str, (u64, f64)> = BTreeMap::new();
    for (name, (n, bits)) in &f.families {
        let e = groups.entry(family_group(name)).or_default();
        e.0 += n;
        e.1 += bits;
    }
    let total_bits: f64 = groups.values().map(|g| g.1).sum();
    println!("  bits by family (census total {:.0} bits vs {} payload bits):", total_bits, f.bytes * 8);
    for (g, (n, bits)) in &groups {
        println!(
            "    {g:<10} {:>12.0} bits  {:>6}  ({n} symbols)",
            bits,
            format!("{:.1}%", 100.0 * bits / total_bits.max(1.0))
        );
    }
    let mut tables: Vec<_> = f.families.iter().collect();
    tables.sort_by(|a, b| b.1 .1.total_cmp(&a.1 .1));
    // lane-cen3: how many tables the row names. The default 8 hides the
    // mode family's own split (a level can spend 40% of its bits on `mode`
    // with no mode table in its top 8), so a census that has to rank
    // per-table gaps between two encoders sets `EC_CENSUS_TABLES`.
    let top = std::env::var("EC_CENSUS_TABLES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8usize);
    println!(
        "    top tables: {}",
        tables
            .iter()
            .take(top)
            .map(|(name, (_, bits))| format!("{name} {:.1}%", 100.0 * bits / total_bits.max(1.0)))
            .collect::<Vec<_>>()
            .join("  "),
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!("usage: syntax_census <stream.obu|stream.ivf> [source.yuv]");
        std::process::exit(2);
    };
    assert!(ec_av1::census::on(), "set EC_AV1_BITCENSUS=1");
    let data = unwrap_ivf(&std::fs::read(path).expect("stream"));
    let pictures = ec_av1::stream::decode_stream(&data).expect("decode");
    let frames = ec_av1::census::take();

    println!("# syntax census of {path} ({} bytes)", data.len());
    println!("\n| frame | kind | q | bytes | lf | cdef bits/y/uv | lr | dq | seg |");
    println!("|---|---|---|---|---|---|---|---|---|");
    for f in &frames {
        println!(
            "| {} | {} | {} | {} | {}/{} | {}/{}/{} | {:?} | {} | {} |",
            f.idx,
            f.kind,
            f.qindex,
            f.bytes,
            f.lf[0],
            f.lf[1],
            f.cdef[0],
            f.cdef[1],
            f.cdef[2],
            f.lr,
            u8::from(f.delta_q),
            u8::from(f.segmentation),
        );
    }
    // PSNR is per DISPLAYED picture, in display order, which is not the order
    // the frames above are coded in (a pyramid codes a hidden ARF ahead of the
    // leaves that predict from it, and re-outputs it later with
    // `show_existing_frame`) -- so it is its own table rather than a column.
    let mut psnrs: Vec<f64> = Vec::new();
    if let Some(src) = args.get(1).map(|p| std::fs::read(p).expect("source")) {
        let mut sum = 0.0;
        let mut rows = Vec::new();
        for (i, pic) in pictures.iter().enumerate() {
            let (luma, chroma) = (pic.width * pic.height, pic.width * pic.height / 4);
            let frame_len = luma + 2 * chroma;
            let Some(b) = src.get(i * frame_len..(i + 1) * frame_len) else { break };
            let want: Vec<u16> = b[..luma].iter().map(|&v| u16::from(v)).collect();
            let p = psnr(&pic.y, &want);
            psnrs.push(p);
            sum += p;
            rows.push(format!("{i}:{p:.2}"));
        }
        println!(
            "\nPSNR-Y display order ({} pictures): {}\nPSNR-Y mean {:.3} dB over {} pictures",
            pictures.len(),
            rows.join(" "),
            sum / rows.len() as f64,
            rows.len(),
        );
    }

    // lane-arfcen: one full report PER CODED FRAME, so an ARF of ours can be
    // put next to the reference encoder's ARF at the same display position.
    if std::env::var("EC_CENSUS_PERFRAME").as_deref() == Ok("1") {
        for f in &frames {
            let p = psnrs.get(f.order_hint as usize).copied().unwrap_or(f64::NAN);
            let refs: Vec<String> = (0..7)
                .filter(|i| f.refs[i + 1] > 0)
                .map(|i| {
                    format!(
                        "{}@{:+}",
                        REFS[i + 1],
                        i64::from(f.ref_hints[i]) - i64::from(f.order_hint),
                    )
                })
                .collect();
            report(
                &format!(
                    "frame {} {} hint {} q {} PSNR-Y {p:.2} dB refs [{}]",
                    f.idx,
                    f.kind,
                    f.order_hint,
                    f.qindex,
                    refs.join(" "),
                ),
                f,
                1,
            );
        }
    }

    for kind in ["key", "arf", "leaf"] {
        let set: Vec<&Frame> = frames.iter().filter(|f| f.kind == kind).collect();
        if !set.is_empty() {
            report(kind, &merge(&set), set.len());
        }
    }
    report("ALL", &merge(&frames.iter().collect::<Vec<_>>()), frames.len());
    // The self-check: every coded block is published exactly once, so the
    // censused area must be the frame's own mi area (a funnel that misses a
    // block class would silently under-count every share above).
    if let Some(pic) = pictures.first() {
        let all = merge(&frames.iter().collect::<Vec<_>>());
        let want = (pic.width.div_ceil(4) * pic.height.div_ceil(4) * frames.len()) as u64;
        let got = all.area.iter().sum::<u64>();
        println!(
            "\ncoverage: censused block area {got} of {want} mi ({:.2}%)",
            100.0 * got as f64 / want as f64
        );
    }
}
