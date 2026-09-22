# Edith findings relevant to edith_codecs — 2026-09-19

From a four-bug fix batch in edith (user-reported import/audio/export defects).
Verdict from all three investigations: **no ec-* codec crate is defective.** The
bugs were edith-side: extension routing, audio-session state, VA-API plugin NAL
framing, container metadata. ec-h264 and ec-aac decoded every test input
correctly (sink output matched ffmpeg volume to 0.1 dB).

## Knowledge items (not bugs, no action required)

1. **ec-mp4 does not read the tkhd display matrix** (`crates/ec-mp4/src/demux.rs`,
   `b"tkhd"` arm in `read_trak`, ~line 326 reads only the trailing display size).
   edith worked around this by reading the matrix through the vendored `mp4`
   crate. If ec-mp4 should know rotation, the change is additive: parse the 3x3
   matrix in the tkhd arm and surface a `rotation` field on
   `ec_core::registry::StreamInfo` (~14 literal sites). Deferred — cross-repo
   public-API change, no consumer needs it today.

2. **edith builds symphonia's mp3 bundle without `mp1`/`mp2` features**, so
   MPEG audio layer I/II files are refused ("unsupported audio codec"). This is
   an edith Cargo-feature question, not an ec-* matter; noted here only because
   it is the one audio-codec gap found during the batch.

## Context (what was fixed in edith)

- `.mpeg`/`.mpg` (mp3 with embedded cover) now routes to the audio import path.
- Timeline audio no longer pins to the first imported source; a sounding file
  can join a session that started silent (add/remove/add sequence fixed).
- VA-API plugin (`crates/engine-hw`) no longer dies on H.264 samples with
  trailing `cabac_zero_word` zero bytes (Android MediaCodec output); export
  falls back to ec-h264 software decode if hw fails before the first frame.
- 90°/180°/270° display-matrix rotation is now honored in preview and export
  (baked into pixels); other angles refused by name.
