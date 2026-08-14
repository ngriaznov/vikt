#!/usr/bin/env python3
"""Can a scorer tell noise from behaviour? - as opposed to ranking everything.

    ./eval/separation.py

Every measurement so far has been a rank correlation over all lines, which asks
"does this scorer order the whole function the way the target does". That is a
harder question than the tool actually claims to answer, and a much harder one
than any consumer needs. An agent wants to know which lines it can skip; a
reviewer wants to know where to look first. Both are *separation* questions.

So: take the clearly-unimportant lines and the clearly-important ones, throw
away the ambiguous middle, and measure how often the scorer puts a member of the
first group below a member of the second. That statistic is the AUC of the
scorer as a binary classifier, and it is directly interpretable - 0.50 is a coin
flip, 1.00 is perfect. It is computed within each function and pooled, so a
scorer cannot win by knowing that one function is more important than another.

Two targets, same as everywhere else in this directory: the blind reader labels
(noise = importance <= 3, load-bearing = importance >= 7) and the rater-free
mutation oracle (noise = use <= 0.15, load-bearing = use >= 0.60).
"""
import json
import sys
from pathlib import Path

HERE = Path(__file__).parent
sys.path.insert(0, str(HERE))
from bakeoff import line_scores  # noqa: E402


def auc(neg, pos):
    """P(a random negative scores below a random positive), ties counted half."""
    if not neg or not pos:
        return None
    wins = 0.0
    for a in neg:
        for b in pos:
            wins += 1.0 if a < b else (0.5 if a == b else 0.0)
    return wins / (len(neg) * len(pos))


def targets():
    truth = json.loads((HERE / "ground-truth-v2.json").read_text())
    yield ("reader labels (<=3 vs >=7)", [
        (f["file"], f["name"],
         [int(k) for k, v in f["lines"].items() if v["importance"] <= 3],
         [int(k) for k, v in f["lines"].items() if v["importance"] >= 7])
        for f in truth["functions"]])

    orc = json.loads((HERE / "mutation-oracle.json").read_text())
    yield ("behaviour (<=0.15 vs >=0.60)", [
        (f["file"], f["name"],
         [int(k) for k, v in f["lines"].items()
          if v["covered"] and v["leverage"] <= 0.15],
         [int(k) for k, v in f["lines"].items()
          if v["covered"] and v["leverage"] >= 0.60])
        for f in orc["functions"]])


def main():
    scorers = json.loads((HERE / "scorers.json").read_text())
    cache = {}

    for label, groups in targets():
        print(f"\n=== separation vs {label} ===")
        npos = sum(len(p) for _, _, _, p in groups)
        nneg = sum(len(n) for _, _, n, _ in groups)
        usable = sum(1 for _, _, n, p in groups if n and p)
        print(f"    {usable} functions have both classes; "
              f"{nneg} noise lines, {npos} load-bearing lines\n")
        print(f"    {'scorer':<14}{'AUC':>7}{'fns':>6}{'missed':>8}")
        rows = []
        for sc in scorers:
            num = den = 0.0
            fns = 0
            missing = 0
            for path, _name, neg, pos in groups:
                key = (sc["name"], path)
                if key not in cache:
                    cache[key] = line_scores(sc["binary"], sc["flag"], path)[0]
                got = cache[key]
                a = [got[l] for l in neg if l in got]
                b = [got[l] for l in pos if l in got]
                missing += sum(1 for l in neg + pos if l not in got)
                v = auc(a, b)
                if v is None:
                    continue
                # Weight by pair count so a function with one noise line does
                # not count as much as one with ten.
                w = len(a) * len(b)
                num += v * w
                den += w
                fns += 1
            rows.append((sc["name"], num / den if den else float("nan"), fns, missing))
        for name, v, fns, miss in sorted(rows, key=lambda r: -r[1]):
            print(f"    {name:<14}{v:>7.3f}{fns:>6}{miss:>8}")


if __name__ == "__main__":
    main()
