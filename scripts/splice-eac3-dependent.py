#!/usr/bin/env python3
"""Splice E-AC-3 dependent substreams onto an independent stream (test-fixture tool).

usage: splice-eac3-dependent.py <independent.eac3> <dependent.eac3> <chanmap-hex> <out.eac3> <dep.convsnr.txt>

ffmpeg's E-AC-3 encoder stops at 5.1, so a 7.1 fixture cannot be encoded directly. This
builds one from two ordinary encodes: each output packet is one independent syncframe
followed by one dependent syncframe. The dependent frame is the second stream's frame with
strmtyp set to 1 (dependent) and the 16-bit custom channel map (Table E2.5) inserted where
bsi() reads it, the one field a dependent header carries and an independent one does not.
The map is written bit 15 first, so 0x8000 is L and 0x0001 is LFE.

The dependent source is an independent frame, and an independent frame carries fields a
dependent one does not: convexpstr in audfrm() and convsnroffste in each audblk(). Their
bit positions come from the table in <dep.convsnr.txt> (one "position flag" row per block,
six per frame, recorded by the crate's own parser from the dependent source; see
gen-ac3dep-fixtures.sh). Both streams must have the same frame count.

Asserts the other preconditions: the source frames are independent, six blocks per frame,
no AHT, and no mixing metadata (mixmdate = 0).
"""
import sys


class Bits:
    def __init__(self, data):
        self.data, self.pos = data, 0

    def read(self, n):
        v = 0
        for _ in range(n):
            byte = self.data[self.pos >> 3]
            v = (v << 1) | ((byte >> (7 - (self.pos & 7))) & 1)
            self.pos += 1
        return v


def bits_of(data):
    return [(byte >> i) & 1 for byte in data for i in range(7, -1, -1)]


def to_bytes(bitlist):
    """Pack bits to bytes, zero-padding to a whole 16-bit word (frames are even-sized)."""
    bitlist = list(bitlist)
    while len(bitlist) % 16:
        bitlist.append(0)
    out = bytearray()
    for i in range(0, len(bitlist), 8):
        v = 0
        for b in bitlist[i:i + 8]:
            v = (v << 1) | b
        out.append(v)
    return bytes(out)


def frames(data):
    pos = 0
    while pos < len(data):
        assert data[pos:pos + 2] == b"\x0b\x77", "not at a sync word"
        size = (((data[pos + 2] & 7) << 8) | data[pos + 3]) + 1
        size *= 2
        yield data[pos:pos + size]
        pos += size


def audfrm_convexpstr_span(bits, pos, acmod, lfeon, nfchans):
    """Bit span [start, end) of the convexpstr fields in an independent 6-block audfrm().

    Annex E sends convexpstr (5 bits per full-bandwidth channel) in independent substreams
    only. Walks audfrm() the way eac3.rs does, for the case the splice supports: six blocks,
    no AHT, expstre set.
    """
    p = pos
    expstre, ahte = bits[p], bits[p + 1]
    p += 2
    assert ahte == 0, "splice expects ahte=0"
    p += 2 + 8  # snroffststr, then transproce..spxattene (eight flags)
    cplinu = [0] * 6
    if acmod >= 2:
        cplstre = [0] * 6
        cplinu[0] = bits[p]
        p += 1
        for blk in range(1, 6):
            cplstre[blk] = bits[p]
            p += 1
            if cplstre[blk]:
                cplinu[blk] = bits[p]
                p += 1
            else:
                cplinu[blk] = cplinu[blk - 1]
    if expstre:
        for blk in range(6):
            if cplinu[blk]:
                p += 2  # cplexpstr
            p += 2 * nfchans  # chexpstr per channel
    else:
        if acmod >= 2 and any(cplinu):
            p += 5  # frmcplexpstr
        p += 5 * nfchans  # frmchexpstr per channel
    if lfeon:
        p += 6  # lfeexpstr per block
    start = p
    p += 5 * nfchans  # convexpstr, present because numblkscod == 3
    return start, p


# Full-bandwidth channels per acmod (Table 5.8).
FBW_CHANNELS = {0: 2, 1: 1, 2: 2, 3: 3, 4: 3, 5: 4, 6: 4, 7: 5}


def dependent_frame(frame, chanmap, convsnr):
    bits = bits_of(frame)
    r = Bits(frame)
    r.pos = 16
    assert r.read(2) == 0, "source frame must be independent"
    r.read(3)  # substreamid
    r.read(11)  # frmsiz, rewritten below
    fscod = r.read(2)
    if fscod == 3:
        r.read(2)  # fscod2
        numblkscod = 3
    else:
        numblkscod = r.read(2)
    assert numblkscod == 3, "splice expects six blocks per frame"
    acmod = r.read(3)
    lfeon = r.read(1)
    r.read(5)  # bsid
    r.read(5)  # dialnorm
    if r.read(1):
        r.read(8)  # compr
    if acmod == 0:
        r.read(5)  # dialnorm2
        if r.read(1):
            r.read(8)  # compr2
    insert_at = r.pos
    nfchans = FBW_CHANNELS[acmod]
    # Walk bsi() past mixmdate (asserted zero), infomdate and addbsi to audfrm().
    p = insert_at
    assert bits[p] == 0, "mixing metadata (mixmdate) is not spliceable"
    p += 1
    if bits[p]:  # infomdate: flag, bsmod(3), copyrightb, origbs, then mode-dependent fields
        p += 1 + 3 + 1 + 1
        if acmod == 2:
            p += 4  # dsurmod, dheadphonmod
        if acmod >= 6:
            p += 2  # dsurexmod
        if bits[p]:  # mixlevel
            p += 1 + 5 + 2 + 1
        else:
            p += 1
        if acmod == 0:
            if bits[p]:
                p += 1 + 5 + 2 + 1
            else:
                p += 1
        if fscod < 3:
            p += 1  # sourcefscod
    else:
        p += 1
    if bits[p]:  # addbsi
        p += 1 + 6
        addbsil = int("".join(map(str, bits[p - 6:p])), 2)
        p += (addbsil + 1) * 8
    else:
        p += 1
    audfrm = p
    cstart, cend = audfrm_convexpstr_span(bits, audfrm, acmod, lfeon, nfchans)

    # Independent-only fields to drop: convexpstr in audfrm(), and convsnroffste (a flag, plus
    # ten bits of convsnroffst when set) at the start of each audblk()'s bit allocation.
    spans = [(cstart, cend)]
    for pos, flag in convsnr:
        spans.append((pos, pos + (11 if flag else 1)))
    spans.sort()
    new = bits[:16] + [0, 1] + bits[18:insert_at]  # strmtyp = 1, substreamid kept
    new += [1] + [(chanmap >> (15 - i)) & 1 for i in range(16)]  # chanmap present
    at = insert_at
    for start, end in spans:
        new += bits[at:start]
        at = end
    new += bits[at:]
    frame_bytes = len(to_bytes(new))
    frmsiz = frame_bytes // 2 - 1
    assert frmsiz < 2048, "frame too large for frmsiz"
    new[21:32] = [(frmsiz >> (10 - i)) & 1 for i in range(11)]
    return to_bytes(new)


def read_convsnr(path, frames_count):
    """Per-frame lists of (bit position, flag) for convsnroffste, six blocks per frame."""
    with open(path) as f:
        rows = [tuple(int(x) for x in line.split()) for line in f if line.strip()]
    assert len(rows) == 6 * frames_count, "convsnr table does not match the frame count"
    return [rows[6 * i:6 * i + 6] for i in range(frames_count)]


def main():
    ind_path, dep_path, chanmap_hex, out_path, convsnr_path = sys.argv[1:6]
    chanmap = int(chanmap_hex, 16)
    with open(ind_path, "rb") as f:
        ind = list(frames(f.read()))
    with open(dep_path, "rb") as f:
        dep = list(frames(f.read()))
    assert len(ind) == len(dep), f"frame counts differ: {len(ind)} vs {len(dep)}"
    table = read_convsnr(convsnr_path, len(dep))
    with open(out_path, "wb") as out:
        for a, b, convsnr in zip(ind, dep, table):
            out.write(a)
            out.write(dependent_frame(b, chanmap, convsnr))
    print(f"wrote {len(ind)} packets to {out_path}")


if __name__ == "__main__":
    main()
