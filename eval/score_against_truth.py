#!/usr/bin/env python3
"""Score a vikt binary against the blind expert labels in ground-truth-v1.json.

    ./eval/score_against_truth.py <path-to-vikt-binary> [--scorer NAME]

Reports Spearman rank correlation per function and averaged, plus the lines each
scorer disagrees with most, which is where the interesting information is, not
in the headline number.
"""
import json
import subprocess
import sys
from pathlib import Path

TRUTH = Path(__file__).parent / "ground-truth-v1.json"


def spearman(xs, ys):
    """Rank correlation with average ranks for ties."""
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
    mx, my = sum(rx) / n, sum(ry) / n
    num = sum((a - mx) * (b - my) for a, b in zip(rx, ry))
    dx = sum((a - mx) ** 2 for a in rx) ** 0.5
    dy = sum((b - my) ** 2 for b in ry) ** 0.5
    return num / (dx * dy) if dx and dy else 0.0


def main():
    binary = sys.argv[1]
    scorer = None
    if "--scorer" in sys.argv:
        scorer = sys.argv[sys.argv.index("--scorer") + 1]

    truth = json.loads(TRUTH.read_text())
    label = f"{binary}" + (f" --scorer {scorer}" if scorer else "")
    print(f"\n=== {label} ===")

    per_fn = []
    all_pairs = []
    for fn in truth["functions"]:
        cmd = [binary, fn["file"]]
        if scorer:
            cmd += ["--scorer", scorer]
        try:
            out = subprocess.run(cmd, capture_output=True, timeout=600).stdout
            doc = json.loads(out)
        except Exception as e:  # noqa: BLE001
            print(f"  !! {fn['file']}: {e}")
            continue

        # Line -> score, taking the most salient span covering that line.
        got = {}
        for f in doc["functions"]:
            for sp in f["spans"]:
                for ln in range(sp["start"], sp["end"] + 1):
                    if ln not in got or sp["function_score"] > got[ln]:
                        got[ln] = sp["function_score"]

        pairs = []
        missing = []
        for ln_s, meta in fn["lines"].items():
            ln = int(ln_s)
            if ln in got:
                pairs.append((ln, meta["importance"], got[ln], meta["note"]))
            else:
                missing.append(ln)

        if len(pairs) < 4:
            print(f"  {fn['name']:<18} too few mapped lines ({len(pairs)}), skipped")
            continue

        rho = spearman([p[1] for p in pairs], [p[2] for p in pairs])
        per_fn.append((fn["name"], rho, len(pairs), len(missing)))
        all_pairs += [(fn["name"], *p) for p in pairs]

    print(f"  {'function':<20}{'rho':>7}{'lines':>7}{'unmapped':>10}")
    for name, rho, n, miss in per_fn:
        print(f"  {name:<20}{rho:>7.3f}{n:>7}{miss:>10}")
    if per_fn:
        avg = sum(r for _, r, _, _ in per_fn) / len(per_fn)
        print(f"  {'MEAN':<20}{avg:>7.3f}")

    # Biggest disagreements, by rank distance within each function.
    print("\n  worst disagreements (expert rank vs scorer rank, within function):")
    by_fn = {}
    for name, ln, imp, sc, note in all_pairs:
        by_fn.setdefault(name, []).append((ln, imp, sc, note))
    rows = []
    for name, items in by_fn.items():
        n = len(items)
        imp_rank = {ln: i for i, (ln, _, _, _) in enumerate(sorted(items, key=lambda t: t[1]))}
        sc_rank = {ln: i for i, (ln, _, _, _) in enumerate(sorted(items, key=lambda t: t[2]))}
        for ln, imp, sc, note in items:
            d = (imp_rank[ln] - sc_rank[ln]) / max(1, n - 1)
            rows.append((abs(d), d, name, ln, imp, sc, note))
    rows.sort(reverse=True)
    for _, d, name, ln, imp, sc, note in rows[:8]:
        direction = "scorer UNDER-rates" if d > 0 else "scorer OVER-rates"
        print(f"    {direction}  {name}:{ln}  expert={imp}/10 score={sc:.2f}  {note[:52]}")


if __name__ == "__main__":
    main()
