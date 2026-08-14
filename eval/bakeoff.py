#!/usr/bin/env python3
"""Measure every candidate scorer against a ground-truth file, in one table.

    ./eval/bakeoff.py <truth.json> [--only name,name] [--csv out.csv]

Reads `eval/scorers.json`, a list of {name, binary, flag} entries, runs each one
over every function named in the truth file, and reports:

  * per-function Spearman rho
  * pooled rho over all labelled lines, with a Fisher-z 95% interval
  * score resolution (distinct values, per-function spread) - the one axis that
    involves no rater and is therefore not contaminated by who wrote the labels

Pooled rho is the headline. Per-function rho averaged over three functions has a
confidence interval of roughly +-0.5 and cannot separate anything from anything.
Pooling is done on within-function ranks, so a long function does not outvote a
short one and differing label distributions stay comparable.
"""
import json
import math
import statistics
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).parent


def spearman(xs, ys):
    def ranks(v):
        order = sorted(range(len(v)), key=lambda i: v[i])
        r = [0.0] * len(v)
        i = 0
        while i < len(order):
            j = i
            while j + 1 < len(order) and v[order[j + 1]] == v[order[i]]:
                j += 1
            avg = (i + j) / 2.0 + 1.0
            for k in range(i, j + 1):
                r[order[k]] = avg
            i = j + 1
        return r

    rx, ry = ranks(xs), ranks(ys)
    n = len(xs)
    if n < 3:
        return 0.0
    mx, my = sum(rx) / n, sum(ry) / n
    num = sum((a - mx) * (b - my) for a, b in zip(rx, ry))
    dx = sum((a - mx) ** 2 for a in rx) ** 0.5
    dy = sum((b - my) ** 2 for b in ry) ** 0.5
    return num / (dx * dy) if dx and dy else 0.0


def fisher_ci(rho, n, z=1.96):
    """95% interval on rho via the Fisher z transform."""
    if n < 5 or abs(rho) >= 1.0:
        return (float("nan"), float("nan"))
    zr = math.atanh(rho)
    se = 1.0 / math.sqrt(n - 3)
    return (math.tanh(zr - z * se), math.tanh(zr + z * se))


def line_scores(binary, flag, path):
    """Run one binary over one file, return {line: best score}, seconds, doc."""
    cmd = [binary, str(path)] + (flag.split() if flag else [])
    t0 = time.perf_counter()
    out = subprocess.run(cmd, capture_output=True, timeout=900).stdout
    dt = time.perf_counter() - t0
    doc = json.loads(out)
    got = {}
    for f in doc["functions"]:
        for sp in f["spans"]:
            for ln in range(sp["start"], sp["end"] + 1):
                if ln not in got or sp["score"] > got[ln]:
                    got[ln] = sp["score"]
    return got, dt, doc


def resolution(doc):
    """Distinct-value statistics over every function in one lowered file."""
    per_fn = []
    allv = set()
    for f in doc["functions"]:
        vals = [sp["score"] for sp in f["spans"]]
        if len(vals) < 3:
            continue
        allv.update(round(v, 6) for v in vals)
        per_fn.append((len(set(round(v, 6) for v in vals)), max(vals) - min(vals)))
    return allv, per_fn


def load_truth(path, no_delete=False):
    """Read either a blind-label file or a mutation-oracle file.

    Both end up in the same shape - functions, each with {line: importance} -
    so the same table can be produced against a human's ranking and against a
    rater-free one. Oracle lines the driver corpus never executed are dropped:
    a mutation on an unreached line cannot change anything, and scoring it zero
    would say something about the corpus rather than the line.
    """
    doc = json.loads(Path(path).read_text())
    if doc.get("schema", "").startswith("vikt-mutation-oracle"):
        key = "leverage_no_delete" if no_delete else "leverage"
        fns = []
        for f in doc["functions"]:
            lines = {
                ln: {"importance": m[key], "tier": "", "note": ""}
                for ln, m in f["lines"].items()
                if m["covered"] and m.get(key) is not None
            }
            if len(lines) >= 4:
                fns.append({"file": f["file"], "name": f["name"].split(".")[-1],
                            "character": "", "lines": lines})
        return {"functions": fns, "kind": "oracle"}
    doc["kind"] = "labels"
    return doc


def main():
    truth_path = Path(sys.argv[1])
    only = None
    if "--only" in sys.argv:
        only = set(sys.argv[sys.argv.index("--only") + 1].split(","))

    truth = load_truth(truth_path, no_delete="--no-delete" in sys.argv)
    scorers = json.loads((HERE / "scorers.json").read_text())
    if only:
        scorers = [s for s in scorers if s["name"] in only]

    files = sorted({fn["file"] for fn in truth["functions"]})
    rows = []
    for sc in scorers:
        cache = {}
        distinct = set()
        fn_res = []
        secs = 0.0
        for p in files:
            try:
                got, dt, doc = line_scores(sc["binary"], sc.get("flag", ""), p)
            except Exception as e:  # noqa: BLE001
                print(f"  !! {sc['name']} {p}: {e}", file=sys.stderr)
                continue
            cache[p] = got
            secs += dt
            d, fr = resolution(doc)
            distinct |= d
            fn_res += fr

        per_fn = []
        pooled_x, pooled_y = [], []
        for fn in truth["functions"]:
            got = cache.get(fn["file"], {})
            pairs = [
                (m["importance"], got[int(l)])
                for l, m in fn["lines"].items()
                if int(l) in got
            ]
            if len(pairs) < 4:
                continue
            per_fn.append((fn["name"], spearman(*zip(*pairs)), len(pairs)))
            xs = [p[0] for p in pairs]
            ys = [p[1] for p in pairs]
            n = len(xs)
            rx = sorted(range(n), key=lambda i: xs[i])
            ry = sorted(range(n), key=lambda i: ys[i])
            px = [0.0] * n
            py = [0.0] * n
            for i, j in enumerate(rx):
                px[j] = i / (n - 1)
            for i, j in enumerate(ry):
                py[j] = i / (n - 1)
            pooled_x += px
            pooled_y += py

        rho = spearman(pooled_x, pooled_y)
        lo, hi = fisher_ci(rho, len(pooled_x))
        spreads = sorted(s for _, s in fn_res)
        dcounts = sorted(d for d, _ in fn_res)
        rows.append(dict(
            name=sc["name"], field=sc.get("field", ""), rho=rho, lo=lo, hi=hi,
            n=len(pooled_x), per_fn=per_fn, distinct=len(distinct),
            med_d=statistics.median(dcounts) if dcounts else 0,
            p10_spread=spreads[max(0, int(len(spreads) * 0.10) - 1)] if spreads else 0,
            med_spread=statistics.median(spreads) if spreads else 0,
            secs=secs,
        ))

    rows.sort(key=lambda r: -r["rho"])
    fnames = [f["name"] for f in truth["functions"]]
    print(f"\n=== {truth_path.name}: {len(fnames)} functions, "
          f"{sum(len(f['lines']) for f in truth['functions'])} labelled lines ===\n")
    head = f"{'scorer':<13}{'field':<30}{'rho':>7}{'95% CI':>18}{'n':>6}"
    print(head)
    print("-" * len(head))
    for r in rows:
        ci = "[%.2f, %.2f]" % (r["lo"], r["hi"])
        print(f"{r['name']:<13}{r['field'][:29]:<30}{r['rho']:>7.3f}{ci:>18}{r['n']:>6}")

    print(f"\n{'scorer':<13}{'distinct':>9}{'med/fn':>8}{'p10 spread':>12}"
          f"{'med spread':>12}{'secs':>8}")
    for r in rows:
        print(f"{r['name']:<13}{r['distinct']:>9}{r['med_d']:>8.0f}"
              f"{r['p10_spread']:>12.2f}{r['med_spread']:>12.2f}{r['secs']:>8.1f}")

    print(f"\n{'scorer':<13}" + "".join(f"{n[:13]:>15}" for n in fnames))
    for r in rows:
        m = {n: v for n, v, _ in r["per_fn"]}
        print(f"{r['name']:<13}" + "".join(
            f"{m[n]:>15.3f}" if n in m else f"{'-':>15}" for n in fnames))

    if "--csv" in sys.argv:
        out = Path(sys.argv[sys.argv.index("--csv") + 1])
        with out.open("w") as fh:
            fh.write("scorer,field,rho,lo,hi,n,distinct,med_distinct,p10_spread,secs\n")
            for r in rows:
                fh.write(f"{r['name']},{r['field']},{r['rho']:.4f},{r['lo']:.4f},"
                         f"{r['hi']:.4f},{r['n']},{r['distinct']},{r['med_d']},"
                         f"{r['p10_spread']:.4f},{r['secs']:.1f}\n")
        print(f"\nwrote {out}")


if __name__ == "__main__":
    main()
