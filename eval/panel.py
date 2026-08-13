#!/usr/bin/env python3
"""Annotate a function's source with the panel's per-line importance.

    ./eval/panel.py <file> <function-name>

This is the product shape of everything eval/ has measured: the lean instrument
panel - current, schur, pivot, trophic, strahler, plus position and indent -
combined by ridge weights fitted on the oracle's blind labels, then projected
onto one function as a per-line heat.

Honesty rule: if the requested function is itself in the oracle's label set,
it is EXCLUDED from the fit before prediction, so what you see is a held-out
prediction, never a memorised one. Unlabelled functions are predicted by a
panel fitted on all sixteen labelled ones.

Output columns: line number, heat bar (panel score, within-function rank),
panel score in [0,1], the single instrument contributing most to that line's
score above the panel's average, and the source line. Oracle labels are shown
alongside when the function has them - they are the target, not an input.
"""
import json
import sys
from pathlib import Path

HERE = Path(__file__).parent
sys.path.insert(0, str(HERE))
from bakeoff import line_scores  # noqa: E402
from judgement import rank01, ridge_fit  # noqa: E402

PANEL = ["current", "schur", "pivot", "trophic", "strahler"]
LAMBDA = 0.03  # chosen by inner cross-validation in judgement.py
BLOCKS = " ▁▂▃▄▅▆▇█"


def features_for(f_lines, per_scorer, tiers, src):
    """One row per line: panel ranks + position + indent + boundary flag."""
    cols = {n: rank01([per_scorer[n][l] for l in f_lines]) for n in PANEL}
    npos = rank01([float(i) for i in range(len(f_lines))])
    indent = rank01([float(len(src[l - 1]) - len(src[l - 1].lstrip()))
                     for l in f_lines])
    rows = []
    for i, l in enumerate(f_lines):
        row = [cols[n][i] for n in PANEL]
        row.append(npos[i])
        row.append(indent[i])
        row.append(1.0 if tiers.get(l) == "boundary" else 0.0)
        rows.append(row)
    return rows


def main():
    path, fname = sys.argv[1], sys.argv[2]
    scorers = {s["name"]: s for s in json.loads((HERE / "scorers.json").read_text())
               if s["name"] in PANEL}
    if len(scorers) != len(PANEL):
        sys.exit(f"scorers.json is missing panel members: "
                 f"{sorted(set(PANEL) - set(scorers))}")
    truth = json.loads((HERE / "ground-truth-v3.json").read_text())
    feat_names = PANEL + ["position", "indent", "tier=boundary"]

    # ---- fit on the oracle corpus, excluding the target if labelled --------
    xs, ys = [], []
    target_is_labelled = False
    for f in truth["functions"]:
        if f["file"] == path and f["name"] == fname:
            target_is_labelled = True
            continue
        per_scorer = {n: line_scores(s["binary"], s["flag"], f["file"])[0]
                      for n, s in scorers.items()}
        _, _, doc = line_scores(scorers["current"]["binary"],
                                scorers["current"]["flag"], f["file"])
        tiers = {ln: sp["tier"] for fn in doc["functions"] for sp in fn["spans"]
                 for ln in range(sp["start"], sp["end"] + 1)}
        src = Path(f["file"]).read_text().splitlines()
        lines = sorted(int(k) for k in f["lines"]
                       if all(int(k) in per_scorer[n] for n in PANEL))
        if len(lines) < 5:
            continue
        xs += features_for(lines, per_scorer, tiers, src)
        ys += rank01([f["lines"][str(l)]["importance"] for l in lines])
    w = ridge_fit(xs, ys, LAMBDA)

    # ---- score the target --------------------------------------------------
    per_scorer = {}
    docs = {}
    for n, s in scorers.items():
        got, _, doc = line_scores(s["binary"], s["flag"], path)
        per_scorer[n] = got
        docs[n] = doc
    cur = docs["current"]
    spans = {}
    tiers = {}
    for fn in cur["functions"]:
        if fn["name"] != fname:
            continue
        for sp in fn["spans"]:
            for ln in range(sp["start"], sp["end"] + 1):
                spans[ln] = sp
                tiers[ln] = sp["tier"]
    if not spans:
        sys.exit(f"function {fname!r} not found in {path} "
                 f"(names: {[fn['name'] for fn in cur['functions']][:20]})")
    lines = sorted(l for l in spans if all(l in per_scorer[n] for n in PANEL))
    src = Path(path).read_text().splitlines()
    rows = features_for(lines, per_scorer, tiers, src)
    pred = [sum(a * b for a, b in zip(r, w)) for r in rows]
    heat = rank01(pred)

    labels = {}
    if target_is_labelled:
        for f in truth["functions"]:
            if f["file"] == path and f["name"] == fname:
                labels = {int(k): v["importance"] for k, v in f["lines"].items()}

    # Per-line "lead instrument": the panel member whose (weight x rank) term
    # exceeds its own average contribution by the most on this line.
    avg_term = [sum(r[j] for r in rows) / len(rows) * w[j] for j in range(len(PANEL))]
    leads = []
    for r in rows:
        deltas = [(r[j] * w[j] - avg_term[j], PANEL[j]) for j in range(len(PANEL))]
        d, name = max(deltas)
        leads.append(name if d > 0.005 else "")

    mode = "HELD-OUT (function excluded from fit)" if target_is_labelled \
        else "fitted on all 16 oracle functions"
    print(f"\npanel: {' + '.join(PANEL)} + position/indent/tier")
    print(f"fit:   ridge lambda={LAMBDA}, {mode}")
    print(f"\n{fname}  {path}\n")
    hdr = f"{'line':>5} {'heat':<10}{'score':>6} {'oracle':>7}  {'lead':<9} source"
    print(hdr)
    print("-" * (len(hdr) + 40))
    lo, hi = min(pred), max(pred)
    for i, l in enumerate(lines):
        score = (pred[i] - lo) / (hi - lo) if hi > lo else 0.5
        bar = BLOCKS[min(8, int(heat[i] * 8.999))] * 8
        lab = f"{labels[l]:>5}/10" if l in labels else f"{'':>7}"
        print(f"{l:>5} {bar:<10}{score:>6.2f} {lab}  {leads[i]:<9} "
              f"{src[l - 1].rstrip()[:66]}")
    print(f"\nweights: " + "  ".join(f"{n}={v:+.2f}"
          for n, v in zip(feat_names, w)))


if __name__ == "__main__":
    main()
