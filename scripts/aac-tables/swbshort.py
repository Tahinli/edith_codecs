import numpy as np, json, swb
n=np.arange(256); ws=np.sin(np.pi/256*(n+0.5))
Ms=np.cos(np.pi/128*np.outer(n+64.5, np.arange(128)+0.5))*ws[:,None]
A=np.zeros((2048,8*128))
for i in range(8):
    A[448+128*i:448+128*i+256, i*128:(i+1)*128]=Ms
P=np.linalg.solve(A.T@A + 1e-8*np.eye(1024), A.T)
res={}
for idx in range(12):
    got=swb.blocks([swb.frame_short(k,idx) for k in range(1,17)], idx)
    edges=[]
    for blk in got:
        if blk is None: break
        X=(P@blk).reshape(8,128)
        if np.abs(X).max()<1e-9: break
        nz=np.where(np.abs(X)>1e-4*np.abs(X).max())
        edges.append(int(nz[1].max())+1)
    res[idx]=edges
    print(idx, len(edges), edges, flush=True)
json.dump(res, open('swb_short.json','w'))
