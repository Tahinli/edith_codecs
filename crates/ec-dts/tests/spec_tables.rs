//! The constant tables in `tables_data.rs` against the numbers printed in
//! ETSI TS 102 114 V1.6.1 (2019-08), committed in `spec_ts102114_tables.txt`
//! (written by `tools/gen_dts_tables.py`, with the clause of every table).
//!
//! The conversions from printed number to Rust value are repeated here
//! independently of the generator.

use ec_dts::tables_data::*;
use std::collections::HashMap;

const SPEC: &str = include_str!("spec_ts102114_tables.txt");

struct Spec {
    tables: HashMap<String, Vec<String>>,
    huff: HashMap<String, Vec<(i32, u32, u64)>>,
}

fn load() -> Spec {
    let mut s = Spec {
        tables: HashMap::new(),
        huff: HashMap::new(),
    };
    for line in SPEC.lines().filter(|l| !l.starts_with('#') && !l.is_empty()) {
        let parts: Vec<&str> = line.splitn(3, " | ").collect();
        let head: Vec<&str> = parts[0].split(' ').collect();
        match head[0] {
            "table" => {
                s.tables.insert(
                    head[1].to_string(),
                    parts[2].split(' ').map(str::to_string).collect(),
                );
            }
            "huff" => {
                let e = parts[2]
                    .split(' ')
                    .map(|t| {
                        let v: Vec<&str> = t.split(':').collect();
                        (v[0].parse().unwrap(), v[1].parse().unwrap(), v[2].parse().unwrap())
                    })
                    .collect();
                s.huff.insert(head[1].to_string(), e);
            }
            other => panic!("unknown record {other}"),
        }
    }
    s
}

/// Integers with "invalid" read as 0 and zero padding to `n`.
fn ints(v: &[String], n: usize) -> Vec<u32> {
    let mut o: Vec<u32> = v
        .iter()
        .map(|x| if x == "invalid" { 0 } else { x.parse().unwrap() })
        .collect();
    o.resize(n, 0);
    o
}

#[test]
fn integer_tables_match_spec() {
    let s = load();
    assert_eq!(SCALE_FACTOR_QUANT6.to_vec(), ints(&s.tables["SCALE_FACTOR_QUANT6"], 64)); // D.1.1
    assert_eq!(SCALE_FACTOR_QUANT7.to_vec(), ints(&s.tables["SCALE_FACTOR_QUANT7"], 128)); // D.1.2
    assert_eq!(LOSSY_QUANT.to_vec(), ints(&s.tables["LOSSY_QUANT"], 32)); // D.2.1
    assert_eq!(LOSSLESS_QUANT.to_vec(), ints(&s.tables["LOSSLESS_QUANT"], 32)); // D.2.2
    // Table 5-26: ABITS 0 is 1 here (spec 0), 27..31 invalid.
    let mut ql = ints(&s.tables["QUANT_LEVELS"], 32);
    ql[0] = 1;
    assert_eq!(QUANT_LEVELS.to_vec(), ql);
    // Table 5-27: adjustment values x 2^22.
    let adj: Vec<u32> = s.tables["SCALE_FACTOR_ADJ"]
        .iter()
        .map(|x| (x.parse::<f64>().unwrap() * 4194304.0) as u32)
        .collect();
    assert_eq!(SCALE_FACTOR_ADJ.to_vec(), adj);
    // D.11: DmixTable preceded by a 0 entry.
    let mut dm = vec![0u32];
    dm.extend(ints(&s.tables["DMIXTABLE"], 241));
    assert_eq!(DMIXTABLE.iter().map(|&x| x as u32).collect::<Vec<_>>(), dm);
}

#[test]
fn float_and_fir_tables_match_spec() {
    let s = load();
    // D.9: round-half-up(coef * 2^29), cosine modulation sign per 32 taps.
    let fir = &s.tables["FIR_64"];
    assert_eq!(fir.len(), 1024);
    for (i, t) in fir.iter().enumerate() {
        let v = (t.parse::<f64>().unwrap() * 536870912.0 + 0.5).floor() as i32;
        let v = if (i / 32) % 4 == 0 { v } else { -v };
        assert_eq!(FIR_64[i], v, "FIR_64[{i}]");
    }
    // D.8, 128x column, polyphase order.
    let lfe = &s.tables["LFE_FIR_128"];
    assert_eq!(lfe.len(), 512);
    for (k, got) in LFE_FIR_128.iter().enumerate() {
        let (q, p) = (k / 4, k % 4);
        let idx = [q, 128 + q, 255 - q, 127 - q][p];
        let want: f32 = lfe[idx].parse().unwrap();
        assert_eq!(got.to_bits(), want.to_bits(), "LFE_FIR_128[{k}]");
    }
}

#[test]
fn huffman_books_match_spec() {
    let s = load();
    let mut order: Vec<(String, i32)> = Vec::new(); // (table, symbol = level + offset)
    for (n, groups, off) in [
        (3, "A", 1),
        (5, "ABC", 2),
        (7, "ABC", 3),
        (9, "ABC", 4),
        (13, "ABC", 6),
        (17, "ABCDEFG", 8),
        (25, "ABCDEFG", 12),
        (33, "ABCDEFG", 16),
        (65, "ABCDEFG", 32),
        (129, "ABCDEFG", 64),
    ] {
        for g in groups.chars() {
            order.push((format!("{g}{n}"), off));
        }
    }
    for g in "ABCDE".chars() {
        order.push((format!("{g}12"), -1)); // level column is ABITS = symbol + 1
    }
    for g in "ABCDE".chars() {
        order.push((format!("S{g}129"), 64));
    }
    for g in "ABCD".chars() {
        order.push((format!("{g}4"), 0));
    }
    assert_eq!(s.huff.len(), order.len());
    let mut got: Vec<(u8, u8)> = Vec::new();
    for (name, off) in &order {
        let mut e = s.huff[name].clone();
        // a prefix code with no gaps
        let kraft: u64 = e.iter().map(|&(_, l, _)| 1u64 << (32 - l)).sum();
        assert_eq!(kraft, 1u64 << 32, "{name} is not a complete prefix code");
        e.sort_by_key(|&(_, l, c)| c << (32 - l));
        got.extend(e.iter().map(|&(lv, l, _)| ((lv + off) as u8, l as u8)));
    }
    assert_eq!(HUFF_SRC.len(), got.len());
    assert_eq!(HUFF_SRC.to_vec(), got);
}
