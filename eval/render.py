#!/usr/bin/env python3
import json, subprocess, sys
BLOCKS=" ▁▂▃▄▅▆▇█"
binary, path, fname = sys.argv[1], sys.argv[2], sys.argv[3]
top = int(sys.argv[4]) if len(sys.argv)>4 else 0
out=subprocess.run([binary,path,'--format','json'],capture_output=True).stdout
d=json.loads(out)
src=open(path).read().splitlines()
for f in d['functions']:
    if f['name']!=fname: continue
    rows=[]
    for sp in f['spans']:
        for ln in range(sp['start'],sp['end']+1):
            rows.append((ln,sp['score'],sp['rank'],sp['tier']))
    rows.sort()
    print(f"\n{fname}  ({path.split('/')[-1]}, {len(rows)} scored lines)")
    if top:
        by=sorted(rows,key=lambda r:-r[1])
        print("HOTTEST:")
        for ln,s,rk,t in by[:top]:
            print(f"  {ln:>5} {s:.2f} {t:<9} {src[ln-1].strip()[:60]}")
        print("COLDEST:")
        for ln,s,rk,t in by[-top:]:
            print(f"  {ln:>5} {s:.2f} {t:<9} {src[ln-1].strip()[:60]}")
    else:
        for ln,s,rk,t in rows:
            bar=BLOCKS[min(8,int(rk*8.999))]*8
            print(f"{ln:>5} {bar:<9}{s:>5.2f}  {t:<9} {src[ln-1].rstrip()[:56]}")
