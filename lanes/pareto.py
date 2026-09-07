import subprocess, time, json, os, sys, re
import numpy as np
S=os.path.dirname(os.path.abspath(__file__))
man=open('REPO/fixtures/real-library-manifest.tsv').read().splitlines()
src=None
for l in man[1:]:
    f=l.split('\t')
    if len(f)>4 and f[2]=='av1' and f[3]=='1920' and os.path.exists(f[0]): src=f[0]; break
assert src, 'no film A'
y4m=os.path.join(S,'filmA.y4m')
if not os.path.exists(y4m):
    subprocess.run(['ffmpeg','-y','-hide_banner','-loglevel','error','-ss','00:35:00','-i',src,
                    '-frames:v','12','-vf','crop=1920:768','-pix_fmt','yuv420p','-r','24',y4m],check=True)
def run(cfg,q,out):
    a=['ffmpeg','-y','-hide_banner','-loglevel','error','-r','24','-i',y4m,'-threads','1']+cfg(q)+['-f','ivf',out]
    t=time.time(); subprocess.run(a,check=True); return time.time()-t
def psnr(out):
    p=subprocess.run(['ffmpeg','-hide_banner','-r','24','-i',out,'-r','24','-i',y4m,'-lavfi','psnr','-f','null','-'],
                     capture_output=True,text=True)
    m=re.search(r'average:([0-9.]+)',p.stderr) or re.search(r'psnr_avg:([0-9.]+)',p.stderr)
    y=re.search(r' y:([0-9.]+)',p.stderr)
    return float(y.group(1)) if y else float(m.group(1))
def rav1e(sp): return lambda q:['-c:v','librav1e','-rav1e-params',f'speed={sp}:quantizer={q}:tile_cols=1:tile_rows=1:threads=1']
def aom(cu):  return lambda q:['-c:v','libaom-av1','-cpu-used',str(cu),'-b:v','0','-crf',str(q),'-row-mt','0','-tiles','1x1']
def svt(pr):  return lambda q:['-c:v','libsvtav1','-preset',str(pr),'-crf',str(q),'-svtav1-params','lp=1']
configs=[('rav1e speed 6',rav1e(6),[50,100,150,200]),('rav1e speed 8',rav1e(8),[50,100,150,200]),
         ('rav1e speed 10',rav1e(10),[50,100,150,200]),
         ('libaom cpu-used 6',aom(6),[5,20,35,45]),('libaom cpu-used 8',aom(8),[5,20,35,45]),
         ('libaom cpu-used 9',aom(9),[5,20,35,45]),('libaom cpu-used 10',aom(10),[5,20,35,45]),
         ('svt-av1 preset 8',svt(8),[20,35,45,55]),('svt-av1 preset 10',svt(10),[20,35,45,55]),
         ('svt-av1 preset 12',svt(12),[20,35,45,55])]
res={}
for name,cfg,qs in configs:
    pts=[]; wall=0.0
    for q in qs:
        out=os.path.join(S,'t.ivf')
        try:
            w=run(cfg,q,out)
        except Exception as e:
            print(f'{name}: FAILED {e}',flush=True); pts=None; break
        wall+=w; b=os.path.getsize(out); p=psnr(out); pts.append((b,p))
        print(f'  {name} q={q}: {b} B, {p:.2f} dB, {w:.1f}s',flush=True)
    if pts: res[name]=(pts,wall)
def bd(a,b):
    # BD-rate of b vs anchor a: percent bits change at equal PSNR
    ra=np.log10([p[0] for p in a]); pa=[p[1] for p in a]
    rb=np.log10([p[0] for p in b]); pb=[p[1] for p in b]
    pa,ra=zip(*sorted(zip(pa,ra))); pb,rb=zip(*sorted(zip(pb,rb)))
    ca=np.polyfit(pa,ra,3); cb=np.polyfit(pb,rb,3)
    lo=max(min(pa),min(pb)); hi=min(max(pa),max(pb))
    ia=np.polyval(np.polyint(ca),hi)-np.polyval(np.polyint(ca),lo)
    ib=np.polyval(np.polyint(cb),hi)-np.polyval(np.polyint(cb),lo)
    return (10**((ib-ia)/(hi-lo))-1)*100
anchor=res['rav1e speed 6'][0]
print('\n| encoder preset | 4-point ladder (B / dB) | BD-rate vs rav1e speed 6 | wall, 4 points (s) | fps |')
print('|---|---|---|---|---|')
for name,(pts,wall) in res.items():
    lad=', '.join(f'{b}/{p:.2f}' for b,p in pts)
    print(f'| {name} | {lad} | {bd(anchor,pts):+.1f}% | {wall:.1f} | {4*12/wall:.2f} |')
