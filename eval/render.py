#!/usr/bin/env python3
import json, subprocess, sys
BLOCKS=" ▁▂▃▄▅▆▇█"
# usage: render.py <binary> <input> <function> [topN] [--src source-file]
# --src is required for bytecode inputs, where the analyzed file is not the
# text to display.
args = sys.argv[1:]
src_path = None
if "--src" in args:
    i = args.index("--src"); src_path = args[i+1]; del args[i:i+2]
binary, path, fname = args[0], args[1], args[2]
top = int(args[3]) if len(args) > 3 else 0
out=subprocess.run([binary,path,'--format','json'],capture_output=True).stdout
d=json.loads(out)
src=open(src_path or path).read().splitlines()
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
