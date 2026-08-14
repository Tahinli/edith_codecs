import swb, aacprobe as A, sys
def frame_tns(max_sfb, n_filt, coef_res, length, order, direction, compress, coefs, sf_index=3):
    w=A.BitW()
    w.w(0,3).w(0,4).w(200,8)
    w.w(0,1).w(0,2).w(0,1).w(max_sfb,6).w(0,1)
    w.w(3,4); swb.sect_len(w,max_sfb,5)
    w.wbits([0]*max_sfb)
    w.w(0,1)   # pulse_data_present
    w.w(1,1)   # tns_data_present
    w.w(n_filt,2)
    if n_filt:
        w.w(coef_res,1)
        for f in range(n_filt):
            w.w(length,6); w.w(order,5)
            if order:
                w.w(direction,1); w.w(compress,1)
                bits = coef_res + 3 - compress
                for c in coefs[:order]:
                    w.w(c & ((1<<bits)-1), bits)
    w.w(0,1)   # gain_control
    w.wbits([1]*6000)
    return A.adts(w.pack(pad=0), sf_index=sf_index)
sil=swb.silent(3)
cases=[
 (20,1,0,8,4,0,0,[1,2,-1,0]),
 (20,1,0,8,4,1,0,[1,2,-1,0]),
 (20,1,1,10,6,0,0,[3,-2,1,0,2,-1]),
 (20,1,1,10,6,0,1,[1,-1,1,0,1,-1]),
 (30,1,0,40,3,0,0,[2,-2,1]),
 (20,2,0,6,3,0,0,[1,1,-1]),
]
frames=[frame_tns(*c) for c in cases]
open('/tmp/claude-1000/-home-tahinli-Documents-Code-Rust-edith-codecs/6410b1a2-7cdb-42c3-ac18-2bbfd6cd1284/scratchpad/short/tns.aac','wb').write(sil+b''.join(f+sil+sil for f in frames))
print('cases', len(cases))
