//! Regression for the threaded decode (lane-av1panichang): a worker that
//! panics must end the decode with an error, never hang it.
//!
//! Every worker site the threaded decoder waits on is armed in turn through
//! `ec_av1::fault`: the frame job itself (inside and past its own catch). Each
//! decode runs on a helper thread and must answer within 60 s; the answer must
//! be an error that names the panic. A second decode in the same process must
//! then succeed.

use ec_av1::fault;
use ec_av1::stream::decode_stream_with_threads;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

/// The fault plan is process-global, so the tests in this binary take turns.
static SERIAL: Mutex<()> = Mutex::new(());

const BUDGET: Duration = Duration::from_secs(60);

fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// Decodes `data` on a helper thread and returns the shown-picture count, or
/// the error text. Panics (failing the test) if no answer arrives in time.
fn decode_within_budget(data: Vec<u8>, threads: usize) -> Result<usize, String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut shown = 0usize;
        let r = decode_stream_with_threads(&data, threads, |_picture, _idx, _shown| {
            shown += 1;
            Ok(())
        });
        let _ = tx.send(r.map(|()| shown).map_err(|e| e.to_string()));
    });
    rx.recv_timeout(BUDGET)
        .unwrap_or_else(|_| panic!("the decode did not answer within {BUDGET:?}: it hung"))
}

#[test]
fn a_worker_panic_ends_the_decode_with_an_error_at_every_site() {
    let _guard = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stream = fixture("reelb_arf_witness.obu");
    // (site, which hit of that site to panic on)
    for (site, nth) in [
        ("frame", 5),
        ("frame-after", 5),
        ("recon", 3),
    ] {
        fault::arm(site, nth);
        let got = decode_within_budget(stream.clone(), 4);
        fault::disarm();
        let err = got.expect_err(&format!(
            "site `{site}` (hit {nth}): the decode should fail"
        ));
        assert!(
            err.contains("panicked"),
            "site `{site}`: the error should name the worker panic, got: {err}"
        );
    }
}

#[test]
fn a_decode_after_a_worker_panic_still_works() {
    let _guard = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stream = fixture("reelb_arf_witness.obu");
    let serial = decode_within_budget(stream.clone(), 1).expect("serial decode");

    fault::arm("frame", 3);
    let failed = decode_within_budget(stream.clone(), 4);
    fault::disarm();
    assert!(failed.is_err(), "the armed decode should fail");

    let again = decode_within_budget(stream, 4).expect("a decode after the panic must succeed");
    assert_eq!(
        again, serial,
        "the post-panic decode shows as many pictures"
    );
}
