#!/usr/bin/env python3
"""Score resolution and cost, over a corpus, with no rater involved.

    ./eval/resolution.py <dir-of-python-files> [--limit N]

Agreement with labels is contestable - somebody had to write the labels. How
much gradient a scorer actually produces is not. This measures, per scorer,
over every function in a corpus:

  distinct        distinct score values across the whole corpus
  med/fn, p90/fn  distinct values within one function
  p10 spread      max-min score inside a function, at the 10th percentile.
                  This is the heatmap criterion: it says what the *flattest*
                  tenth of functions look like. A scorer can have a fine
                  overall gradient and still paint most functions one colour.
  flat            share of functions where every line gets the same score
  wall            seconds to score the corpus, and the slowest single file

Also checks determinism by scoring a sample twice and comparing byte for byte.
"""
import json
import statistics
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).parent


def files(root, limit):
    out = sorted(p for p in Path(root).rglob("*.py") if p.is_file())
    return out[:limit]


def run(binary, flag, path):
    cmd = [binary, str(path)] + flag.split()
    t0 = time.perf_counter()
    p = subprocess.run(cmd, capture_output=True, timeout=900)
    return p.stdout, time.perf_counter() - t0


def main():
    root = sys.argv[1]
    limit = int(sys.argv[sys.argv.index("--limit") + 1]) if "--limit" in sys.argv else 60
    corpus = files(root, limit)
    scorers = json.loads((HERE / "scorers.json").read_text())

    print(f"corpus: {len(corpus)} files under {root}\n")
    head = (f"{'scorer':<13}{'fns':>6}{'distinct':>9}{'med/fn':>8}{'p90/fn':>8}"
            f"{'p10 spr':>9}{'med spr':>9}{'flat%':>7}{'wall':>8}{'slowest':>9}"
            f"{'det':>5}")
    print(head)
    print("-" * len(head))
    rows = []
    for sc in scorers:
        distinct, per_fn_d, per_fn_s = set(), [], []
        wall, slowest, nfn, flat = 0.0, 0.0, 0, 0
        first_out = None
        for i, p in enumerate(corpus):
            try:
                out, dt = run(sc["binary"], sc.get("flag", ""), p)
                doc = json.loads(out)
            except Exception:  # noqa: BLE001
                continue
            if i == 0:
                first_out = out
            wall += dt
            slowest = max(slowest, dt)
            for f in doc["functions"]:
                vals = [s["function_score"] for s in f["spans"]]
                if len(vals) < 3:
                    continue
                nfn += 1
                d = {round(v, 6) for v in vals}
                distinct |= d
                per_fn_d.append(len(d))
                per_fn_s.append(max(vals) - min(vals))
                if len(d) == 1:
                    flat += 1
        det = "?"
        if first_out is not None:
            again, _ = run(sc["binary"], sc.get("flag", ""), corpus[0])
            det = "yes" if again == first_out else "NO"
        q = lambda v, x: sorted(v)[max(0, int(len(v) * x) - 1)] if v else 0  # noqa: E731
        rows.append((sc["name"], nfn, len(distinct),
                     statistics.median(per_fn_d) if per_fn_d else 0,
                     q(per_fn_d, 0.90), q(per_fn_s, 0.10),
                     statistics.median(per_fn_s) if per_fn_s else 0,
                     100.0 * flat / max(1, nfn), wall, slowest, det))

    for r in sorted(rows, key=lambda r: -r[5]):
        print(f"{r[0]:<13}{r[1]:>6}{r[2]:>9}{r[3]:>8.0f}{r[4]:>8.0f}{r[5]:>9.2f}"
              f"{r[6]:>9.2f}{r[7]:>7.1f}{r[8]:>8.1f}{r[9]:>9.2f}{r[10]:>5}")


if __name__ == "__main__":
    main()
