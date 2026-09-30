import sys
import cmpff
def shape(cell, obu, frame_d=0, frame_s=0, planes=(1,2)):
    w,h,ssx,ssy,depth,pix=cmpff.probe_geometry(obu)
    bps=1 if depth<=8 else 2
    sizes,fsz=cmpff.plane_spans(w,h,ssx,ssy,bps)
    disp=cmpff.split(cmpff.ffmpeg_frames(obu,pix),sizes,fsz,cell)
    a=open(f"/home/tahinli/.cache/seed422/{cell}/ours.f{frame_d}",'rb').read()
    b=disp[frame_s]
    cw=-(-w>>ssx); ch=-(-h>>ssy)
    print(f"== {cell} {w}x{h} d{depth} chroma {cw}x{ch}  decode f{frame_d} vs display f{frame_s}")
    for pi in planes:
        name="YUV"[pi]
        off=sum(sizes[:pi])*bps
        blocks={}
        for r in range(ch):
            base=off+(r*cw)*bps
            for c in range(cw):
                if a[base+c*bps:base+(c+1)*bps]!=b[base+c*bps:base+(c+1)*bps]:
                    blocks.setdefault((r//8,c//8),[]).append((r%8,c%8))
        print(f"  {name}: {len(blocks)} 8x8 blocks wrong")
        for k in sorted(blocks):
            v=blocks[k]
            rows=sorted({p[0] for p in v}); cols=sorted({p[1] for p in v})
            print(f"    blk(r{k[0]},c{k[1]}) abs rows {k[0]*8}..{k[0]*8+7} cols {k[1]*8}..{k[1]*8+7}"
                  f": {len(v)}/64 wrong, subrows {rows[0]}..{rows[-1]} subcols {cols[0]}..{cols[-1]}")
if __name__=="__main__":
    shape(sys.argv[1], sys.argv[2], int(sys.argv[3]) if len(sys.argv)>3 else 0)
