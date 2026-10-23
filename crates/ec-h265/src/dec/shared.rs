//! The part of a picture that other pictures read while it is still being decoded.
//!
//! With frame threads a later picture predicts from an earlier one that another
//! worker is still reconstructing. A [`SharedPic`] is the handoff between the two:
//!
//! - The samples live in an `RwLock<DecodedPicture>`. The worker that decodes the
//!   picture copies a CTB row into it only once that row is final (after the in-loop
//!   filters, see [`super::rowfilter`]). Rows are published top to bottom, so the
//!   published region is always a prefix of the picture.
//! - The collocated motion ([`ColField`]) is filled per CTB row as the rows are
//!   parsed, in the same way.
//! - A `Mutex` holds the progress counters and a `Condvar` wakes the readers when a
//!   counter moves. A reader waits for the rows it needs and then takes a read lock,
//!   so no lock is held across a wait. The writer never waits while it holds a lock.
//!
//! A failed picture records its error here, which wakes every waiter with an error.
//! Waiting never reads the samples of an unfinished row.

use std::sync::{Condvar, Mutex, MutexGuard, RwLock, RwLockReadGuard};

use super::motion_field::{ColField, MotionField};
use crate::decode::DecodedPicture;
use ec_core::error::{Error, Result};

/// Progress counters and the failure of a picture.
#[derive(Debug, Default)]
struct Progress {
    /// Luma rows whose samples are final (a prefix of the picture).
    rows: usize,
    /// Luma rows whose collocated motion is in [`SharedPic::col`].
    col_rows: usize,
    /// Every row is final, or the picture failed.
    done: bool,
    /// The first failure of the picture.
    error: Option<String>,
}

/// A picture whose samples and collocated motion are published row by row.
#[derive(Debug)]
pub struct SharedPic {
    /// Luma width.
    pub width: usize,
    /// Luma height.
    pub height: usize,
    planes: RwLock<DecodedPicture>,
    col: RwLock<ColField>,
    progress: Mutex<Progress>,
    changed: Condvar,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A poisoned lock only means another thread panicked while holding it; the
    // counters it guards are plain integers, so the value is still usable.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl SharedPic {
    /// The storage of a `width` by `height` picture of POC `poc`. With `pixels`
    /// false no sample planes are allocated (the motion-only decode).
    pub fn new(
        poc: i32,
        width: usize,
        height: usize,
        bit_depth: (u32, u32),
        pixels: bool,
        chroma_format_idc: u32,
    ) -> SharedPic {
        let planes = if pixels {
            let (sx, sy) = crate::decode::chroma_shifts(chroma_format_idc);
            let cw = width.div_ceil(1 << sx);
            let ch = height.div_ceil(1 << sy);
            DecodedPicture {
                width,
                height,
                bit_depth: bit_depth.0,
                bit_depth_c: bit_depth.1,
                chroma_format_idc,
                y: vec![0; width * height],
                cb: vec![0; cw * ch],
                cr: vec![0; cw * ch],
            }
        } else {
            DecodedPicture {
                width,
                height,
                bit_depth: 8,
                bit_depth_c: 8,
                chroma_format_idc,
                y: Vec::new(),
                cb: Vec::new(),
                cr: Vec::new(),
            }
        };
        SharedPic {
            width,
            height,
            planes: RwLock::new(planes),
            col: RwLock::new(ColField::new(poc, width, height)),
            progress: Mutex::new(Progress::default()),
            changed: Condvar::new(),
        }
    }

    /// A finished picture with the given samples and no motion: a generated picture (8.3.3).
    pub fn from_planes(poc: i32, planes: DecodedPicture) -> SharedPic {
        let (width, height) = (planes.width, planes.height);
        SharedPic {
            width,
            height,
            planes: RwLock::new(planes),
            col: RwLock::new(ColField::new(poc, width, height)),
            progress: Mutex::new(Progress {
                rows: height,
                col_rows: height,
                done: true,
                error: None,
            }),
            changed: Condvar::new(),
        }
    }

    /// Wait until at least `rows` luma rows are final. Returns the picture's failure
    /// instead when it has one.
    pub fn wait_rows(&self, rows: usize) -> Result<()> {
        let t = super::trace::now();
        let r = self.wait_for(rows, |p| p.rows);
        super::trace::since(super::trace::Phase::WaitRows, t);
        r
    }

    /// Wait until the collocated motion of at least `rows` luma rows is published.
    pub fn wait_col_rows(&self, rows: usize) -> Result<()> {
        self.wait_for(rows, |p| p.col_rows)
    }

    /// Wait for the whole picture (samples and motion) and return its samples.
    pub fn snapshot(&self) -> Result<std::sync::Arc<DecodedPicture>> {
        self.wait_rows(usize::MAX)?;
        let planes = self.planes.read().unwrap_or_else(|e| e.into_inner());
        let mut out = planes.clone();
        if out.chroma_format_idc == 0 {
            // The decoder keeps dummy chroma planes of a 4:0:0 picture; the output has none.
            out.cb = Vec::new();
            out.cr = Vec::new();
        }
        Ok(std::sync::Arc::new(out))
    }

    /// The published samples. Only rows below the last [`wait_rows`](Self::wait_rows)
    /// result are meaningful.
    pub fn read_planes(&self) -> RwLockReadGuard<'_, DecodedPicture> {
        self.planes.read().unwrap_or_else(|e| e.into_inner())
    }

    /// The published collocated motion.
    pub fn read_col(&self) -> RwLockReadGuard<'_, ColField> {
        self.col.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Copy the final luma rows `from..to` (and the chroma rows of the same area) of
    /// `src` into the published planes, then publish them. `src` has this picture's
    /// dimensions. A picture without planes only moves the counter.
    pub fn publish_rows(&self, src: &DecodedPicture, to: usize) {
        let from = lock(&self.progress).rows;
        if to <= from {
            return;
        }
        if !src.y.is_empty() {
            let w = self.width;
            let mut planes = self.planes.write().unwrap_or_else(|e| e.into_inner());
            let (sx, sy) = planes.chroma_shifts();
            let cw = w.div_ceil(1 << sx);
            let (cf, ct) = (from >> sy, to.div_ceil(1 << sy));
            planes.y[from * w..to * w].copy_from_slice(&src.y[from * w..to * w]);
            planes.cb[cf * cw..ct * cw].copy_from_slice(&src.cb[cf * cw..ct * cw]);
            planes.cr[cf * cw..ct * cw].copy_from_slice(&src.cr[cf * cw..ct * cw]);
        }
        self.advance(|p| p.rows = to);
    }

    /// Publish the collocated motion of the luma rows up to `to`, read from `field`
    /// (the motion field of this picture; rows already published are skipped).
    pub fn publish_col(&self, field: &MotionField, to: usize) {
        let from = lock(&self.progress).col_rows;
        if to <= from {
            return;
        }
        self.col
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .fill_rows(field, from, to);
        self.advance(|p| p.col_rows = to);
    }

    /// The picture is complete: every row is final and its motion is published.
    pub fn complete(&self) {
        self.advance(|p| {
            p.rows = self.height;
            p.col_rows = self.height;
            p.done = true;
        });
    }

    /// The picture failed. Waiters are released with `message`.
    pub fn fail(&self, message: &str) {
        self.advance(|p| {
            p.done = true;
            if p.error.is_none() {
                p.error = Some(message.to_string());
            }
        });
    }

    /// Wait until the picture is complete (or failed).
    pub fn wait_done(&self) -> Result<()> {
        self.wait_rows(usize::MAX)
    }

    fn advance(&self, f: impl FnOnce(&mut Progress)) {
        let mut p = lock(&self.progress);
        f(&mut p);
        drop(p);
        self.changed.notify_all();
    }

    fn wait_for(&self, want: usize, got: impl Fn(&Progress) -> usize) -> Result<()> {
        let mut p = lock(&self.progress);
        loop {
            if let Some(e) = &p.error {
                return Err(Error::corrupt(format!("HEVC decode: {e}")));
            }
            if p.done || got(&p) >= want {
                return Ok(());
            }
            p = self.changed.wait(p).unwrap_or_else(|e| e.into_inner());
        }
    }
}
