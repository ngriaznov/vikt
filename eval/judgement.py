#!/usr/bin/env python3
"""How close can an algorithm get to the rater's judgement? - measured honestly.

    ./eval/judgement.py [--truth eval/ground-truth-v3.json]

The project's objective, as revised: approximate the expert rater's sense of
per-line importance. That makes the labels the specification rather than a proxy,
and it fixes what the right comparison technique is - not significance tests
against an external oracle, but HELD-OUT PREDICTION: fit on some functions,
predict a function the fitter has never seen, and score agreement there.
Leave-one-function-out is the honest version of "close to your judgement";
anything fitted and scored on the same lines would be memorisation.

Features per line, all computed from the dependence graph by the 14 scorers plus
three cheap structural facts:

  - each scorer's score, rank-normalised within its function (a scorer's scale
    is arbitrary across functions; its ordering is the signal)
  - relative position in the function (the null model - included deliberately,
    since the goal is to match judgement and judgement demonstrably has a
    positional component)
  - indentation depth, and whether the tool tiers the line boundary/core

Model: ridge regression on the rank-normalised features, target = the label's
within-function rank. Tiny, convex, no tuning beyond one regulariser chosen
inside the training folds only. This is not the shipped algorithm - it is the
measurement of how much judgement the 14 signals jointly contain, which is the
ceiling any weighted combination can reach, and the weights say which fields
earn their place.
"""
import json
import math
import sys
from pathlib import Path

HERE = Path(__file__).parent
sys.path.insert(0, str(HERE))
from bakeoff import line_scores, spearman  # noqa: E402


def rank01(vals):
    """Within-function rank in [0,1], ties averaged."""
    order = sorted(range(len(vals)), key=lambda i: vals[i])
    r = [0.0] * len(vals)
    i = 0
    while i < len(order):
        j = i
        while j + 1 < len(order) and vals[order[j + 1]] == vals[order[i]]:
            j += 1
        avg = (i + j) / 2.0
        for k in range(i, j + 1):
            r[order[k]] = avg
        i = j + 1
    d = max(1, len(vals) - 1)
    return [x / d for x in r]


def solve(a, b):
    """Gaussian elimination with partial pivoting; a is n x n, b length n."""
    n = len(a)
    m = [row[:] + [b[i]] for i, row in enumerate(a)]
    for col in range(n):
        piv = max(range(col, n), key=lambda r: abs(m[r][col]))
        m[col], m[piv] = m[piv], m[col]
        if abs(m[col][col]) < 1e-12:
            continue
        inv = 1.0 / m[col][col]
        for r in range(n):
            if r == col:
                continue
            f = m[r][col] * inv
            for c in range(col, n + 1):
                m[r][c] -= f * m[col][c]
    return [m[i][n] / m[i][i] if abs(m[i][i]) > 1e-12 else 0.0 for i in range(n)]


def ridge_fit(rows, y, lam):
    """rows: list of feature vectors; returns weights (no intercept: features
    and target are both rank-normalised around comparable ranges)."""
    k = len(rows[0])
    ata = [[sum(r[i] * r[j] for r in rows) for j in range(k)] for i in range(k)]
    for i in range(k):
        ata[i][i] += lam * len(rows)
    aty = [sum(r[i] * t for r, t in zip(rows, y)) for i in range(k)]
    return solve(ata, aty)


def main():
    truth_path = sys.argv[sys.argv.index("--truth") + 1] if "--truth" in sys.argv \
        else HERE / "ground-truth-v3.json"
    truth = json.loads(Path(truth_path).read_text())
    scorers = json.loads((HERE / "scorers.json").read_text())
    names = [s["name"] for s in scorers]

    # Gather per-function feature matrices.
    functions = []
    for f in truth["functions"]:
        per_scorer = {}
        tiers = {}
        for s in scorers:
            got, _, doc = line_scores(s["binary"], s["flag"], f["file"])
            per_scorer[s["name"]] = got
            if s["name"] == "current":
                for fn in doc["functions"]:
                    for sp in fn["spans"]:
                        for ln in range(sp["start"], sp["end"] + 1):
                            tiers[ln] = sp["tier"]
        src = Path(f["file"]).read_text().splitlines()
        lines = sorted(int(k) for k in f["lines"]
                       if all(int(k) in per_scorer[n] for n in names))
        if len(lines) < 5:
            continue
        cols = {n: rank01([per_scorer[n][l] for l in lines]) for n in names}
        npos = rank01([float(i) for i in range(len(lines))])
        indent = rank01([float(len(src[l - 1]) - len(src[l - 1].lstrip()))
                         for l in lines])
        feats = []
        for i, l in enumerate(lines):
            row = [cols[n][i] for n in names]
            row.append(npos[i])
            row.append(indent[i])
            row.append(1.0 if tiers.get(l) == "boundary" else 0.0)
            feats.append(row)
        target = rank01([f["lines"][str(l)]["importance"] for l in lines])
        functions.append({"name": f["name"], "lines": lines, "x": feats,
                          "y": target,
                          "raw": {n: [per_scorer[n][l] for l in lines]
                                  for n in names},
                          "pos": npos})
    feat_names = names + ["position", "indent", "tier=boundary"]

    # Baselines: each single scorer, plus position alone, on the same functions.
    print(f"\n=== target: {Path(truth_path).name} - {len(functions)} functions, "
          f"{sum(len(f['lines']) for f in functions)} lines ===\n")
    print("-- single signals (no fitting, so no train/test split needed) --")
    singles = []
    for n in names:
        per = [spearman(fn["raw"][n], fn["y"]) for fn in functions]
        singles.append((sum(per) / len(per), n, per))
    pos_per = [spearman(fn["pos"], fn["y"]) for fn in functions]
    singles.append((sum(pos_per) / len(pos_per), "position (null)", pos_per))
    singles.sort(reverse=True)
    for avg, n, _ in singles:
        print(f"   {n:<16}{avg:>7.3f}")

    # Leave-one-function-out ridge over the combined signals.
    lambdas = [0.03, 0.1, 0.3, 1.0, 3.0]
    held = []
    weight_sum = [0.0] * len(feat_names)
    for i, fn in enumerate(functions):
        train = [f for j, f in enumerate(functions) if j != i]
        xs = [r for f in train for r in f["x"]]
        ys = [t for f in train for t in f["y"]]
        # pick lambda by inner leave-one-out over the *training* functions only
        best_lam, best_sc = None, -2.0
        for lam in lambdas:
            sc = 0.0
            for k, hold in enumerate(train):
                xt = [r for j, f in enumerate(train) if j != k for r in f["x"]]
                yt = [t for j, f in enumerate(train) if j != k for t in f["y"]]
                w = ridge_fit(xt, yt, lam)
                pred = [sum(a * b for a, b in zip(r, w)) for r in hold["x"]]
                sc += spearman(pred, hold["y"])
            sc /= len(train)
            if sc > best_sc:
                best_sc, best_lam = sc, lam
        w = ridge_fit(xs, ys, best_lam)
        for j, v in enumerate(w):
            weight_sum[j] += v
        pred = [sum(a * b for a, b in zip(r, w)) for r in fn["x"]]
        held.append((fn["name"], spearman(pred, fn["y"]), best_lam))

    print("\n-- combined (ridge over all signals), leave-one-function-out --")
    for name, r, lam in held:
        print(f"   {name:<22}{r:>7.3f}   (lambda {lam})")
    avg = sum(r for _, r, _ in held) / len(held)
    best_single = singles[0]
    print(f"\n   combined, held-out mean : {avg:.3f}")
    print(f"   best single ({best_single[1]:<9})  : {best_single[0]:.3f}")
    print(f"   position null           : {sum(pos_per)/len(pos_per):.3f}")
    wins = sum(1 for (_, r, _), s in zip(held, best_single[2]) if r > s)
    print(f"   combined beats best single on {wins}/{len(held)} functions")

    print("\n-- mean fitted weights (which signals carry judgement) --")
    order = sorted(range(len(feat_names)), key=lambda j: -abs(weight_sum[j]))
    for j in order:
        print(f"   {feat_names[j]:<16}{weight_sum[j] / len(functions):>8.3f}")


if __name__ == "__main__":
    main()
