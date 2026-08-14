#!/usr/bin/env python3
"""Print one function's source with every scorer's per-line score beside it.

    ./eval/sidebyside.py <file> <function> [scorer,scorer,...]

Both targets are shown when available: `exp` is the blind human label (0-10),
`mut` is behavioural leverage from the mutation oracle (0-1, blank when the
driver corpus never reached the line).
"""
import json
import sys
from pathlib import Path

HERE = Path(__file__).parent
sys.path.insert(0, str(HERE))
from bakeoff import line_scores  # noqa: E402


def main():
    path, fname = sys.argv[1], sys.argv[2]
    want = sys.argv[3].split(",") if len(sys.argv) > 3 else None
    scorers = [s for s in json.loads((HERE / "scorers.json").read_text())
               if not want or s["name"] in want]

    truth = json.loads((HERE / "ground-truth-v2.json").read_text())
    exp = {int(k): v["importance"]
           for f in truth["functions"] if f["name"] == fname and f["file"] == path
           for k, v in f["lines"].items()}
    orc = json.loads((HERE / "mutation-oracle.json").read_text())
    mut = {int(k): v for f in orc["functions"]
           if f["file"] == path and f["name"].split(".")[-1] == fname
           for k, v in f["lines"].items() if v["covered"]}

    cols = {}
    for s in scorers:
        cols[s["name"]] = line_scores(s["binary"], s["flag"], path)[0]

    lines = sorted(set(exp) | set(mut))
    src = Path(path).read_text().splitlines()
    print(f"\n{fname}  {path}\n")
    hdr = f"{'line':>5}{'exp':>5}{'mut':>6}" + "".join(f"{n[:8]:>9}" for n in cols)
    print(hdr + "  source")
    print("-" * (len(hdr) + 50))
    for ln in lines:
        e = f"{exp[ln]:>5}" if ln in exp else f"{'-':>5}"
        m = f"{mut[ln]['leverage']:>6.2f}" if ln in mut else f"{'-':>6}"
        vals = "".join(f"{c.get(ln, float('nan')):>9.3f}" for c in cols.values())
        print(f"{ln:>5}{e}{m}{vals}  {src[ln - 1].strip()[:46]}")


if __name__ == "__main__":
    main()
