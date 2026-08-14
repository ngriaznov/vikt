#!/usr/bin/env python3
"""Behavioural weight-refit report.

    ./eval/calibration/refit.py

Reads every `eval/calibration/*.jsonl` row (one row per line the tool
scored, with its 7 panel features and the mutation-testing `kill_rate`
observed for that line) and asks: if the panel weights had been fitted
against *behavioural leverage* (does mutating this line get caught?)
instead of the blind-reader judgement they were actually fitted against
(`eval/judgement.py`, `eval/ground-truth-v3.json` /
`eval/ground-truth-js-v1.json`), would the weights look different, and
would they generalise better?

Pooling and features
---------------------
Rows are pooled by the panel profile that produced them (matches
`vikt_core::panel::PanelProfile`):

  - `instruction` -- bytecode-granular frontends: python + rust here.
  - `statement`   -- AST-granular frontends: javascript here.

Each row's `instruments` dict already *is* the exact 7-vector
`line_features()` computes and `score()` dots against the shipped
weights -- `[current, schur, pivot, trophic, strahler, position,
boundary]` -- so no re-derivation or re-normalisation happens here; we
read the seven values off in that order and regress `kill_rate` on
them directly, no intercept, exactly mirroring the shipped panel's
dot-product (so "current weights' score" and "refit weights' score"
are computed identically and differ only in the weight vector).

Fitting
-------
Ridge, lambda in {0.01, 0.03, 0.1, 0.3, 1.0}, chosen by inner
leave-one-group-out (group = dataset root, i.e. the source repo the
rows came from) over the training groups: for each candidate lambda,
leave each training group out in turn, fit on the rest, score Spearman
rho against the held-out group's kill_rate, and average. The lambda
with the best mean inner rho is used for the fold's fit.

Both profiles evaluated here have few groups (3 for instruction, 2 for
statement). When a fold's *training* set contains fewer than 2 groups,
group-level inner CV is impossible by construction -- there's nothing
left to leave out. In that case (every outer fold of the 2-group
statement profile) this script falls back to row-level leave-one-out
CV within the lone training group to pick lambda, and says so in the
report. This is a documented compromise, not silent substitution.

Evaluation
----------
Leave-one-group-out: for each held-out group, Spearman rho of (a) the
CURRENT shipped weights' dot-product score, (b) the refit weights'
score (trained on the other groups in the same profile only), and (c)
the position feature alone, each against that group's kill_rate.
Report per-group and the mean across groups.

The full-data refit vector reported per profile (the one in the
headline coefficient table) is fit on *all* groups of that profile,
lambda chosen the same way (LOGO over all of that profile's groups).
It is a different fit from any single outer fold above -- those folds
each exclude one group -- so it is reported separately, not averaged
into the held-out numbers.
"""
import json
import sys
from pathlib import Path

import numpy as np

HERE = Path(__file__).parent

# vikt_core::panel -- INSTRUCTION_WEIGHTS / STATEMENT_WEIGHTS, in the same
# feature order as everything else here. Quoted, not imported: this script
# must not depend on a build of the Rust crate to run, and the whole point
# is comparing against exactly what's shipped today.
CURRENT_WEIGHTS = {
    "instruction": np.array(
        [0.1643, 0.1654, 0.1888, 0.1092, 0.1097, 0.2505, -0.0521]
    ),
    "statement": np.array(
        [0.1386, 0.1109, 0.0917, 0.1559, 0.1587, 0.1800, 0.0522]
    ),
}
FEATURE_NAMES = ["current", "schur", "pivot", "trophic", "strahler", "position", "boundary"]
LAMBDAS = [0.01, 0.03, 0.1, 0.3, 1.0]
POSITION_IDX = FEATURE_NAMES.index("position")


def spearman(xs, ys):
    """Rank correlation, ties averaged. Matches eval/bakeoff.py::spearman."""
    xs = np.asarray(xs, dtype=float)
    ys = np.asarray(ys, dtype=float)
    n = len(xs)
    if n < 3:
        return 0.0

    def ranks(v):
        order = np.argsort(v, kind="mergesort")
        r = np.zeros(n)
        i = 0
        while i < n:
            j = i
            while j + 1 < n and v[order[j + 1]] == v[order[i]]:
                j += 1
            avg = (i + j) / 2.0 + 1.0
            r[order[i : j + 1]] = avg
            i = j + 1
        return r

    rx, ry = ranks(xs), ranks(ys)
    mx, my = rx.mean(), ry.mean()
    dx, dy = rx - mx, ry - my
    denom = np.sqrt((dx**2).sum()) * np.sqrt((dy**2).sum())
    return float((dx * dy).sum() / denom) if denom else 0.0


def ridge_fit(X, y, lam):
    """No-intercept ridge, closed form. Matches eval/judgement.py::ridge_fit's
    lambda scaling (penalty scaled by row count) so the grid means the same
    thing project-wide."""
    n, k = X.shape
    ata = X.T @ X + lam * n * np.eye(k)
    aty = X.T @ y
    return np.linalg.solve(ata, aty)


def load_rows():
    """One row per eval/calibration/*.jsonl line. `root` is the group key
    (dataset identity); `profile` is the pooling key."""
    rows = []
    for path in sorted(HERE.glob("*.jsonl")):
        with open(path) as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                d = json.loads(line)
                x = np.array([d["instruments"][n] for n in FEATURE_NAMES])
                rows.append(
                    {
                        "root": d["root"],
                        "profile": d["profile"],
                        "language": d["language"],
                        "x": x,
                        "y": d["kill_rate"],
                        "source": path.name,
                    }
                )
    return rows


def group_matrices(rows):
    """profile -> group (root) -> (X, y)."""
    out = {}
    for r in rows:
        g = out.setdefault(r["profile"], {}).setdefault(r["root"], {"x": [], "y": []})
        g["x"].append(r["x"])
        g["y"].append(r["y"])
    return {
        profile: {
            root: (np.array(g["x"]), np.array(g["y"])) for root, g in groups.items()
        }
        for profile, groups in out.items()
    }


def select_lambda_logo(train_groups, lambdas):
    """Inner leave-one-group-out over the training groups. Returns
    (best_lambda, mean_inner_rho, "logo"). Requires >=2 groups."""
    names = list(train_groups)
    best_lam, best_score = lambdas[0], -2.0
    for lam in lambdas:
        scores = []
        for held in names:
            train_x = np.concatenate(
                [train_groups[g][0] for g in names if g != held]
            )
            train_y = np.concatenate(
                [train_groups[g][1] for g in names if g != held]
            )
            w = ridge_fit(train_x, train_y, lam)
            hx, hy = train_groups[held]
            scores.append(spearman(hx @ w, hy))
        mean_score = sum(scores) / len(scores)
        if mean_score > best_score:
            best_lam, best_score = lam, mean_score
    return best_lam, best_score, "logo"


def select_lambda_row_loo(X, y, lambdas):
    """Fallback when the training set has only one group: row-level
    leave-one-out CV within it, pooled Spearman of the LOO predictions.
    Returns (best_lambda, rho, "row-loo")."""
    n = len(y)
    idx = np.arange(n)
    best_lam, best_score = lambdas[0], -2.0
    for lam in lambdas:
        preds = np.zeros(n)
        for i in range(n):
            mask = idx != i
            w = ridge_fit(X[mask], y[mask], lam)
            preds[i] = X[i] @ w
        score = spearman(preds, y)
        if score > best_score:
            best_lam, best_score = lam, score
    return best_lam, best_score, "row-loo"


def select_lambda(train_groups, lambdas):
    if len(train_groups) >= 2:
        return select_lambda_logo(train_groups, lambdas)
    (only_root,) = train_groups
    X, y = train_groups[only_root]
    return select_lambda_row_loo(X, y, lambdas)


def outer_logo_eval(groups, current_weights):
    """For each held-out group: rho of current-weight score, refit score,
    and position alone, against kill_rate. Returns list of dict rows plus
    the (lambda, selection-method) used for each fold."""
    names = list(groups)
    results = []
    for held in names:
        train_groups = {g: groups[g] for g in names if g != held}
        lam, inner_rho, method = select_lambda(train_groups, LAMBDAS)
        train_x = np.concatenate([train_groups[g][0] for g in train_groups])
        train_y = np.concatenate([train_groups[g][1] for g in train_groups])
        w = ridge_fit(train_x, train_y, lam)

        hx, hy = groups[held]
        results.append(
            {
                "group": held,
                "n": len(hy),
                "rho_current": spearman(hx @ current_weights, hy),
                "rho_refit": spearman(hx @ w, hy),
                "rho_position": spearman(hx[:, POSITION_IDX], hy),
                "lambda": lam,
                "inner_rho": inner_rho,
                "inner_method": method,
                "weights": w,
            }
        )
    return results


def full_profile_fit(groups, lambdas):
    """Fit on every group of the profile; lambda by LOGO over those same
    groups (or row-LOO fallback if there's only one)."""
    lam, inner_rho, method = select_lambda(groups, lambdas)
    X = np.concatenate([groups[g][0] for g in groups])
    y = np.concatenate([groups[g][1] for g in groups])
    w = ridge_fit(X, y, lam)
    return w, lam, inner_rho, method, len(y)


def main():
    rows = load_rows()
    by_profile = group_matrices(rows)

    report = {}
    for profile in ("instruction", "statement"):
        groups = by_profile.get(profile, {})
        if not groups:
            continue
        current_w = CURRENT_WEIGHTS[profile]
        outer = outer_logo_eval(groups, current_w)
        full_w, full_lam, full_inner_rho, full_method, n_total = full_profile_fit(
            groups, LAMBDAS
        )
        report[profile] = {
            "groups": {g: len(y) for g, (_, y) in groups.items()},
            "n_total": n_total,
            "outer": outer,
            "full_weights": full_w,
            "full_lambda": full_lam,
            "full_inner_rho": full_inner_rho,
            "full_method": full_method,
            "current_weights": current_w,
        }

    write_report(report)
    print_summary(report)


def fmt_vec(w):
    return "[" + ", ".join(f"{v:+.4f}" for v in w) + "]"


def print_summary(report):
    for profile, r in report.items():
        print(f"\n=== {profile} ===")
        print(f"groups: {r['groups']}  (n={r['n_total']})")
        print(f"full-data refit: lambda={r['full_lambda']}  weights={fmt_vec(r['full_weights'])}")
        print(f"{'group':<14}{'n':>5}{'current':>10}{'refit':>10}{'position':>10}  lambda  method")
        for o in r["outer"]:
            print(
                f"{o['group']:<14}{o['n']:>5}{o['rho_current']:>10.3f}"
                f"{o['rho_refit']:>10.3f}{o['rho_position']:>10.3f}"
                f"  {o['lambda']:<6}  {o['inner_method']}"
            )
        mc = sum(o["rho_current"] for o in r["outer"]) / len(r["outer"])
        mr = sum(o["rho_refit"] for o in r["outer"]) / len(r["outer"])
        mp = sum(o["rho_position"] for o in r["outer"]) / len(r["outer"])
        print(f"{'mean':<14}{'':>5}{mc:>10.3f}{mr:>10.3f}{mp:>10.3f}")


def write_report(report):
    lines = []
    lines.append("# Weight-refit report (behavioural)")
    lines.append("")
    lines.append(
        "Generated by `eval/calibration/refit.py` from every "
        "`eval/calibration/*.jsonl` row. Ridge regression of mutation-"
        "testing `kill_rate` on the panel's 7 per-line features "
        "(`[current, schur, pivot, trophic, strahler, position, "
        "boundary]`), fitted separately per profile and evaluated "
        "leave-one-group-out (group = dataset root). Numbers only -- "
        "no production code or shipped weight changed."
    )
    lines.append("")

    for profile, r in report.items():
        substrate = "python + rust" if profile == "instruction" else "javascript"
        lines.append(f"## `{profile}` profile ({substrate})")
        lines.append("")
        lines.append("Row/group counts:")
        lines.append("")
        lines.append("| group (root) | rows |")
        lines.append("|---|---|")
        for g, n in r["groups"].items():
            lines.append(f"| `{g}` | {n} |")
        lines.append(f"| **total** | **{r['n_total']}** |")
        lines.append("")

        method_note = (
            f"chosen by row-level leave-one-out CV within the single training "
            f"group (fallback: fewer than 2 training groups available for "
            f"group-level inner CV)"
            if r["full_method"] == "row-loo"
            else "chosen by inner leave-one-group-out over this profile's groups"
        )
        lines.append(
            f"Full-data refit (all {len(r['groups'])} group(s) pooled): "
            f"lambda = **{r['full_lambda']}** ({method_note}, inner rho "
            f"{r['full_inner_rho']:.3f})."
        )
        lines.append("")
        lines.append("| feature | current shipped weight | refit weight (full data) |")
        lines.append("|---|---|---|")
        for i, name in enumerate(FEATURE_NAMES):
            lines.append(
                f"| `{name}` | {r['current_weights'][i]:+.4f} | "
                f"{r['full_weights'][i]:+.4f} |"
            )
        lines.append("")

        lines.append(
            "Leave-one-group-out evaluation (Spearman rho of each score "
            "against `kill_rate`; refit weights for each row are trained "
            "on the *other* group(s) in this profile only, never on the "
            "held-out group):"
        )
        lines.append("")
        lines.append(
            "| held-out group | n | current weights | refit weights | position alone | inner lambda | inner method |"
        )
        lines.append("|---|---|---|---|---|---|---|")
        for o in r["outer"]:
            lines.append(
                f"| `{o['group']}` | {o['n']} | {o['rho_current']:.3f} | "
                f"{o['rho_refit']:.3f} | {o['rho_position']:.3f} | "
                f"{o['lambda']} | {o['inner_method']} |"
            )
        mc = sum(o["rho_current"] for o in r["outer"]) / len(r["outer"])
        mr = sum(o["rho_refit"] for o in r["outer"]) / len(r["outer"])
        mp = sum(o["rho_position"] for o in r["outer"]) / len(r["outer"])
        lines.append(f"| **mean** | | **{mc:.3f}** | **{mr:.3f}** | **{mp:.3f}** | | |")
        lines.append("")

        if profile == "statement":
            lines.append(
                "> Both outer folds above train on a single remaining group "
                "(`mime-types` alone, or `ms` alone -- this profile only has "
                "two groups total), so the inner lambda selection for those "
                "folds is the row-level leave-one-out fallback, not "
                "group-level inner CV. Treat this profile's held-out numbers "
                "as indicative, not well-powered: two groups is barely "
                "enough to run leave-one-group-out at all, and the variance "
                "of a 2-fold mean is large."
            )
            lines.append("")

    lines.append("## Does the refit beat the current weights, held out?")
    lines.append("")

    verdict_bits = []
    for profile, r in report.items():
        mc = sum(o["rho_current"] for o in r["outer"]) / len(r["outer"])
        mr = sum(o["rho_refit"] for o in r["outer"]) / len(r["outer"])
        wins = sum(1 for o in r["outer"] if o["rho_refit"] > o["rho_current"])
        total = len(r["outer"])
        verdict_bits.append((profile, mc, mr, wins, total))

    para = []
    for profile, mc, mr, wins, total in verdict_bits:
        substrate = "python + rust" if profile == "instruction" else "javascript"
        if mr > mc:
            verb = "do beat"
        elif mr < mc:
            verb = "do not beat"
        else:
            verb = "tie"
        para.append(
            f"On the `{profile}` profile ({substrate}, {total} held-out "
            f"group{'s' if total != 1 else ''}), the behaviourally-refit "
            f"weights {verb} the current shipped weights held out: mean "
            f"Spearman rho {mr:.3f} (refit) vs {mc:.3f} (current), refit "
            f"winning on {wins}/{total} groups."
        )
    lines.append(" ".join(para))
    lines.append("")
    lines.append(
        "Caveat, and it matters: `kill_rate` measures **behavioural "
        "leverage** -- whether a line, when mutated, changes something a "
        "test suite can catch. It is not the same target the shipped "
        "weights were fitted against. `INSTRUCTION_WEIGHTS` and "
        "`STATEMENT_WEIGHTS` (see `crates/vikt-core/src/panel.rs`) were "
        "ridge-fitted against a blind expert rater's per-line **reader "
        "importance** judgements (`eval/ground-truth-v3.json`, "
        "`eval/ground-truth-js-v1.json`) -- what a careful human reading the "
        "source, with no scorer output in front of them, marks as mattering. "
        "A line can carry high behavioural leverage and low reader "
        "importance (an easily-mutated boundary check nobody would call "
        "central to the function) or the reverse (a load-bearing "
        "structural line a good mutation operator rarely touches). Where "
        "this report shows the refit beating the current weights, that is "
        "evidence the panel features predict mutation-kill sensitivity "
        "better with different coefficients -- it is not evidence the "
        "current weights are wrong for the target they were actually "
        "fitted against, and it should not be read as license to hand-edit "
        "`panel.rs`. Where the refit does not beat the current weights, "
        "that says plainly: **it does not**, on this data, at this sample "
        "size."
    )
    lines.append("")
    lines.append(
        "Sample sizes here are small relative to the ground-truth fits "
        + "; ".join(
            f"({len(r['groups'])} groups / {r['n_total']} rows for `{profile}`)"
            for profile, r in report.items()
        )
        + " against 16 functions/425 lines and 9 "
        "functions/100 lines respectively, and every group is a different "
        "repository rather than a different function within one codebase, "
        "so held-out rho here has much higher fold-to-fold variance than "
        "the numbers quoted in `panel.rs`'s doc comments. This report "
        "should be read as a first behavioural calibration check, not as "
        "grounds to replace the shipped weights on its own."
    )
    lines.append("")

    (HERE / "REFIT.md").write_text("\n".join(lines) + "\n")


if __name__ == "__main__":
    sys.exit(main())
