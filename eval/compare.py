#!/usr/bin/env python3
"""Compare scorers with the right unit of analysis, paired, and corrected.

    ./eval/compare.py [--boot N]

`bakeoff.py` pooled every labelled line into one Spearman and put a Fisher-z
interval on it with n = the number of lines. That is wrong in three ways, all of
which inflate confidence:

  1. WRONG UNIT. Lines inside one function are not independent observations -
     they share a control-flow graph, a scorer's per-function normalisation, and
     a rater's per-function frame of mind. Fisher-z with n = 264 lines claims the
     precision of 264 independent draws when the corpus contains 10 functions.
     Correct treatment: the function is the cluster, and every interval comes
     from resampling functions with replacement.

  2. UNPAIRED. Every scorer is run on the *same* functions, so the question
     "is A better than B" should be asked as a paired difference per function,
     not by checking whether two marginal intervals overlap. Overlapping
     marginals routinely hide a real paired difference, and non-overlapping
     marginals are not the test either.

  3. NO MULTIPLICITY CONTROL. Thirteen challengers were compared against the
     incumbent and the best was reported. At alpha = 0.05 that is most of a
     false positive per target before anyone writes any code. Holm-Bonferroni
     over the family of 13.

Effect sizes are Fisher z averaged over functions, weighted by (n_lines - 3),
and reported back-transformed as rho. AUC is averaged over functions weighted by
the number of (noise, load-bearing) pairs that function contributes.
"""
import json
import math
import random
import sys
from pathlib import Path

HERE = Path(__file__).parent
sys.path.insert(0, str(HERE))
from bakeoff import line_scores, load_truth, spearman  # noqa: E402

BOOT = int(sys.argv[sys.argv.index("--boot") + 1]) if "--boot" in sys.argv else 4000
RNG = random.Random(20260813)
INCUMBENT = "current"


def z(r):
    r = max(-0.999999, min(0.999999, r))
    return math.atanh(r)


def wmean(pairs):
    """Weighted mean of (value, weight); None if no weight."""
    d = sum(w for _, w in pairs)
    return sum(v * w for v, w in pairs) / d if d else None


def cluster_boot(per_fn, stat=wmean, n=BOOT):
    """Percentile interval from resampling the *functions*, not the lines."""
    if len(per_fn) < 2:
        return (float("nan"), float("nan"))
    out = []
    k = len(per_fn)
    for _ in range(n):
        s = [per_fn[RNG.randrange(k)] for _ in range(k)]
        v = stat(s)
        if v is not None:
            out.append(v)
    out.sort()
    if not out:
        return (float("nan"), float("nan"))
    return out[int(0.025 * len(out))], out[int(0.975 * len(out))]


def paired_boot(diffs, n=BOOT):
    """Interval and two-sided p for a per-function paired difference."""
    if len(diffs) < 2:
        return float("nan"), (float("nan"), float("nan")), float("nan")
    est = wmean(diffs)
    out = []
    k = len(diffs)
    for _ in range(n):
        s = [diffs[RNG.randrange(k)] for _ in range(k)]
        v = wmean(s)
        if v is not None:
            out.append(v)
    out.sort()
    lo, hi = out[int(0.025 * len(out))], out[int(0.975 * len(out))]
    # Two-sided bootstrap p: how much of the resampled mass sits on the far side
    # of zero, doubled. Floored at 1/BOOT since it cannot resolve finer.
    below = sum(1 for v in out if v <= 0) / len(out)
    p = 2 * min(below, 1 - below)
    return est, (lo, hi), max(p, 1.0 / n)


def holm(pvals):
    """Holm-Bonferroni adjusted p-values, order preserved."""
    idx = sorted(range(len(pvals)), key=lambda i: pvals[i])
    m = len(pvals)
    adj = [0.0] * m
    run = 0.0
    for rank, i in enumerate(idx):
        run = max(run, (m - rank) * pvals[i])
        adj[i] = min(1.0, run)
    return adj


def per_function_rho(truth, got_by_file):
    """[(fisher-z of rho, weight)] per function, plus the raw rho list."""
    out, raw = [], []
    for f in truth["functions"]:
        got = got_by_file.get(f["file"], {})
        pairs = [(m["importance"], got[int(k)])
                 for k, m in f["lines"].items() if int(k) in got]
        if len(pairs) < 5:
            continue
        r = spearman([p[0] for p in pairs], [p[1] for p in pairs])
        out.append((z(r), len(pairs) - 3))
        raw.append((f["name"], r, len(pairs)))
    return out, raw


def auc_one(neg, pos):
    if not neg or not pos:
        return None
    wins = sum(1.0 if a < b else (0.5 if a == b else 0.0) for a in neg for b in pos)
    return wins / (len(neg) * len(pos))


def per_function_auc(groups, got_by_file):
    out = []
    for path, neg, pos in groups:
        got = got_by_file.get(path, {})
        a = [got[l] for l in neg if l in got]
        b = [got[l] for l in pos if l in got]
        v = auc_one(a, b)
        if v is not None:
            out.append((v, len(a) * len(b)))
    return out


def main():
    scorers = json.loads((HERE / "scorers.json").read_text())
    truth_labels = load_truth(HERE / "ground-truth-v2.json")
    truth_sem = load_truth(HERE / "mutation-oracle.json", no_delete=True)
    truth_full = load_truth(HERE / "mutation-oracle.json")

    orc = json.loads((HERE / "mutation-oracle.json").read_text())
    lab = json.loads((HERE / "ground-truth-v2.json").read_text())
    groups_reader = [(f["file"],
                      [int(k) for k, v in f["lines"].items() if v["importance"] <= 4],
                      [int(k) for k, v in f["lines"].items() if v["importance"] >= 8])
                     for f in lab["functions"]]
    groups_behav = [(f["file"],
                     [int(k) for k, v in f["lines"].items()
                      if v["covered"] and v["leverage"] <= 0.15],
                     [int(k) for k, v in f["lines"].items()
                      if v["covered"] and v["leverage"] >= 0.60])
                    for f in orc["functions"]]

    # Score every file once per scorer.
    files = sorted({f["file"] for t in (truth_labels, truth_sem, truth_full)
                    for f in t["functions"]})
    cache = {}
    for sc in scorers:
        cache[sc["name"]] = {p: line_scores(sc["binary"], sc["flag"], p)[0]
                             for p in files}

    TARGETS = [
        ("reader labels (Spearman)", "rho", truth_labels, None),
        ("behaviour, semantic (Spearman)", "rho", truth_sem, None),
        ("behaviour, full (Spearman)", "rho", truth_full, None),
        ("reader labels (AUC, <=4 vs >=8)", "auc", None, groups_reader),
        ("behaviour (AUC, <=.15 vs >=.60)", "auc", None, groups_behav),
    ]

    for label, kind, truth, groups in TARGETS:
        per = {}
        for sc in scorers:
            g = cache[sc["name"]]
            per[sc["name"]] = (per_function_rho(truth, g)[0] if kind == "rho"
                               else per_function_auc(groups, g))
        nfn = len(per[INCUMBENT])
        print(f"\n{'=' * 78}\n{label}   -- {nfn} functions (the unit of analysis)\n")

        # Marginal effect, cluster-bootstrapped over functions.
        rows = []
        for sc in scorers:
            p = per[sc["name"]]
            est = wmean(p)
            lo, hi = cluster_boot(p)
            if kind == "rho":
                est, lo, hi = math.tanh(est), math.tanh(lo), math.tanh(hi)
            rows.append((sc["name"], est, lo, hi))
        rows.sort(key=lambda r: -r[1])

        # Paired comparison against the incumbent.
        base = {i: v for i, v in enumerate(per[INCUMBENT])}
        pv, diffs = {}, {}
        for sc in scorers:
            if sc["name"] == INCUMBENT:
                continue
            d = [(a - base[i][0], w) for i, (a, w) in enumerate(per[sc["name"]])
                 if i in base]
            est, (lo, hi), p = paired_boot(d)
            diffs[sc["name"]] = (est, lo, hi)
            pv[sc["name"]] = p
        names = list(pv)
        adj = holm([pv[n] for n in names])
        adjm = dict(zip(names, adj))

        unit = "rho" if kind == "rho" else "AUC"
        print(f"  {'scorer':<13}{unit:>7}{'95% CI (cluster)':>20}"
              f"{'paired vs current':>20}{'p':>8}{'p(Holm)':>9}")
        for name, est, lo, hi in rows:
            ci = "[%.2f, %.2f]" % (lo, hi)
            if name == INCUMBENT:
                print(f"  {name:<13}{est:>7.3f}{ci:>20}{'--':>20}{'':>8}{'':>9}")
                continue
            de, dlo, dhi = diffs[name]
            dz = "%+.2f [%+.2f, %+.2f]" % (de, dlo, dhi)
            star = "*" if adjm[name] < 0.05 else " "
            print(f"  {name:<13}{est:>7.3f}{ci:>20}{dz:>20}"
                  f"{pv[name]:>8.3f}{adjm[name]:>8.3f}{star}")
        print("  paired difference is in Fisher z for rho, in AUC points for AUC;"
              "\n  * = survives Holm-Bonferroni over the 13 challengers.")


if __name__ == "__main__":
    main()
