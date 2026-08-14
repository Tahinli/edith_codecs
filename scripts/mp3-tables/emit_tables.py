"""Turn the measured Huffman tables into ec-mp3 source."""

import json

HDR = '''//! Layer III Huffman code tables.
//!
//! Every entry here was *measured*, not transcribed: `scripts/mp3-tables.py`
//! writes legal Layer III frames whose main-data bits it chooses, decodes them
//! with ffmpeg, and walks each code tree by asking whether all continuations of
//! a bit prefix decode to the same pair. The result is checked for a Kraft sum
//! of exactly 1, for full coverage of the value grid, and for uniqueness before
//! it is written out, so a wrong entry cannot reach this file silently.
//!
//! `(length, code)` indexed by `x * dim + y`, where `x` and `y` are the two
//! (or four, for the count1 tables) values the codeword yields, in the order
//! they are written to the spectrum.

'''


def emit_table(name, entries, dim, nvals):
    idx = {}
    for code, vals in entries.items():
        i = 0
        for v in vals:
            i = i * dim + v
        idx[i] = (len(code), int(code, 2) if code else 0)
    assert len(idx) == dim ** nvals, (name, len(idx))
    body = ", ".join(f"({idx[i][0]}, {idx[i][1]})" for i in range(dim ** nvals))
    return f"static {name}: [(u8, u16); {dim ** nvals}] = [{body}];\n"


def main():
    tabs = json.load(open("huffman.json"))
    out = [HDR]
    names = {}
    for key in ("1", "2", "3", "5", "6", "7", "8", "9", "10", "11", "12",
                "13", "15", "16", "24"):
        entries = tabs[key]
        dim = max(max(v) for v in entries.values()) + 1
        name = f"T{key}"
        names[key] = name
        out.append(emit_table(name, entries, dim, 2))
    for sel in (0, 1):
        entries = tabs[f"count1_{sel}"]
        out.append(emit_table(f"C{sel}", entries, 2, 4))
    with open("huffman_tables.rs", "w") as f:
        f.write("\n".join(out))
    print("wrote huffman_tables.rs")


if __name__ == "__main__":
    main()
