# Lane pstail report

Branch lane/pstail (worktree /home/user/.cache/wt/pstail), commits prefixed `lane/pstail: `, no trailers, not pushed.

## Results
1. **Lost tail frame: fixed.** Cause: in an MPEG-1 `-f mpeg` file the last picture is an I-frame that opens a GOP in a PES without a PTS; with no earlier picture of the GOP the temporal-reference derivation had no GOP base, so pts stayed None and the stream end (max pts + frame) was one frame short. `psvideo.rs`: a new GOP's base now continues from the end of the display order so far (`disp_end`); every picture gets a pts. Repro: `-f mpeg` mpeg1video `-g 10 -bf 2`, 60 frames from `color+drawbox` (the edith FLASH clip): video stream duration was 59 frames, now 60*3600 ticks; packet count equals `ffprobe -count_frames`. TS path checked with a new test (mpeg1 and mpeg2 in mpegts, same source): no bug there (PES per picture), counts and durations are right; the TS code is untouched and all TS tests pass unchanged.
   - Needed for equality of seek and sequential reads: seeks into an MPEG-1/2 stream now start reading at the keyframe before the landing keyframe and drop everything that starts before the landing offset (`pre_drop`), so the derived times equal the sequential read's. A keyframe whose time is derived is a valid landing once the scan has seen the GOP before it.
2. **Byte-joined VOBs: fixed.** Epochs are still found from the SCR (`Clock::feed`), now also by a forward step of more than 1 s (backward more than 0.5 s as before; heuristic documented in the `ps.rs` module doc and DONE.md). After the walk, `align_epochs` shifts every epoch after the first so that its first video picture is shown where the previous epoch's last picture ends (max pts + one frame, measured by reading up to 4 MiB before and 1 MiB after each join with the real track code). Audio uses the same shift. Sequential reads take the aligned shift from the epoch table at the join pack. Result for two 2.4 s VOBs: duration 4.8 s exactly (432000 ticks), 120 frames, display times one frame apart over the join; seeks equal sequential on both sides (frames 3..119, Exact and SyncBefore, plus the generic `seek_exact`); audio offset to video after the join equals the one before within one MP2 frame (tested), audio continuous.
   - Limits: a PTS restart without an SCR restart is not seen; the alignment needs a video picture on both sides (else the SCR-continuous shift is kept); files over 96 MiB still find joins by sampling backward steps only.
3. **24-bit DVD LPCM exact seek: not done (not cheap).** Measured on `-f dvd` pcm_dvd 24-bit stereo: ffmpeg cuts packets at 2010 payload bytes (not a multiple of the 12-byte group, 6 bytes carry over), the first-access-unit pointer is always 4, and the PES stamps jitter by up to a packet. So neither the pointer nor the PTS gives sample alignment; the only exact way is the cumulative payload byte count from the start of the stream (a per-track index of every PES), which is linear at open and not worth it here. Edith keeps reading those tracks from the top.

## Tests (crates/ec-mpegts/tests/ps.rs, new, ffmpeg synthetic only)
`tail_picture_without_a_timestamp_is_counted`, `ts_tail_picture_is_counted`, `joined_vobs_have_the_summed_duration`, `seeks_on_both_sides_of_a_join_equal_the_sequential_decode`, `audio_keeps_its_offset_to_the_video_after_a_join`. All 25 old ps tests, TS tests and committed fixtures pass unchanged.

## Gates (server host c, final tree)
- `cargo clippy --workspace --all-targets --release -- -D warnings`: clean.
- `EC_REQUIRE_FIXTURES=1 cargo test -p ec-mpegts --release`: all pass (ps: 29).
- Fuzz (10 min each, host c, cargo +nightly fuzz, seeded corpus): demux_ts 24900 runs, demux_ps 24217 runs, ts_parts 3.08M runs, no crashes. (Run on the tree before the two clippy-only commits; the later commits change no library behaviour.)

## Edith: can "59 or 60" be tightened? Yes.
With this ec-mpegts the `-f mpeg` clip plays 60 of 60 frames. The joined-VOB test can also be exact (probe 4.8 s, 120 frames, seeks past the join land on the target frame). Exact diff for `crates/engine/tests/mpegps.rs` (NOT applied):

```diff
@@ fn sync_...  (the loop over `cases`)
-        // A last picture whose PES carries no time stamp is not counted in the stream's length
-        // (an ec-mpegts limit, ffmpeg's `-f mpeg` muxer writes that tail): one frame short at most.
-        assert!((59..=60).contains(&frames.len()), "{}: {} frames", spec.name, frames.len());
+        assert_eq!(frames.len(), 60, "{}: {} frames", spec.name, frames.len());
@@ fn two_vobs_joined_by_bytes_play_one_after_the_other
-    // Approximate across the join: the second clip's clock is rebuilt from the pack clock, and
-    // the muxer's 0.7 s lead of the picture over the pack clock leaves a gap of up to half a second.
-    assert!(secs > 4.7 && secs < 5.5, "{secs} s");
+    // The timeline is continuous over the join: the length is the sum of the two clips.
+    assert!((secs - 4.8).abs() < 0.03, "{secs} s");
     let all = session_frames(&joined);
-    assert!(all.len() >= 118 && all.len() <= 132, "{} frames", all.len());
+    assert_eq!(facts.frame_count, 120);
     ...
-    // A seek before the join is the sequential frame. After it the timeline is the pack clock's
-    // (a gap the length of the muxer's lead), so a seek lands within half a second of where it
-    // was aimed, and what it shows is a frame of the sequential play.
-    for target in [5u32, 33, 59] {
+    // A seek on either side of the join is the sequential frame.
+    for target in [5u32, 33, 59, 60, 66, 70, 80, 119] {
     ...
-    for target in [66u32, 70, 80] { ... the flash_frame_in / `<= 14` block ... }
```
(The `for target in [5,33,59]` loop body already asserts `seeked.index == target` and equal pixels; extend its list and delete the second loop.) `facts.frame_count` is the field of `engine::probe_media`'s result used elsewhere in that file (`meta.frame_count`); use whichever the joined probe exposes.
The 20/24-bit LPCM note in edith (`TsAudio::seek` reads from the top) stays.
