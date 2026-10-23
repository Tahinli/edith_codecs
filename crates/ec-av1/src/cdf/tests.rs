use super::*;

/// Every row of a CDF table is non-decreasing and ends with the
/// alphabet's total (`32768`) followed by the adaptation counter (`0`),
/// which is the shape [`crate::msac`] expects to read.
fn assert_row_shape(row: &[u16]) {
    assert_eq!(row[row.len() - 2], 32768, "row does not total 32768");
    assert_eq!(row[row.len() - 1], 0, "row's adaptation counter is not 0");
    for w in row[..row.len() - 1].windows(2) {
        assert!(w[0] <= w[1], "row is not non-decreasing: {row:?}");
    }
}

/// Every `eob_pt` table libaom keeps as `[q][plane][2]` has its class-1
/// (`tx_class != TX_CLASS_2D`) row here, and it is the *other* row.
///
/// libaom only ever trained the class-1 row where a 1D tx type can occur:
/// `av1_get_ext_tx_set_type` returns `DCT_IDTX`/`DCTONLY` once
/// `tx_size_sqr_up >= TX_32X32`, so the 512- and 1024-point rows are the
/// untouched uniform initialiser. Pinned so a future table edit cannot
/// quietly narrow the domain back to 2D-only (lane-eobc1 r1; values
/// diffed against `token_cdfs.h` by `scripts/extract-eob-class1.py`).
#[test]
fn eob_pt_class1_rows_cover_every_size() {
    fn uniform(row: &[u16]) -> bool {
        let n = row.len() - 1;
        row[..n].iter().enumerate().all(|(i, &v)| {
            let n = n as u32;
            v == (((i as u32 + 1) * 32768 + n / 2) / n) as u16
        })
    }
    let trained: [(&[u16], &[u16]); 8] = [
        (&EOB_PT_16_LUMA, &EOB_PT_16_LUMA_CLASS1),
        (&EOB_PT_16_CHROMA, &EOB_PT_16_CHROMA_CLASS1),
        (&EOB_PT_64_LUMA, &EOB_PT_64_LUMA_CLASS1),
        (&EOB_PT_64_CHROMA, &EOB_PT_64_CHROMA_CLASS1),
        (&EOB_PT_128_CHROMA, &EOB_PT_128_CHROMA_CLASS1),
        (&EOB_PT_256_LUMA, &EOB_PT_256_LUMA_CLASS1),
        (&EOB_PT_256_CHROMA, &EOB_PT_256_CHROMA_CLASS1),
        (&EOB_PT_512_LUMA, &EOB_PT_512_LUMA_CLASS1),
    ];
    for (two_d, class1) in trained {
        assert_row_shape(class1);
        assert_eq!(two_d.len(), class1.len());
    }
    for (two_d, class1) in &trained[..7] {
        assert_ne!(two_d, class1, "class-1 row is a copy of the 2D row");
        assert!(!uniform(class1), "class-1 row was never trained");
    }
    for row in [
        &EOB_PT_512_LUMA_CLASS1[..],
        &EOB_PT_512_CHROMA_CLASS1[..],
        &EOB_PT_1024_LUMA_CLASS1[..],
        &EOB_PT_1024_CHROMA_CLASS1[..],
    ] {
        assert_row_shape(row);
        assert!(uniform(row), "libaom trained a 32-point class-1 row");
    }
}

#[test]
fn inter_frame_tables_are_well_formed_cdfs() {
    for row in INTRA_INTER {
        assert_row_shape(&row);
    }
    for ctx in SINGLE_REF {
        for row in ctx {
            assert_row_shape(&row);
        }
    }
    for row in Y_MODE {
        assert_row_shape(&row);
    }
    for row in NEW_MV {
        assert_row_shape(&row);
    }
    for row in ZERO_MV {
        assert_row_shape(&row);
    }
    for row in REF_MV {
        assert_row_shape(&row);
    }
    for row in DRL_MODE {
        assert_row_shape(&row);
    }
    assert_row_shape(&MV_JOINT);
    assert_row_shape(&MV_CLASS);
    assert_row_shape(&MV_CLASS0_BIT);
    for row in MV_CLASS0_FR {
        assert_row_shape(&row);
    }
    assert_row_shape(&MV_CLASS0_HP);
    for row in MV_BIT {
        assert_row_shape(&row);
    }
    assert_row_shape(&MV_FR);
    assert_row_shape(&MV_HP);
    assert_row_shape(&MV_SIGN);
}

/// Each table's row length matches the spec's symbol count for that
/// syntax element: two entries (total, counter) plus one fewer than the
/// alphabet.
#[test]
fn inter_frame_tables_have_the_spec_alphabet_size() {
    assert_eq!(INTRA_INTER[0].len(), 2 + 1); // binary
    assert_eq!(SINGLE_REF[0][0].len(), 2 + 1); // binary
    assert_eq!(Y_MODE[0].len(), 2 + 13 - 1); // 13 luma intra modes
    assert_eq!(NEW_MV[0].len(), 2 + 1);
    assert_eq!(ZERO_MV[0].len(), 2 + 1);
    assert_eq!(REF_MV[0].len(), 2 + 1);
    assert_eq!(DRL_MODE[0].len(), 2 + 1);
    assert_eq!(MV_JOINT.len(), 2 + 3); // MV_JOINTS = 4
    assert_eq!(MV_CLASS.len(), 2 + 10); // MV_CLASSES = 11
    assert_eq!(MV_CLASS0_BIT.len(), 2 + 1);
    assert_eq!(MV_CLASS0_FR[0].len(), 2 + 3); // 4-way fraction
    assert_eq!(MV_CLASS0_HP.len(), 2 + 1);
    assert_eq!(MV_BIT[0].len(), 2 + 1);
    assert_eq!(MV_FR.len(), 2 + 3);
    assert_eq!(MV_HP.len(), 2 + 1);
    assert_eq!(MV_SIGN.len(), 2 + 1);
}
