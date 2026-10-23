//! CEA-608 (line 21) caption decoder: CC1 and CC2 on field 1, CC3 and CC4 on field 2.
//!
//! Pop-on, roll-up (2 to 4 rows) and paint-on captioning, preamble address codes, mid-row codes,
//! the special and extended character sets, tab offsets, backspace, erase displayed / non-displayed
//! memory and end-of-caption swaps. Text mode (T1 to T4) is recognised and its data dropped.
//!
//! A cue is one *stable* picture of the display memory: a new cue starts whenever the displayed
//! text is committed (EOC, EDM, CR in roll-up, a mode switch, end of stream). Colour, italic and
//! underline become [`Segment`] wrappers; row and column ride in [`CaptionLine`] and in the cue's
//! [`CuePosition`] (percent of a 32 column by 15 row grid).

use ec_subs::{CuePosition, Segment, SubtitleCue, TextAlign};

/// One text line of a caption with its screen position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptionLine {
    /// Zero based row, 0..=14 for 608.
    pub row: u8,
    /// Zero based column of the first visible character, 0..=31 for 608.
    pub col: u8,
    /// The line's characters without trailing blanks.
    pub text: String,
}

/// A decoded caption cue.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptionCue {
    /// 608: 1 to 4 for CC1 to CC4. 708: the service number (1).
    pub channel: u8,
    /// The cue in the shared subtitle IR (text plus italic / underline / colour).
    pub cue: SubtitleCue,
    /// Row and column of every line.
    pub lines: Vec<CaptionLine>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cell {
    pub ch: char,
    pub color: u8,
    pub italic: bool,
    pub underline: bool,
}

pub(crate) const EMPTY: Cell = Cell {
    ch: '\0',
    color: 0,
    italic: false,
    underline: false,
};

const COLORS: [(u8, u8, u8); 8] = [
    (255, 255, 255),
    (0, 255, 0),
    (0, 0, 255),
    (0, 255, 255),
    (255, 0, 0),
    (255, 255, 0),
    (255, 0, 255),
    (0, 0, 0),
];

pub(crate) fn rgb(i: u8) -> (u8, u8, u8) {
    COLORS[(i as usize).min(7)]
}

pub(crate) fn visible(c: &Cell) -> bool {
    c.ch != '\0' && c.ch != ' '
}

/// Build a cue from rows of cells. `rows` pairs a row number with its cells.
pub(crate) fn rows_to_cue(
    channel: u8,
    rows: &[(u8, &[Cell])],
    cols: f32,
    row_count: f32,
    start_us: i64,
    end_us: i64,
) -> Option<CaptionCue> {
    let mut segments = Vec::new();
    let mut lines = Vec::new();
    for (row, cells) in rows {
        let Some(first) = cells.iter().position(visible) else {
            continue;
        };
        let last = cells.iter().rposition(visible)?;
        if !lines.is_empty() {
            segments.push(Segment::LineBreak);
        }
        let mut text = String::new();
        let mut k = first;
        while k <= last {
            let key = (cells[k].color, cells[k].italic, cells[k].underline);
            let mut run = String::new();
            while k <= last && (cells[k].color, cells[k].italic, cells[k].underline) == key {
                run.push(if cells[k].ch == '\0' {
                    ' '
                } else {
                    cells[k].ch
                });
                k += 1;
            }
            text.push_str(&run);
            let mut seg = vec![Segment::Text(run)];
            if key.2 {
                seg = vec![Segment::Underline(seg)];
            }
            if key.1 {
                seg = vec![Segment::Italic(seg)];
            }
            if key.0 != 0 {
                seg = vec![Segment::Color {
                    rgb: rgb(key.0),
                    children: seg,
                }];
            }
            segments.extend(seg);
        }
        lines.push(CaptionLine {
            row: *row,
            col: first as u8,
            text,
        });
    }
    let top = lines.first()?;
    let positioning = Some(CuePosition {
        x: Some(top.col as f32 / cols * 100.0),
        y: Some(top.row as f32 / row_count * 100.0),
        align: TextAlign::Left,
        size: None,
    });
    Some(CaptionCue {
        channel,
        cue: SubtitleCue {
            start_us,
            end_us,
            style_ref: None,
            positioning,
            segments,
        },
        lines,
    })
}

/// Push a cue, merging into the previous one when contiguous and identical.
pub(crate) fn push_merged(out: &mut Vec<CaptionCue>, c: CaptionCue) {
    if let Some(l) = out.last_mut()
        && l.cue.end_us == c.cue.start_us
        && l.lines == c.lines
        && l.cue.segments == c.cue.segments
    {
        l.cue.end_us = c.cue.end_us;
        return;
    }
    out.push(c);
}

type Grid = [[Cell; 32]; 15];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    None,
    PopOn,
    RollUp,
    PaintOn,
}

struct Chan {
    mode: Mode,
    text_mode: bool,
    disp: Grid,
    non: Grid,
    row: usize,
    col: usize,
    color: u8,
    italic: bool,
    underline: bool,
    roll_n: usize,
    show_since: i64,
    cues: Vec<CaptionCue>,
}

impl Chan {
    fn new() -> Chan {
        Chan {
            mode: Mode::None,
            text_mode: false,
            disp: [[EMPTY; 32]; 15],
            non: [[EMPTY; 32]; 15],
            row: 14,
            col: 0,
            color: 0,
            italic: false,
            underline: false,
            roll_n: 2,
            show_since: 0,
            cues: Vec::new(),
        }
    }

    fn disp_visible(&self) -> bool {
        self.disp.iter().any(|r| r.iter().any(visible))
    }

    fn commit(&mut self, ch: u8, t: i64) {
        if self.disp_visible() && t > self.show_since {
            let rows: Vec<(u8, &[Cell])> = self
                .disp
                .iter()
                .enumerate()
                .map(|(i, r)| (i as u8, &r[..]))
                .collect();
            if let Some(c) = rows_to_cue(ch, &rows, 32.0, 15.0, self.show_since, t) {
                push_merged(&mut self.cues, c);
            }
        }
        self.show_since = t;
    }

    fn put(&mut self, c: char, t: i64) {
        let cell = Cell {
            ch: c,
            color: self.color,
            italic: self.italic,
            underline: self.underline,
        };
        let col = self.col.min(31);
        match self.mode {
            Mode::None => {}
            Mode::PopOn => {
                self.non[self.row][col] = cell;
                self.col = (self.col + 1).min(32);
            }
            Mode::RollUp | Mode::PaintOn => {
                if !self.disp_visible() {
                    self.show_since = t;
                }
                self.disp[self.row][col] = cell;
                self.col = (self.col + 1).min(32);
            }
        }
    }

    fn target(&mut self) -> Option<&mut Grid> {
        match self.mode {
            Mode::None => None,
            Mode::PopOn => Some(&mut self.non),
            _ => Some(&mut self.disp),
        }
    }
}

fn basic(c: u8) -> char {
    match c {
        0x2A => 'á',
        0x5C => 'é',
        0x5E => 'í',
        0x5F => 'ó',
        0x60 => 'ú',
        0x7B => 'ç',
        0x7C => '÷',
        0x7D => 'Ñ',
        0x7E => 'ñ',
        0x7F => '\u{25A0}',
        c => c as char,
    }
}

const SPECIAL: [char; 16] = [
    '®', '°', '½', '¿', '™', '¢', '£', '♪', 'à', ' ', 'è', 'â', 'ê', 'î', 'ô', 'û',
];
const EXT_A: [char; 32] = [
    'Á', 'É', 'Ó', 'Ú', 'Ü', 'ü', '\u{2018}', '¡', '*', '\u{2019}', '\u{2014}', '©', '\u{2120}',
    '\u{2022}', '\u{201C}', '\u{201D}', 'À', 'Â', 'Ç', 'È', 'Ê', 'Ë', 'ë', 'Î', 'Ï', 'ï', 'Ô', 'Ù',
    'ù', 'Û', '«', '»',
];
const EXT_B: [char; 32] = [
    'Ã', 'ã', 'Í', 'Ì', 'ì', 'Ò', 'ò', 'Õ', 'õ', '{', '}', '\\', '^', '_', '|', '~', 'Ä', 'ä', 'Ö',
    'ö', 'ß', '¥', '¤', '\u{2502}', 'Å', 'å', 'Ø', 'ø', '\u{250C}', '\u{2510}', '\u{2514}',
    '\u{2518}',
];
const PAC_ROW: [usize; 8] = [11, 1, 3, 12, 14, 5, 7, 9];

/// CEA-608 decoder over (time, field, byte pair) input.
pub struct Cea608Decoder {
    ch: [Chan; 4],
    active: [usize; 2],
    last_ctrl: [Option<(u8, u8)>; 2],
}

impl Default for Cea608Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Cea608Decoder {
    /// A decoder in power-up state.
    pub fn new() -> Cea608Decoder {
        Cea608Decoder {
            ch: [Chan::new(), Chan::new(), Chan::new(), Chan::new()],
            active: [0, 0],
            last_ctrl: [None, None],
        }
    }

    /// Feed one byte pair (parity bits are masked, not checked). `field` 0 or 1.
    pub fn push_pair(&mut self, time_us: i64, field: u8, b1: u8, b2: u8) {
        let f = (field & 1) as usize;
        let (b1, b2) = (b1 & 0x7F, b2 & 0x7F);
        if b1 < 0x10 {
            self.last_ctrl[f] = None;
            return;
        }
        if b1 >= 0x20 {
            self.last_ctrl[f] = None;
            let c = f * 2 + self.active[f];
            let chan = &mut self.ch[c];
            if chan.text_mode {
                return;
            }
            chan.put(basic(b1), time_us);
            if b2 >= 0x20 {
                chan.put(basic(b2), time_us);
            }
            return;
        }
        let low = b1 & 7;
        let undoubled = (low == 1 && b2 >= 0x30) || ((low == 2 || low == 3) && b2 >= 0x20);
        if !undoubled {
            if self.last_ctrl[f] == Some((b1, b2)) {
                self.last_ctrl[f] = None;
                return;
            }
            self.last_ctrl[f] = Some((b1, b2));
        } else {
            self.last_ctrl[f] = None;
        }
        self.active[f] = ((b1 >> 3) & 1) as usize;
        let ci = f * 2 + self.active[f];
        let n = (ci + 1) as u8;
        let t = time_us;
        let c = &mut self.ch[ci];
        if b2 >= 0x40 {
            // Preamble address code.
            if c.text_mode {
                return;
            }
            let mut row = PAC_ROW[low as usize] + usize::from(b2 >= 0x60);
            if low == 0 && b2 >= 0x60 {
                return;
            }
            row -= 1;
            c.underline = b2 & 1 != 0;
            let v = (b2 >> 1) & 0xF;
            c.italic = v == 7;
            c.color = if v < 7 { v } else { 0 };
            c.col = if v >= 8 { ((v - 8) * 4) as usize } else { 0 };
            if c.mode == Mode::RollUp {
                row = row.max(c.roll_n - 1);
            }
            c.row = row.min(14);
            return;
        }
        match (low, b2) {
            (1, 0x20..=0x2F) if !c.text_mode => {
                let v = (b2 - 0x20) >> 1;
                c.underline = b2 & 1 != 0;
                c.italic = v == 7;
                c.color = if v < 7 { v } else { 0 };
                c.put(' ', t);
            }
            (1, 0x30..=0x3F) if !c.text_mode => c.put(SPECIAL[(b2 - 0x30) as usize], t),
            (2 | 3, 0x20..=0x3F) if !c.text_mode => {
                c.col = c.col.saturating_sub(1);
                let table = if low == 2 { &EXT_A } else { &EXT_B };
                c.put(table[(b2 - 0x20) as usize], t);
            }
            (7, 0x21..=0x23) if !c.text_mode => c.col = (c.col + (b2 - 0x20) as usize).min(31),
            (4 | 5, 0x20..=0x2F) => match b2 {
                0x20 => {
                    c.text_mode = false;
                    c.mode = Mode::PopOn;
                }
                0x21 if !c.text_mode => {
                    c.col = c.col.saturating_sub(1);
                    let col = c.col;
                    let row = c.row;
                    if let Some(g) = c.target() {
                        g[row][col] = EMPTY;
                    }
                }
                0x24 if !c.text_mode => {
                    let (row, col) = (c.row, c.col.min(31));
                    if let Some(g) = c.target() {
                        for x in &mut g[row][col..] {
                            *x = EMPTY;
                        }
                    }
                }
                0x25..=0x27 => {
                    c.text_mode = false;
                    c.roll_n = (b2 - 0x23) as usize;
                    if c.mode != Mode::RollUp {
                        c.commit(n, t);
                        c.disp = [[EMPTY; 32]; 15];
                        c.non = [[EMPTY; 32]; 15];
                        c.row = 14;
                        c.col = 0;
                        c.mode = Mode::RollUp;
                    } else {
                        c.row = c.row.max(c.roll_n - 1);
                    }
                }
                0x29 => {
                    c.text_mode = false;
                    c.mode = Mode::PaintOn;
                }
                0x2A | 0x2B => c.text_mode = true,
                0x2C => {
                    c.commit(n, t);
                    c.disp = [[EMPTY; 32]; 15];
                }
                0x2D if c.mode == Mode::RollUp && !c.text_mode => {
                    c.commit(n, t);
                    let base = c.row;
                    let top = (base + 1).saturating_sub(c.roll_n);
                    for r in top..base {
                        c.disp[r] = c.disp[r + 1];
                    }
                    c.disp[base] = [EMPTY; 32];
                    c.col = 0;
                }
                0x2E => c.non = [[EMPTY; 32]; 15],
                0x2F => {
                    c.text_mode = false;
                    c.commit(n, t);
                    std::mem::swap(&mut c.disp, &mut c.non);
                    c.mode = Mode::PopOn;
                    c.show_since = t;
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// Close the open display at `end_us` and return all cues of all channels by start time.
    pub fn finish(&mut self, end_us: i64) -> Vec<CaptionCue> {
        let mut out = Vec::new();
        for (i, c) in self.ch.iter_mut().enumerate() {
            c.commit((i + 1) as u8, end_us);
            out.append(&mut c.cues);
        }
        out.sort_by_key(|c| (c.cue.start_us, c.channel));
        out
    }
}
