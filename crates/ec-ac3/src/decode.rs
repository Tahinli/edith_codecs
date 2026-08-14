//! Syncframe and audio-block decoding: everything between a parsed header and
//! a block of PCM (A/52 §6.1, §7.3-§7.9).
//!
//! One [`Core`] holds the state a syncframe stream carries from block to block
//! and from frame to frame — exponents, coupling coordinates, bit allocation
//! parameters, and the overlap-add tail. The audio block routine parses
//! `audblk()` in bit stream order, because that is the only order in which it
//! can be parsed: the number of bits a mantissa occupies is computed, not sent.

use ec_core::{BitReader, Error, Result};

use crate::bitalloc::{self, Allocation, BitAllocParams, Channel, DeltaBa};
use crate::bsi::{Acmod, Bsi};
use crate::exps::{self, Strategy};
use crate::mantissa::{Dither, Mantissas, scale};
use crate::transform::{BLOCK_SAMPLES, Imdct};

/// Full-bandwidth channels at most (3/2 mode).
pub(crate) const MAX_FBW: usize = 5;
/// Index of the LFE channel in the per-channel arrays.
pub(crate) const LFE: usize = 5;
/// Index of the coupling channel in the per-channel arrays.
pub(crate) const CPL: usize = 6;
/// Coded channels a frame can carry, plus the coupling channel.
pub(crate) const CHANNELS: usize = 7;
/// Coefficients per channel per block.
pub(crate) const COEFFS: usize = 256;

/// Which syntax the frame is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Syntax {
    /// AC-3 (`bsid <= 10`), six blocks per frame.
    Ac3,
    /// Enhanced AC-3 (Annex E, `bsid == 16`), one to six blocks per frame.
    Eac3,
}

/// The decoder state a stream carries across blocks and frames.
pub(crate) struct Core {
    pub(crate) syntax: Syntax,
    pub(crate) fscod: usize,
    pub(crate) acmod: Acmod,
    pub(crate) nfchans: usize,
    pub(crate) lfeon: bool,
    /// Decoded exponents per channel, valid up to that channel's `endmant`.
    pub(crate) exps: [[u8; COEFFS]; CHANNELS],
    /// Bit allocation pointers per channel.
    pub(crate) bap: [[u8; COEFFS]; CHANNELS],
    /// Exponent strategy of the current block per channel.
    pub(crate) expstr: [Strategy; CHANNELS],
    pub(crate) endmant: [usize; CHANNELS],
    pub(crate) strtmant: [usize; CHANNELS],
    // Coupling.
    pub(crate) cplinu: bool,
    pub(crate) chincpl: [bool; MAX_FBW],
    pub(crate) phsflginu: bool,
    pub(crate) cplbegf: usize,
    pub(crate) ncplsubnd: usize,
    pub(crate) ncplbnd: usize,
    /// Coupling band each sub-band belongs to, after `cplbndstrc` merging.
    pub(crate) cplband: [usize; 18],
    pub(crate) cplco: [[f32; 18]; MAX_FBW],
    pub(crate) phsflg: [bool; 18],
    pub(crate) cplleak: (i32, i32),
    /// Annex E sends a channel's coupling coordinates in the first block of
    /// each coupling run and only when they change after that.
    pub(crate) first_cplco: [bool; MAX_FBW],
    /// Likewise for the coupling leak initialisation.
    pub(crate) first_cplleak: bool,
    // Rematrixing.
    pub(crate) nrematbnd: usize,
    pub(crate) rematflg: [bool; 4],
    // Bit allocation.
    pub(crate) ba: BitAllocParams,
    pub(crate) csnroffst: u8,
    pub(crate) fsnroffst: [u8; CHANNELS],
    pub(crate) fgaincod: [u8; CHANNELS],
    pub(crate) deltba: [DeltaBa; CHANNELS],
    pub(crate) deltba_on: [bool; CHANNELS],
    // Per-block flags.
    pub(crate) blksw: [bool; MAX_FBW],
    pub(crate) dithflag: [bool; MAX_FBW],
    /// Dynamic range gain for program 1 and, in 1+1 mode, program 2.
    pub(crate) dynrng: [f32; 2],
    // Output.
    pub(crate) coeffs: [[f32; COEFFS]; CHANNELS],
    pub(crate) delay: [[f32; COEFFS]; MAX_FBW + 1],
    pub(crate) imdct: Imdct,
    pub(crate) dither: Dither,
    /// Fraction of the `dynrng` compression to apply (§7.7.1).
    pub(crate) drc_scale: f32,
    /// Whether zero-bit mantissas get dither (§7.3.4).
    pub(crate) dither_on: bool,
    // Spectral extension (Annex E §3.6). All unused under AC-3.
    pub(crate) spxinu: bool,
    pub(crate) chinspx: [bool; MAX_FBW],
    /// First and last-plus-one spectral extension sub-band.
    pub(crate) spx_begin: usize,
    pub(crate) spx_end: usize,
    /// Sub-band the copy region starts at.
    pub(crate) spxstrtf: usize,
    /// Raw `spxbegf`, which the coupling end frequency is derived from.
    pub(crate) spxbegf: usize,
    pub(crate) spxbndstrc: [bool; 18],
    /// Coupling banding structure, kept across blocks for Annex E's "reuse".
    pub(crate) cplbndstrc: [bool; 18],
    pub(crate) nspxbnds: usize,
    pub(crate) spxbndsz: [usize; 18],
    pub(crate) spxco: [[f32; 18]; MAX_FBW],
    /// Noise and signal blending factors per band.
    pub(crate) nblend: [[f32; 18]; MAX_FBW],
    pub(crate) sblend: [[f32; 18]; MAX_FBW],
    pub(crate) first_spxco: [bool; MAX_FBW],
    pub(crate) chinspxatten: [bool; MAX_FBW],
    pub(crate) spxattencod: [u8; MAX_FBW],
    /// The noise source of §3.6.4.2.4, kept apart from the dither source so
    /// neither one's sequence depends on the other's use.
    pub(crate) spx_noise: Dither,
    // Adaptive hybrid transform (Annex E §3.4). All false under AC-3.
    /// Which channels are AHT-coded this frame.
    pub(crate) aht: [bool; CHANNELS],
    /// Whether a channel's whole-frame AHT payload has been read yet.
    pub(crate) aht_read: [bool; CHANNELS],
    /// Six blocks of mantissas per channel: `[ch][blk * COEFFS + bin]`.
    pub(crate) aht_mant: Vec<f32>,
}

impl Core {
    /// A decoder with no history: silence in the overlap-add tail and no
    /// coupling, which is the state the standard asks for at a sync point.
    pub(crate) fn new() -> Core {
        Core {
            syntax: Syntax::Ac3,
            fscod: 0,
            acmod: Acmod::Stereo,
            nfchans: 2,
            lfeon: false,
            exps: [[0; COEFFS]; CHANNELS],
            bap: [[0; COEFFS]; CHANNELS],
            expstr: [Strategy::Reuse; CHANNELS],
            endmant: [0; CHANNELS],
            strtmant: [0; CHANNELS],
            cplinu: false,
            chincpl: [false; MAX_FBW],
            phsflginu: false,
            cplbegf: 0,
            ncplsubnd: 0,
            ncplbnd: 0,
            cplband: [0; 18],
            cplco: [[0.0; 18]; MAX_FBW],
            phsflg: [false; 18],
            cplleak: (0, 0),
            first_cplco: [true; MAX_FBW],
            first_cplleak: true,
            nrematbnd: 0,
            rematflg: [false; 4],
            ba: BitAllocParams::default(),
            csnroffst: 0,
            fsnroffst: [0; CHANNELS],
            fgaincod: [0; CHANNELS],
            deltba: [DeltaBa::default(); CHANNELS],
            deltba_on: [false; CHANNELS],
            blksw: [false; MAX_FBW],
            dithflag: [true; MAX_FBW],
            dynrng: [1.0; 2],
            coeffs: [[0.0; COEFFS]; CHANNELS],
            delay: [[0.0; COEFFS]; MAX_FBW + 1],
            imdct: Imdct::new(),
            dither: Dither::default(),
            drc_scale: 1.0,
            dither_on: true,
            spxinu: false,
            chinspx: [false; MAX_FBW],
            spx_begin: 0,
            spx_end: 0,
            spxstrtf: 0,
            spxbegf: 0,
            spxbndstrc: [false; 18],
            cplbndstrc: [false; 18],
            nspxbnds: 0,
            spxbndsz: [0; 18],
            spxco: [[0.0; 18]; MAX_FBW],
            nblend: [[0.0; 18]; MAX_FBW],
            sblend: [[0.0; 18]; MAX_FBW],
            first_spxco: [true; MAX_FBW],
            chinspxatten: [false; MAX_FBW],
            spxattencod: [0; MAX_FBW],
            spx_noise: Dither::default(),
            aht: [false; CHANNELS],
            aht_read: [false; CHANNELS],
            aht_mant: vec![0.0; CHANNELS * crate::aht::AHT_BLOCKS * COEFFS],
        }
    }

    /// Drop everything a seek invalidates.
    pub(crate) fn reset(&mut self) {
        for d in &mut self.delay {
            d.fill(0.0);
        }
        self.cplinu = false;
        self.dynrng = [1.0; 2];
        self.exps = [[0; COEFFS]; CHANNELS];
    }

    /// Adopt a frame header: channel configuration for the blocks that follow.
    pub(crate) fn start_frame(&mut self, syntax: Syntax, fscod: usize, bsi: &Bsi) {
        self.syntax = syntax;
        self.fscod = fscod;
        self.acmod = bsi.acmod;
        self.nfchans = bsi.nfchans;
        self.lfeon = bsi.lfeon;
        self.strtmant = [0; CHANNELS];
        self.endmant[LFE] = 7;
        self.aht_read = [false; CHANNELS];
        // §7.2.2.6: delta bit allocation starts each syncframe switched off.
        self.deltba_on = [false; CHANNELS];
        self.deltba = [DeltaBa::default(); CHANNELS];
    }

    /// One `dynrng` word as the gain this decoder will actually apply,
    /// scaled by [`Core::drc_scale`] (§7.7.1 "Partial Compression").
    pub(crate) fn gain(&self, dynrng: u8) -> f32 {
        let gain = dynrng_gain(dynrng);
        if self.drc_scale == 1.0 {
            gain
        } else {
            gain.powf(self.drc_scale)
        }
    }

    /// Everything after the bit stream has been parsed: decouple, rematrix,
    /// apply the dynamic range gain, transform, interleave.
    pub(crate) fn finish_block(&mut self, out: &mut Vec<f32>) {
        self.decouple();
        self.rematrix();
        self.apply_gain();
        self.transform(out);
    }

    /// Coded channels the frame hands out, LFE included.
    pub(crate) fn channels(&self) -> usize {
        self.nfchans + usize::from(self.lfeon)
    }

    /// Decode one `audblk()`, appending `BLOCK_SAMPLES` interleaved samples per
    /// channel to `out` in the order [`Core::channel_order`] states.
    pub(crate) fn block(
        &mut self,
        r: &mut BitReader<'_>,
        blk: usize,
        out: &mut Vec<f32>,
    ) -> Result<()> {
        self.parse_block(r, blk)?;
        self.finish_block(out);
        Ok(())
    }

    // ------------------------------------------------------------- parsing --

    fn parse_block(&mut self, r: &mut BitReader<'_>, blk: usize) -> Result<()> {
        let nfchans = self.nfchans;
        for ch in 0..nfchans {
            self.blksw[ch] = r.read_bit()?;
        }
        for ch in 0..nfchans {
            self.dithflag[ch] = r.read_bit()?;
        }
        if r.read_bit()? {
            self.dynrng[0] = self.gain(r.read_bits(8)? as u8);
        } else if blk == 0 {
            self.dynrng[0] = 1.0;
        }
        if self.acmod == Acmod::DualMono {
            if r.read_bit()? {
                self.dynrng[1] = self.gain(r.read_bits(8)? as u8);
            } else if blk == 0 {
                self.dynrng[1] = 1.0;
            }
        }

        // Coupling strategy.
        if r.read_bit()? {
            self.cplinu = r.read_bit()?;
            if self.cplinu {
                for ch in 0..nfchans {
                    self.chincpl[ch] = r.read_bit()?;
                }
                if self.acmod == Acmod::Stereo {
                    self.phsflginu = r.read_bit()?;
                }
                self.cplbegf = r.read_bits(4)? as usize;
                let cplendf = r.read_bits(4)? as usize;
                if cplendf + 3 <= self.cplbegf {
                    return Err(Error::corrupt(format!(
                        "AC-3 coupling: cplbegf {} above cplendf {cplendf}",
                        self.cplbegf
                    )));
                }
                self.ncplsubnd = 3 + cplendf - self.cplbegf;
                self.strtmant[CPL] = 37 + 12 * self.cplbegf;
                self.endmant[CPL] = 37 + 12 * (cplendf + 3);
                // Sub-bands merge into coupling bands as cplbndstrc says.
                self.cplband[0] = 0;
                self.ncplbnd = 1;
                for sbnd in 1..self.ncplsubnd {
                    if !r.read_bit()? {
                        self.ncplbnd += 1;
                    }
                    self.cplband[sbnd] = self.ncplbnd - 1;
                }
            }
        }

        // Coupling coordinates.
        let mut cplcoe = [false; MAX_FBW];
        if self.cplinu {
            for ch in 0..nfchans {
                if !self.chincpl[ch] {
                    continue;
                }
                cplcoe[ch] = r.read_bit()?;
                if !cplcoe[ch] {
                    continue;
                }
                let mstrcplco = r.read_bits(2)? as i32;
                for bnd in 0..self.ncplbnd {
                    let cplcoexp = r.read_bits(4)? as i32;
                    let cplcomant = r.read_bits(4)? as f32;
                    let mant = if cplcoexp == 15 {
                        cplcomant / 16.0
                    } else {
                        (cplcomant + 16.0) / 32.0
                    };
                    self.cplco[ch][bnd] = mant * scale((cplcoexp + 3 * mstrcplco).min(24) as u8);
                }
            }
            if self.acmod == Acmod::Stereo && self.phsflginu && (cplcoe[0] || cplcoe[1]) {
                for bnd in 0..self.ncplbnd {
                    self.phsflg[bnd] = r.read_bit()?;
                }
            }
        }

        // Rematrixing.
        if self.acmod == Acmod::Stereo && r.read_bit()? {
            self.nrematbnd = if !self.cplinu || self.cplbegf > 2 {
                4
            } else if self.cplbegf > 0 {
                3
            } else {
                2
            };
            for bnd in 0..self.nrematbnd {
                self.rematflg[bnd] = r.read_bit()?;
            }
        }

        // Exponent strategies and bandwidths.
        self.expstr = [Strategy::Reuse; CHANNELS];
        if self.cplinu {
            self.expstr[CPL] = Strategy::from_code(r.read_bits(2)?);
        }
        for ch in 0..nfchans {
            self.expstr[ch] = Strategy::from_code(r.read_bits(2)?);
        }
        if self.lfeon {
            self.expstr[LFE] = if r.read_bit()? {
                Strategy::D15
            } else {
                Strategy::Reuse
            };
        }
        for ch in 0..nfchans {
            if self.expstr[ch] != Strategy::Reuse {
                if self.chincpl[ch] && self.cplinu {
                    self.endmant[ch] = self.strtmant[CPL];
                } else {
                    let chbwcod = r.read_bits(6)? as usize;
                    if chbwcod > 60 {
                        return Err(Error::corrupt(format!(
                            "AC-3 audblk: chbwcod = {chbwcod} > 60"
                        )));
                    }
                    self.endmant[ch] = (chbwcod + 12) * 3 + 37;
                }
            }
        }

        // Exponents.
        if self.cplinu && self.expstr[CPL] != Strategy::Reuse {
            let absexp = (r.read_bits(4)? << 1) as u8;
            let (start, end) = (self.strtmant[CPL], self.endmant[CPL]);
            let ngrps = self.expstr[CPL].coupling_groups(start, end);
            exps::decode(
                r,
                self.expstr[CPL],
                ngrps,
                absexp,
                start,
                &mut self.exps[CPL],
            )?;
        }
        for ch in 0..nfchans {
            if self.expstr[ch] == Strategy::Reuse {
                continue;
            }
            let absexp = r.read_bits(4)? as u8;
            self.exps[ch][0] = absexp;
            let ngrps = self.expstr[ch].fbw_groups(self.endmant[ch]);
            exps::decode(r, self.expstr[ch], ngrps, absexp, 1, &mut self.exps[ch]).map_err(
                |e| match e {
                    Error::Corrupt { context } => Error::corrupt(format!(
                        "{context} (block {blk}, channel {ch}, {:?}, endmant {})",
                        self.expstr[ch], self.endmant[ch]
                    )),
                    other => other,
                },
            )?;
            let _gainrng = r.read_bits(2)?;
        }
        if self.lfeon && self.expstr[LFE] != Strategy::Reuse {
            let absexp = r.read_bits(4)? as u8;
            self.exps[LFE][0] = absexp;
            exps::decode(r, Strategy::D15, 2, absexp, 1, &mut self.exps[LFE])?;
        }

        // Bit allocation parameters.
        if r.read_bit()? {
            self.ba = BitAllocParams {
                sdcycod: r.read_bits(2)? as u8,
                fdcycod: r.read_bits(2)? as u8,
                sgaincod: r.read_bits(2)? as u8,
                dbpbcod: r.read_bits(2)? as u8,
                floorcod: r.read_bits(3)? as u8,
            };
        } else if blk == 0 {
            return Err(Error::corrupt(
                "AC-3 audblk 0: bit allocation parameters absent, nothing to reuse",
            ));
        }
        if r.read_bit()? {
            self.csnroffst = r.read_bits(6)? as u8;
            if self.cplinu {
                self.fsnroffst[CPL] = r.read_bits(4)? as u8;
                self.fgaincod[CPL] = r.read_bits(3)? as u8;
            }
            for ch in 0..nfchans {
                self.fsnroffst[ch] = r.read_bits(4)? as u8;
                self.fgaincod[ch] = r.read_bits(3)? as u8;
            }
            if self.lfeon {
                self.fsnroffst[LFE] = r.read_bits(4)? as u8;
                self.fgaincod[LFE] = r.read_bits(3)? as u8;
            }
        } else if blk == 0 {
            return Err(Error::corrupt(
                "AC-3 audblk 0: SNR offsets absent, nothing to reuse",
            ));
        }
        if self.cplinu && r.read_bit()? {
            // §7.2.2.1: fastleak = (cplfleak << 8) + 768, and the same for
            // the slow leak — the offset is not optional.
            self.cplleak = (
                ((r.read_bits(3)? as i32) << 8) + 768,
                ((r.read_bits(3)? as i32) << 8) + 768,
            );
        }

        // Delta bit allocation.
        if r.read_bit()? {
            let mut deltbae = [0u32; CHANNELS];
            if self.cplinu {
                deltbae[CPL] = r.read_bits(2)?;
            }
            for ch in 0..nfchans {
                deltbae[ch] = r.read_bits(2)?;
            }
            let mut order = Vec::with_capacity(CHANNELS);
            if self.cplinu {
                order.push(CPL);
            }
            order.extend(0..nfchans);
            for ch in order {
                match deltbae[ch] {
                    // Reuse: leave the previous segments in place.
                    0 => {}
                    1 => {
                        self.deltba_on[ch] = true;
                        let nseg = r.read_bits(3)? as usize + 1;
                        let mut dba = DeltaBa {
                            nseg,
                            ..DeltaBa::default()
                        };
                        for seg in 0..nseg {
                            dba.offset[seg] = r.read_bits(5)? as u8;
                            dba.length[seg] = r.read_bits(4)? as u8;
                            dba.delta[seg] = r.read_bits(3)? as u8;
                        }
                        self.deltba[ch] = dba;
                    }
                    // Off, and reserved treated as off.
                    _ => self.deltba_on[ch] = false,
                }
            }
        }

        // Dummy data.
        if r.read_bit()? {
            let skipl = r.read_bits(9)? as u64;
            r.skip_bits(skipl * 8)?;
        }

        self.allocate();
        self.read_mantissas(r, blk)
    }

    /// Run the bit allocation model for every channel of this block.
    pub(crate) fn allocate(&mut self) {
        // §7.2.2.1.1: an all-zero set of SNR offsets means no bits at all.
        let all_zero = (0..self.nfchans).all(|ch| self.fsnroffst[ch] == 0)
            && (!self.lfeon || self.fsnroffst[LFE] == 0)
            && (!self.cplinu || self.fsnroffst[CPL] == 0);
        if self.csnroffst == 0 && all_zero {
            for ch in 0..CHANNELS {
                self.bap[ch].fill(0);
            }
            return;
        }
        let mut channels: Vec<(usize, Channel)> = (0..self.nfchans)
            .filter(|&ch| self.endmant[ch] > 0)
            .map(|ch| (ch, Channel::Fbw))
            .collect();
        if self.cplinu {
            channels.push((CPL, Channel::Coupling { leak: self.cplleak }));
        }
        if self.lfeon {
            channels.push((LFE, Channel::Lfe));
        }
        for (ch, kind) in channels {
            let snroffset =
                (((i32::from(self.csnroffst) - 15) << 4) + i32::from(self.fsnroffst[ch])) << 2;
            let alloc = Allocation {
                fscod: self.fscod,
                params: self.ba,
                range: (self.strtmant[ch], self.endmant[ch]),
                fgaincod: self.fgaincod[ch],
                snroffset,
                kind,
                dba: self.deltba_on[ch].then_some(&self.deltba[ch]),
                high_efficiency: self.aht[ch],
            };
            bitalloc::compute(&alloc, &self.exps[ch], &mut self.bap[ch]);
        }
    }

    /// Unpack every mantissa of the block into [`Core::coeffs`].
    pub(crate) fn read_mantissas(&mut self, r: &mut BitReader<'_>, blk: usize) -> Result<()> {
        // §7.3.5: the 3-, 5- and 11-level groups are shared *across* exponent
        // sets — a partial group left by one channel is finished by the next.
        // One reader per block, not per channel, is the whole of that rule.
        let mut m = Mantissas::new();
        let mut got_cplchan = false;
        for ch in 0..self.nfchans {
            let end = self.endmant[ch];
            if self.aht[ch] {
                self.aht_channel(r, ch, (0, end), blk)?;
            } else {
                for bin in 0..end {
                    let bap = self.bap[ch][bin];
                    let value = m.read(r, bap)?;
                    self.coeffs[ch][bin] = if bap == 0 && self.dithflag[ch] && self.dither_on {
                        self.dither.value() * scale(self.exps[ch][bin])
                    } else {
                        value * scale(self.exps[ch][bin])
                    };
                }
            }
            self.coeffs[ch][end..].fill(0.0);
            if self.cplinu && self.chincpl[ch] && !got_cplchan {
                let (start, end) = (self.strtmant[CPL], self.endmant[CPL]);
                if self.aht[CPL] {
                    self.aht_channel(r, CPL, (start, end), blk)?;
                } else {
                    for bin in start..end {
                        let value = m.read(r, self.bap[CPL][bin])?;
                        self.coeffs[CPL][bin] = value * scale(self.exps[CPL][bin]);
                    }
                }
                got_cplchan = true;
            }
        }
        if self.lfeon {
            if self.aht[LFE] {
                self.aht_channel(r, LFE, (0, 7), blk)?;
            } else {
                for bin in 0..7 {
                    let value = m.read(r, self.bap[LFE][bin])?;
                    self.coeffs[LFE][bin] = value * scale(self.exps[LFE][bin]);
                }
            }
            self.coeffs[LFE][7..].fill(0.0);
        }
        Ok(())
    }

    /// One AHT channel: read the whole frame's mantissas on the first block
    /// that asks for them, then hand this block its share (§E3.4).
    fn aht_channel(
        &mut self,
        r: &mut BitReader<'_>,
        ch: usize,
        range: (usize, usize),
        blk: usize,
    ) -> Result<()> {
        let base = ch * crate::aht::AHT_BLOCKS * COEFFS;
        if !self.aht_read[ch] {
            let frame = &mut self.aht_mant[base..base + crate::aht::AHT_BLOCKS * COEFFS];
            crate::aht::read_channel(
                r,
                &self.bap[ch],
                range,
                &mut self.dither,
                self.dither_on,
                frame,
            )?;
            self.aht_read[ch] = true;
        }
        let offset = base + blk * COEFFS;
        for bin in range.0..range.1 {
            self.coeffs[ch][bin] = self.aht_mant[offset + bin] * scale(self.exps[ch][bin]);
        }
        Ok(())
    }

    // ------------------------------------------------------------ decoding --

    /// §7.4.3: rebuild each coupled channel's high bins from the coupling
    /// channel, scaled by that channel's coupling coordinates.
    pub(crate) fn decouple(&mut self) {
        if !self.cplinu {
            return;
        }
        let (start, end) = (self.strtmant[CPL], self.endmant[CPL]);
        for ch in 0..self.nfchans {
            if !self.chincpl[ch] {
                continue;
            }
            for sbnd in 0..self.ncplsubnd {
                let bnd = self.cplband[sbnd];
                let mut co = self.cplco[ch][bnd] * 8.0;
                // §5.4.3.18: in 2/0 with phase flags, the right channel's
                // contribution is inverted for the flagged bands.
                if ch == 1 && self.phsflginu && self.phsflg[bnd] {
                    co = -co;
                }
                let from = start + sbnd * 12;
                for bin in from..(from + 12).min(end) {
                    let cpl = if self.bap[CPL][bin] == 0 && self.dithflag[ch] && self.dither_on {
                        self.dither.value() * scale(self.exps[CPL][bin])
                    } else {
                        self.coeffs[CPL][bin]
                    };
                    self.coeffs[ch][bin] = cpl * co;
                }
            }
            self.coeffs[ch][end..].fill(0.0);
        }
    }

    /// §7.5.4: undo the sum/difference coding of the 2/0 mode.
    pub(crate) fn rematrix(&mut self) {
        if self.acmod != Acmod::Stereo || self.nrematbnd == 0 {
            return;
        }
        const BANDS: [usize; 4] = [13, 25, 37, 61];
        let limit = if self.cplinu {
            self.strtmant[CPL]
        } else {
            self.endmant[0].min(self.endmant[1])
        };
        for bnd in 0..self.nrematbnd {
            if !self.rematflg[bnd] {
                continue;
            }
            let start = BANDS[bnd];
            let end = if bnd + 1 < BANDS.len() {
                BANDS[bnd + 1].min(limit)
            } else {
                limit
            };
            for bin in start..end {
                let (l, r) = (self.coeffs[0][bin], self.coeffs[1][bin]);
                self.coeffs[0][bin] = l + r;
                self.coeffs[1][bin] = l - r;
            }
        }
    }

    /// §7.7.1: the dynamic range gain the encoder asked for.
    pub(crate) fn apply_gain(&mut self) {
        let mut channels: Vec<usize> = (0..self.nfchans).collect();
        if self.lfeon {
            channels.push(LFE);
        }
        for ch in channels {
            // In 1+1 mode the two coded channels are separate programs with
            // separate gain words; everywhere else one word covers the frame.
            let program = usize::from(self.acmod == Acmod::DualMono && ch == 1);
            let gain = self.dynrng[program];
            if gain != 1.0 {
                for v in &mut self.coeffs[ch] {
                    *v *= gain;
                }
            }
        }
    }

    /// Inverse transform every channel and interleave the result.
    pub(crate) fn transform(&mut self, out: &mut Vec<f32>) {
        let channels = self.channels();
        let base = out.len();
        out.resize(base + channels * BLOCK_SAMPLES, 0.0);
        let mut pcm = [0.0f32; BLOCK_SAMPLES];
        for (slot, ch) in self.channel_order().into_iter().enumerate() {
            let short = ch < MAX_FBW && self.blksw[ch];
            let store = if ch == LFE { MAX_FBW } else { ch };
            self.imdct
                .block(&self.coeffs[ch], short, &mut self.delay[store], &mut pcm);
            for (n, &v) in pcm.iter().enumerate() {
                out[base + n * channels + slot] = v;
            }
        }
    }

    /// The order this frame's channels are handed out in: the family's own
    /// L, R, C, LFE, Ls, Rs convention, as indices into the coded channels.
    pub(crate) fn channel_order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = match self.acmod {
            // 1+1 and 2/0 are already in order, as is any mode without a
            // centre channel; the coded order only differs where C sits.
            Acmod::Surround3_0 | Acmod::Surround3_1 | Acmod::Surround3_2 => {
                let mut v = vec![0, 2, 1];
                v.extend(3..self.nfchans);
                v
            }
            _ => (0..self.nfchans).collect(),
        };
        if self.lfeon {
            // The LFE follows the front channels, which is where every layout
            // in the family (and in WAVE) puts it.
            let fronts = match self.acmod {
                Acmod::Surround3_0 | Acmod::Surround3_1 | Acmod::Surround3_2 => 3,
                Acmod::Mono => 1,
                _ => 2,
            };
            order.insert(fronts.min(order.len()), LFE);
        }
        order
    }
}

/// §7.7.1.2: the 8-bit `dynrng` word as a linear gain.
///
/// The three most significant bits are a signed power of two from -4 to 3 with
/// an implicit +1, and the five least significant bits a fraction in `[1/2, 1)`.
/// The all-zero word is unity, which is what a block with no `dynrng` means.
fn dynrng_gain(dynrng: u8) -> f32 {
    let exponent = ((dynrng as i8) >> 5) as i32 + 1;
    let mantissa = f32::from(32 + (dynrng & 0x1f)) / 64.0;
    mantissa * (exponent as f32).exp2()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynrng_zero_is_unity_and_the_ends_are_plus_minus_24_db() {
        assert!((dynrng_gain(0x00) - 1.0).abs() < 1e-6);
        // X = 3 (+24.08 dB), Y = 11111 (63/64) => 2^4 * 63/64 = 15.75.
        assert!((dynrng_gain(0b011_11111) - 15.75).abs() < 1e-4);
        // X = -4 (-18.06 dB), Y = 00000 (1/2) => 2^-3 * 1/2 = 0.0625.
        assert!((dynrng_gain(0b100_00000) - 0.0625).abs() < 1e-6);
        // X = -1 is the table's "0 dB", but Y always contributes between
        // -0.14 and -6.02 dB: 2^0 * 1/2.
        assert!((dynrng_gain(0b111_00000) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn channel_order_puts_centre_second_and_lfe_after_the_fronts() {
        let mut core = Core::new();
        core.acmod = Acmod::Surround3_2;
        core.nfchans = 5;
        core.lfeon = true;
        assert_eq!(core.channel_order(), vec![0, 2, 1, LFE, 3, 4]);
        core.lfeon = false;
        assert_eq!(core.channel_order(), vec![0, 2, 1, 3, 4]);
        core.acmod = Acmod::Stereo;
        core.nfchans = 2;
        assert_eq!(core.channel_order(), vec![0, 1]);
        core.acmod = Acmod::Mono;
        core.nfchans = 1;
        core.lfeon = true;
        assert_eq!(core.channel_order(), vec![0, LFE]);
    }
}
