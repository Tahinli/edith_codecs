# lane/legacyaudio

## Delivered
- ec-core CodecIds (contiguous after RawVideo): ImaAdpcmWav, MsAdpcm, ImaAdpcmQt, PcmAlaw, PcmMulaw, AmrNb, AmrWb.
- ec-pcm-ext: IMA ADPCM WAV, MS ADPCM, QuickTime IMA4, G.711 A-law/mu-law -> S16. 0-diff vs ffmpeg on 66 fixtures (mono/stereo; wav/avi/mov; 8/22.05/44.1 kHz) and 18 files through ec-probe Reader+AudioDecoder.
- ec-amr: AMR-NB (all 8 modes, SID/DTX, bad/lost frames) and AMR-WB (all 9 modes, SID, bad frames), fixed-point, plus raw .amr / .awb reader (`container.rs`).
- ec-riff: WAV/AVI tags 0x0002/0x0006/0x0007/0x0011 (`WavReader::compressed()`, `AviReader::audio_extradata()`). ec-mp4: `ima4`, `alaw`, `ulaw`, `samr`/`damr`, `sawb`. ec-probe: all registered, `.amr` sniff.
- Fuzz targets: ec-pcm-ext (decode_blocks), ec-amr (nb decode, container, wb). Built-in check only; not run under cargo-fuzz.

## Bit-exactness
- The 3GPP site and etsi returned 403, so the official TS 26.073/26.173 sources and TS 26.074/26.174 vectors were not obtained. Decoders were ported from opencore-amr 0.1.6 (derived from the 3GPP reference) and are bit-exact vs ffmpeg `libopencore_amrnb` / `libopencore_amrwb` (the fixed-point oracle) on all modes, DTX/SID, lost and corrupted frames, and random garbage. Not verified directly against the 3GPP vectors.
- SNR vs ffmpeg native float decoders: AMR-NB 18-41 dB (non-DTX; about -1 dB with comfort noise, since noise generation differs); AMR-WB 14-32 dB.
- opencore hard-codes quality=1; our decoders honor the Q bit (WB reference rebuilt locally to compare).

## Notes / not done
- Partial trailing ADPCM block in a WAV is dropped (ffmpeg decodes it).
- `#![allow(clippy::all)]` on the ec-amr `nb` module.
- ffmpeg refuses AMR in .mp4, so tests use .3gp/.3g2/.mov.
- Existing ec-riff test changed: unsupported-format example now GSM (0x0031).
- Gate: workspace clippy -D warnings clean; EC_REQUIRE_FIXTURES=1 tests pass for ec-core, ec-pcm-ext, ec-amr, ec-riff, ec-mp4, ec-probe (server).

## What edith needs
- `ec_probe::AudioDecoder::new` handles the 7 new ids; no other change. New CodecId variants may need arms in any exhaustive match (enum is non_exhaustive). Muxing/encoding of these codecs is not implemented.
