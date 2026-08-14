//! The three header packets: identification, comment, setup.
//!
//! Everything a Vorbis stream needs before its first audio packet lives here —
//! the blocksizes, the codebooks, and the floor/residue/mapping/mode
//! configurations that audio packets index by number. The setup header is
//! parsed once; audio decode after that is pure lookup.

use ec_core::{Error, Result};

use crate::bits::Bits;
use crate::codebook::{Codebook, ilog};

/// Identification header (§4.2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identification {
    /// Channel count, in Vorbis channel order.
    pub channels: u8,
    /// Sample rate in Hz.
    pub rate: u32,
    /// Nominal bitrate the encoder stated, 0 when it stated none.
    pub bitrate_nominal: i32,
    /// Short block size in samples.
    pub blocksize_0: usize,
    /// Long block size in samples.
    pub blocksize_1: usize,
}

impl Identification {
    /// Parse the identification packet, header byte included.
    pub fn parse(data: &[u8]) -> Result<Identification> {
        let mut bits = Bits::new(header_body(data, 1)?);
        let version = bits.read(32);
        if version != 0 {
            return Err(Error::unsupported(
                format!("Vorbis bitstream version {version}"),
                "only version 0 (Vorbis I) is defined",
            ));
        }
        let channels = bits.read(8) as u8;
        let rate = bits.read(32);
        let _bitrate_maximum = bits.read(32) as i32;
        let bitrate_nominal = bits.read(32) as i32;
        let _bitrate_minimum = bits.read(32) as i32;
        let blocksize_0 = 1usize << bits.read(4);
        let blocksize_1 = 1usize << bits.read(4);
        let framing = bits.bit();
        if bits.eop() || !framing {
            return Err(Error::corrupt("identification header truncated"));
        }
        if channels == 0 || rate == 0 {
            return Err(Error::corrupt("identification header states no audio"));
        }
        let legal = |n: usize| n.is_power_of_two() && (64..=8192).contains(&n);
        if !legal(blocksize_0) || !legal(blocksize_1) || blocksize_0 > blocksize_1 {
            return Err(Error::corrupt(format!(
                "blocksizes {blocksize_0}/{blocksize_1}"
            )));
        }
        Ok(Identification {
            channels,
            rate,
            bitrate_nominal,
            blocksize_0,
            blocksize_1,
        })
    }
}

/// Comment header (§5): the vendor string and the tag list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Comments {
    /// Encoder identification.
    pub vendor: String,
    /// `NAME=value` tags, in file order, with the name upper-cased.
    pub tags: Vec<(String, String)>,
}

impl Comments {
    /// Parse the comment packet, header byte included.
    ///
    /// A malformed tag is skipped rather than fatal: metadata never stops
    /// audio from playing.
    pub fn parse(data: &[u8]) -> Result<Comments> {
        let body = header_body(data, 3)?;
        let mut pos = 0usize;
        let take = |pos: &mut usize| -> Option<String> {
            let len = u32::from_le_bytes(body.get(*pos..*pos + 4)?.try_into().ok()?) as usize;
            *pos += 4;
            let text = body.get(*pos..pos.checked_add(len)?)?;
            *pos += len;
            Some(String::from_utf8_lossy(text).into_owned())
        };
        let vendor = take(&mut pos).ok_or_else(|| Error::corrupt("comment vendor string"))?;
        let count = u32::from_le_bytes(
            body.get(pos..pos + 4)
                .ok_or_else(|| Error::corrupt("comment count"))?
                .try_into()
                .map_err(|_| Error::corrupt("comment count"))?,
        );
        pos += 4;
        let mut tags = Vec::new();
        for _ in 0..count {
            let Some(entry) = take(&mut pos) else { break };
            if let Some((name, value)) = entry.split_once('=') {
                tags.push((name.to_ascii_uppercase(), value.to_string()));
            }
        }
        Ok(Comments { vendor, tags })
    }
}

/// Floor type 0: an LSP curve (§6.2).
#[derive(Debug, Clone)]
pub struct Floor0 {
    /// LSP order.
    pub order: usize,
    /// Rate the bark map is built against.
    pub rate: u32,
    /// Bark map resolution.
    pub bark_map_size: usize,
    /// Bits the amplitude is stated in.
    pub amplitude_bits: u32,
    /// dB offset applied to the amplitude.
    pub amplitude_offset: f32,
    /// Books the coefficient vector may be coded with.
    pub books: Vec<usize>,
}

/// Floor type 1: a piecewise-linear curve in dB (§7.2).
#[derive(Debug, Clone)]
pub struct Floor1 {
    /// Class of each partition, in order.
    pub partition_classes: Vec<usize>,
    /// Values per class.
    pub class_dimensions: Vec<usize>,
    /// Subclass bits per class.
    pub class_subclasses: Vec<u32>,
    /// Book naming the subclass, per class; unused when `class_subclasses` is 0.
    pub class_masterbooks: Vec<usize>,
    /// Book per subclass, `-1` for "this subclass codes nothing".
    pub subclass_books: Vec<Vec<i32>>,
    /// dB step per Y unit.
    pub multiplier: i32,
    /// X coordinate of every value, in coding order.
    pub x_list: Vec<u32>,
    /// Indices of `x_list` sorted by X, the order the curve is rendered in.
    pub sorted: Vec<usize>,
    /// Per value, the coding-order index of the nearest lower/higher X already
    /// coded, precomputed because the decode loop needs it per packet.
    pub neighbours: Vec<(usize, usize)>,
}

/// One floor configuration.
#[derive(Debug, Clone)]
pub enum FloorConfig {
    /// LSP floor.
    Zero(Floor0),
    /// Piecewise-linear floor.
    One(Floor1),
}

/// Residue configuration (§8).
#[derive(Debug, Clone)]
pub struct ResidueConfig {
    /// 0, 1 or 2 — how a partition's values are laid out.
    pub kind: u8,
    /// First coefficient coded.
    pub begin: usize,
    /// One past the last coefficient coded.
    pub end: usize,
    /// Coefficients per partition.
    pub partition_size: usize,
    /// Number of classes.
    pub classifications: usize,
    /// Book the class words are coded with.
    pub classbook: usize,
    /// Book per (class, pass), `-1` where the pass codes nothing.
    pub books: Vec<[i32; 8]>,
}

/// Mapping configuration (§4.2.4): which floor and residue serve which channel,
/// and which channel pairs are coupled.
#[derive(Debug, Clone)]
pub struct MappingConfig {
    /// `(magnitude, angle)` channel per coupling step.
    pub coupling: Vec<(usize, usize)>,
    /// Submap per channel.
    pub mux: Vec<usize>,
    /// `(floor, residue)` per submap.
    pub submaps: Vec<(usize, usize)>,
}

/// One mode (§4.2.4): a block size and a mapping.
#[derive(Debug, Clone, Copy)]
pub struct Mode {
    /// False for a short block, true for a long one.
    pub block_flag: bool,
    /// Mapping this mode decodes with.
    pub mapping: usize,
}

/// Everything the setup header configures.
#[derive(Debug, Clone)]
pub struct Setup {
    /// Codebooks, indexed by number.
    pub codebooks: Vec<Codebook>,
    /// Floor configurations.
    pub floors: Vec<FloorConfig>,
    /// Residue configurations.
    pub residues: Vec<ResidueConfig>,
    /// Mapping configurations.
    pub mappings: Vec<MappingConfig>,
    /// Modes; an audio packet's first field is an index into this.
    pub modes: Vec<Mode>,
}

impl Setup {
    /// Parse the setup packet, header byte included.
    pub fn parse(data: &[u8], ident: &Identification) -> Result<Setup> {
        let mut bits = Bits::new(header_body(data, 5)?);
        let channels = usize::from(ident.channels);

        let codebook_count = bits.read(8) as usize + 1;
        let mut codebooks = Vec::with_capacity(codebook_count);
        for _ in 0..codebook_count {
            codebooks.push(Codebook::parse(&mut bits)?);
        }

        // Time-domain transforms: a placeholder in Vorbis I, but the field is
        // there and must read zero.
        let time_count = bits.read(6) + 1;
        for _ in 0..time_count {
            if bits.read(16) != 0 {
                return Err(Error::corrupt("time domain transform is not zero"));
            }
        }

        let floor_count = bits.read(6) as usize + 1;
        let mut floors = Vec::with_capacity(floor_count);
        for _ in 0..floor_count {
            floors.push(parse_floor(&mut bits, &codebooks, ident)?);
        }

        let residue_count = bits.read(6) as usize + 1;
        let mut residues = Vec::with_capacity(residue_count);
        for _ in 0..residue_count {
            residues.push(parse_residue(&mut bits, codebooks.len())?);
        }

        let mapping_count = bits.read(6) as usize + 1;
        let mut mappings = Vec::with_capacity(mapping_count);
        for _ in 0..mapping_count {
            mappings.push(parse_mapping(
                &mut bits,
                channels,
                floors.len(),
                residues.len(),
            )?);
        }

        let mode_count = bits.read(6) as usize + 1;
        let mut modes = Vec::with_capacity(mode_count);
        for _ in 0..mode_count {
            let block_flag = bits.bit();
            let window_type = bits.read(16);
            let transform_type = bits.read(16);
            let mapping = bits.read(8) as usize;
            if window_type != 0 || transform_type != 0 {
                return Err(Error::corrupt("mode states an undefined transform"));
            }
            if mapping >= mappings.len() {
                return Err(Error::corrupt("mode names a mapping that is not there"));
            }
            modes.push(Mode {
                block_flag,
                mapping,
            });
        }

        if !bits.bit() || bits.eop() {
            return Err(Error::corrupt("setup header framing"));
        }

        Ok(Setup {
            codebooks,
            floors,
            residues,
            mappings,
            modes,
        })
    }
}

/// Strip and check the `type, "vorbis"` prefix every header carries.
fn header_body(data: &[u8], kind: u8) -> Result<&[u8]> {
    if data.len() < 7 || data[0] != kind || &data[1..7] != b"vorbis" {
        return Err(Error::corrupt(format!(
            "not a Vorbis header of type {kind}"
        )));
    }
    Ok(&data[7..])
}

fn parse_floor(
    bits: &mut Bits,
    codebooks: &[Codebook],
    ident: &Identification,
) -> Result<FloorConfig> {
    match bits.read(16) {
        0 => {
            let order = bits.read(8) as usize;
            let rate = bits.read(16);
            let bark_map_size = bits.read(16) as usize;
            let amplitude_bits = bits.read(6);
            let amplitude_offset = bits.read(8) as f32;
            let book_count = bits.read(4) as usize + 1;
            let mut books = Vec::with_capacity(book_count);
            for _ in 0..book_count {
                let book = bits.read(8) as usize;
                if book >= codebooks.len() {
                    return Err(Error::corrupt("floor 0 names a book that is not there"));
                }
                books.push(book);
            }
            if bits.eop() || order == 0 || bark_map_size == 0 || amplitude_bits > 32 {
                return Err(Error::corrupt("floor 0 configuration"));
            }
            Ok(FloorConfig::Zero(Floor0 {
                order,
                rate,
                bark_map_size,
                amplitude_bits,
                amplitude_offset,
                books,
            }))
        }
        1 => {
            let partitions = bits.read(5) as usize;
            let mut partition_classes = Vec::with_capacity(partitions);
            let mut max_class = 0usize;
            for _ in 0..partitions {
                let class = bits.read(4) as usize;
                max_class = max_class.max(class);
                partition_classes.push(class);
            }
            let class_count = if partitions == 0 { 0 } else { max_class + 1 };
            let mut class_dimensions = Vec::with_capacity(class_count);
            let mut class_subclasses = Vec::with_capacity(class_count);
            let mut class_masterbooks = Vec::with_capacity(class_count);
            let mut subclass_books = Vec::with_capacity(class_count);
            for _ in 0..class_count {
                let dimensions = bits.read(3) as usize + 1;
                let subclasses = bits.read(2);
                let masterbook = if subclasses > 0 {
                    let book = bits.read(8) as usize;
                    if book >= codebooks.len() {
                        return Err(Error::corrupt(
                            "floor 1 names a masterbook that is not there",
                        ));
                    }
                    book
                } else {
                    0
                };
                let mut books = Vec::with_capacity(1 << subclasses);
                for _ in 0..(1u32 << subclasses) {
                    let book = bits.read(8) as i32 - 1;
                    if book >= codebooks.len() as i32 {
                        return Err(Error::corrupt("floor 1 names a book that is not there"));
                    }
                    books.push(book);
                }
                class_dimensions.push(dimensions);
                class_subclasses.push(subclasses);
                class_masterbooks.push(masterbook);
                subclass_books.push(books);
            }
            let multiplier = bits.read(2) as i32 + 1;
            let range_bits = bits.read(4);
            let mut x_list = vec![0u32, 1u32 << range_bits];
            for &class in &partition_classes {
                for _ in 0..class_dimensions[class] {
                    x_list.push(bits.read(range_bits));
                }
            }
            if bits.eop() {
                return Err(Error::corrupt("floor 1 configuration truncated"));
            }
            if x_list.len() > 65 {
                return Err(Error::corrupt("floor 1 states more than 65 values"));
            }
            // The X list must be distinct or the curve has two heights at one
            // place; a stream that says so is corrupt, not a rendering choice.
            let mut seen = x_list.clone();
            seen.sort_unstable();
            if seen.windows(2).any(|w| w[0] == w[1]) {
                return Err(Error::corrupt("floor 1 repeats an X coordinate"));
            }
            if *seen.last().unwrap_or(&0) as usize > ident.blocksize_1 / 2 {
                return Err(Error::corrupt("floor 1 X coordinate past the block"));
            }
            let mut sorted: Vec<usize> = (0..x_list.len()).collect();
            sorted.sort_by_key(|&i| x_list[i]);
            let neighbours = (0..x_list.len())
                .map(|i| (low_neighbour(&x_list, i), high_neighbour(&x_list, i)))
                .collect();
            Ok(FloorConfig::One(Floor1 {
                partition_classes,
                class_dimensions,
                class_subclasses,
                class_masterbooks,
                subclass_books,
                multiplier,
                x_list,
                sorted,
                neighbours,
            }))
        }
        other => Err(Error::unsupported(
            format!("floor type {other}"),
            "Vorbis I defines floor 0 and floor 1",
        )),
    }
}

/// Coding-order index of the largest X below `x_list[i]` among values already
/// coded (§7.2.2 `low_neighbor`).
fn low_neighbour(x_list: &[u32], i: usize) -> usize {
    let mut best = 0usize;
    let mut best_x = None;
    for (j, &x) in x_list.iter().enumerate().take(i) {
        if x < x_list[i] && best_x.is_none_or(|b| x > b) {
            best = j;
            best_x = Some(x);
        }
    }
    best
}

/// The mirror of [`low_neighbour`].
fn high_neighbour(x_list: &[u32], i: usize) -> usize {
    let mut best = 0usize;
    let mut best_x = None;
    for (j, &x) in x_list.iter().enumerate().take(i) {
        if x > x_list[i] && best_x.is_none_or(|b| x < b) {
            best = j;
            best_x = Some(x);
        }
    }
    best
}

fn parse_residue(bits: &mut Bits, codebook_count: usize) -> Result<ResidueConfig> {
    let kind = bits.read(16);
    if kind > 2 {
        return Err(Error::unsupported(
            format!("residue type {kind}"),
            "Vorbis I defines 0, 1 and 2",
        ));
    }
    let begin = bits.read(24) as usize;
    let end = bits.read(24) as usize;
    let partition_size = bits.read(24) as usize + 1;
    let classifications = bits.read(6) as usize + 1;
    let classbook = bits.read(8) as usize;
    if bits.eop() || classbook >= codebook_count || end < begin {
        return Err(Error::corrupt("residue configuration"));
    }
    let mut cascade = Vec::with_capacity(classifications);
    for _ in 0..classifications {
        let low = bits.read(3);
        let high = if bits.bit() { bits.read(5) } else { 0 };
        cascade.push((high << 3) | low);
    }
    let mut books = Vec::with_capacity(classifications);
    for &cascade in &cascade {
        let mut row = [-1i32; 8];
        for (pass, slot) in row.iter_mut().enumerate() {
            if cascade & (1 << pass) != 0 {
                let book = bits.read(8) as usize;
                if book >= codebook_count {
                    return Err(Error::corrupt("residue names a book that is not there"));
                }
                *slot = book as i32;
            }
        }
        books.push(row);
    }
    if bits.eop() {
        return Err(Error::corrupt("residue configuration truncated"));
    }
    Ok(ResidueConfig {
        kind: kind as u8,
        begin,
        end,
        partition_size,
        classifications,
        classbook,
        books,
    })
}

fn parse_mapping(
    bits: &mut Bits,
    channels: usize,
    floor_count: usize,
    residue_count: usize,
) -> Result<MappingConfig> {
    if bits.read(16) != 0 {
        return Err(Error::unsupported(
            "mapping type other than 0",
            "Vorbis I defines mapping type 0 only",
        ));
    }
    let submap_count = if bits.bit() {
        bits.read(4) as usize + 1
    } else {
        1
    };
    let mut coupling = Vec::new();
    if bits.bit() {
        let steps = bits.read(8) as usize + 1;
        let field = ilog((channels - 1) as u32);
        for _ in 0..steps {
            let magnitude = bits.read(field) as usize;
            let angle = bits.read(field) as usize;
            if magnitude == angle || magnitude >= channels || angle >= channels {
                return Err(Error::corrupt("coupling step names an impossible pair"));
            }
            coupling.push((magnitude, angle));
        }
    }
    if bits.read(2) != 0 {
        return Err(Error::corrupt("mapping reserved bits are not zero"));
    }
    let mux = match submap_count > 1 {
        true => (0..channels)
            .map(|_| {
                let submap = bits.read(4) as usize;
                match submap < submap_count {
                    true => Ok(submap),
                    false => Err(Error::corrupt("channel names a submap that is not there")),
                }
            })
            .collect::<Result<Vec<_>>>()?,
        false => vec![0; channels],
    };
    let mut submaps = Vec::with_capacity(submap_count);
    for _ in 0..submap_count {
        let _unused = bits.read(8);
        let floor = bits.read(8) as usize;
        let residue = bits.read(8) as usize;
        if floor >= floor_count || residue >= residue_count {
            return Err(Error::corrupt(
                "submap names a floor or residue that is not there",
            ));
        }
        submaps.push((floor, residue));
    }
    if bits.eop() {
        return Err(Error::corrupt("mapping truncated"));
    }
    Ok(MappingConfig {
        coupling,
        mux,
        submaps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identification_refuses_what_it_cannot_decode() {
        assert!(Identification::parse(b"garbage").is_err());
        // Type 1 header, blocksizes 256/2048, 2 channels, 44100 Hz.
        let mut header = vec![1u8];
        header.extend_from_slice(b"vorbis");
        header.extend_from_slice(&0u32.to_le_bytes());
        header.push(2);
        header.extend_from_slice(&44_100u32.to_le_bytes());
        header.extend_from_slice(&0i32.to_le_bytes());
        header.extend_from_slice(&128_000i32.to_le_bytes());
        header.extend_from_slice(&0i32.to_le_bytes());
        header.push(0xb8); // blocksize_0 = 2^8, blocksize_1 = 2^11
        header.push(0x01); // framing
        let ident = Identification::parse(&header).expect("identification");
        assert_eq!(ident.channels, 2);
        assert_eq!(ident.rate, 44_100);
        assert_eq!(ident.blocksize_0, 256);
        assert_eq!(ident.blocksize_1, 2048);
        assert_eq!(ident.bitrate_nominal, 128_000);
    }
}
