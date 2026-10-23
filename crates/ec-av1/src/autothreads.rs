//! lane-av1threads: the decoder's default frame-worker count.
//!
//! `EC_AV1_THREADS` used to default to 1, so an edith decode ran on one core.
//! It now defaults to `min(available_parallelism, `[`DEFAULT_CAP`]`)`, and
//! `EC_AV1_THREADS=1` still forces the single-threaded decoder. Frame workers
//! are byte-exact by the stream gates; they do not on their own reach 1080p30
//! realtime on the bench stream (see `lanes/av1threads.report.md`).
//!
//! The in-frame filter bands follow the thread count: at `EC_AV1_THREADS > 1`
//! they default to [`split`]'s filter share, and `EC_AV1_FILTER_THREADS`
//! overrides it. `EC_AV1_THREADS=1` is fully serial. Reconstruction runs on
//! the superblock-row wavefront at `EC_AV1_RECON_THREADS` workers, which
//! defaults to the recon half of [`split`].

/// Upper bound on the default frame-worker count.
pub(crate) const DEFAULT_CAP: usize = 6;

/// The default decode thread count: the core count, capped at
/// [`DEFAULT_CAP`]. Never less than 1.
pub(crate) fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .clamp(1, DEFAULT_CAP)
}

/// The in-frame `(recon, filter)` thread split for `threads` decode threads.
/// The recon half is the wavefront's worker count (2 to 4); the filter half
/// is serial at 1, otherwise half the threads (1 to 3).
pub(crate) fn split(threads: usize) -> (usize, usize) {
    if threads <= 1 {
        return (1, 1);
    }
    ((threads * 3 / 4).clamp(2, 4), (threads / 2).clamp(1, 3))
}

/// The decode thread count for an `EC_AV1_THREADS` value: the parsed value
/// when it is a positive integer, otherwise [`default_threads`].
///
/// Under `cfg(test)` the unset default is 1: the gates read thread-local
/// counters that frame workers never increment, so a threaded default would
/// blind about 150 of them. The threaded path is pinned explicitly by
/// [`tests::frame_workers_decode_the_same_pictures_as_one_thread`].
pub(crate) fn decode_threads_from(value: Option<&str>) -> usize {
    value
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or_else(|| if cfg!(test) { 1 } else { default_threads() })
}

/// Decodes `data` with `threads` frame workers and returns every shown
/// picture's planes, in output order.
#[cfg(test)]
fn shown_planes(data: &[u8], threads: usize) -> Vec<(usize, Vec<u16>, Vec<u16>, Vec<u16>)> {
    let mut out = Vec::new();
    crate::stream::decode_stream_with_threads(data, threads, |picture, _, shown| {
        if shown {
            out.push((
                picture.width,
                picture.y.clone(),
                picture.u.clone(),
                picture.v.clone(),
            ));
        }
        Ok(())
    })
    .expect("fixture decodes");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_env_value_wins_and_zero_or_junk_falls_back() {
        assert_eq!(decode_threads_from(Some("1")), 1);
        assert_eq!(decode_threads_from(Some(" 3 ")), 3);
        assert_eq!(decode_threads_from(Some("0")), 1);
        assert_eq!(decode_threads_from(Some("many")), 1);
        assert_eq!(decode_threads_from(None), 1);
    }

    #[test]
    fn frame_workers_decode_the_same_pictures_as_one_thread() {
        let fixtures: [&[u8]; 2] = [
            include_bytes!("../fixtures/420_mixll_altref_640x480_5f.obu"),
            include_bytes!("../fixtures/420_mixll_320x240_5f.obu"),
        ];
        for data in fixtures {
            let serial = shown_planes(data, 1);
            assert!(!serial.is_empty(), "fixture shows no frames");
            assert_eq!(serial, shown_planes(data, 4));
        }
    }

    #[test]
    fn split_is_serial_at_one_thread_and_bounded_above() {
        assert_eq!(split(1), (1, 1));
        assert_eq!(split(2), (2, 1));
        assert_eq!(split(4), (3, 2));
        assert_eq!(split(6), (4, 3));
        assert_eq!(split(64), (4, 3));
    }

    #[test]
    fn default_is_capped_and_positive() {
        let n = default_threads();
        assert!((1..=DEFAULT_CAP).contains(&n));
    }
}
