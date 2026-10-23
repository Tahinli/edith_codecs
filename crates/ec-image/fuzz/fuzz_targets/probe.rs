#![no_main]

use libfuzzer_sys::fuzz_target;

// The bounded header probes, over the whole input and over a reader. Neither may panic, and
// neither may decode pixels, so no Limits are passed: a probe that allocated per declared
// size would show up as an out-of-memory here rather than a wrong answer.
fuzz_target!(|data: &[u8]| {
    let _ = ec_image::probe(data);
    let _ = ec_image::info(data);
    let _ = ec_image::probe_reader(data);
});
