#!/usr/bin/env python3
"""One-shot probe: does the real decoder read a `bs_pointer` field in
FIXVAR sbr_grid(), and is bs_freq_res transmitted in reverse time order for
that class? Reuses the already-oracle-proven `sbr_bits_full`/`oracle_check`
machinery from sbrpayload_fixtures.py, swapping in a grid writer with a
non-uniform freq_res list and an EXPLICIT (not FILL) codeword for the one
Huffman slot the high-res envelope needs -- FILL's length is book-dependent
and not what we wrote, so byte prediction is only exact with an explicit
word (learned the hard way: FILL made every naive attempt here mismatch by
several bytes even for the untouched baseline). Not part of the pytest/cargo
run -- throwaway measurement, output pasted into the ledger.
"""
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import sbrtables as T
from sbrpayload_fixtures import one_band_header, sbr_bits_full, oracle_check

cfg0, tables0 = one_band_header()
assert tables0["n_low"] == 1 and tables0["n_high"] == 2 and tables0["n_q"] == 1, tables0

ENV15_F_DELTA0 = T.as_bits(2, 0)  # (length=2, code=0b00, delta=0)


def ptr_bits(num_env):
    n = 1
    while (1 << n) < num_env + 1:
        n += 1
    return n


def make_grid_writer(time_fr, pointer, reverse):
    num_env = len(time_fr)
    num_rel = num_env - 1

    def write(s, grid_cfg):
        s.w(1, 2)  # frame_class = FIXVAR
        s.w(1, 2)  # var_bord
        s.w(num_rel, 2)
        for _ in range(num_rel):
            s.w(1, 2)  # rel_bord = 2*1+2 = 4
        if pointer:
            s.w(0, ptr_bits(num_env))
        wire = list(reversed(time_fr)) if reverse else list(time_fr)
        for fr in wire:
            s.w(fr, 1)
        return num_env, time_fr

    return write


def probe(oracle, label, time_fr, pointer, reverse):
    import sbrpayload_fixtures as F
    F.write_grid = make_grid_writer(time_fr, pointer, reverse)
    cfg = dict(cfg0, df_env=[0] * 8, df_noise=[0, 0])
    ok, n = oracle_check(oracle, cfg, tables0, {}, [ENV15_F_DELTA0], label)
    return ok, n


if __name__ == "__main__":
    oracle = T.Oracle(8)
    time_fr = [1, 0, 0, 0]  # env0 high-res (needs the one Huffman slot)
    for pointer in (False, True):
        for reverse in (False, True):
            label = f"fixvar_ptr{int(pointer)}_rev{int(reverse)}"
            probe(oracle, label, time_fr, pointer, reverse)
    oracle.save()
