//! Helpers shared by the merged `it` test modules.

use std::path::{Path, PathBuf};

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(name)
}

fn mean(a: &[f32]) -> f64 {
    a.iter().map(|v| f64::from(*v)).sum::<f64>() / a.len().max(1) as f64
}

/// Zero-mean normalised correlation of two equal-length slices.
pub fn correlation(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    if n < 1024 {
        return 0.0;
    }
    let (a, b) = (&a[..n], &b[..n]);
    let (ma, mb) = (mean(a), mean(b));
    let (mut num, mut da, mut db) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b) {
        let x = f64::from(*x) - ma;
        let y = f64::from(*y) - mb;
        num += x * y;
        da += x * x;
        db += y * y;
    }
    let den = (da * db).sqrt();
    if den == 0.0 { 0.0 } else { num / den }
}

pub fn deinterleave(samples: &[f32], channels: usize) -> Vec<Vec<f32>> {
    let mut planes = vec![Vec::with_capacity(samples.len() / channels.max(1)); channels];
    for (i, &v) in samples.iter().enumerate() {
        planes[i % channels].push(v);
    }
    planes
}
