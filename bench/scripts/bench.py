#!/usr/bin/env python3
"""Drive the `time` binary across several process invocations and compare runs.

A single process fixes one memory layout (link order, environment size, ASLR
draw), and that layout alone can shift a result by more than a small
optimisation does. So a reportable measurement is several invocations, each
with a different environment size, and an A/B comparison interleaves the two
binaries (A B B A ...) so drift over time hits both sides equally.

  bench.py run     --bin BIN --out DIR [--runs N] [-- ARGS for the binary]
  bench.py ab      --base BIN --cand BIN --out DIR [--rounds N] [-- ARGS]
  bench.py compare BASE_DIR_OR_CSV CAND_DIR_OR_CSV [--metric ns_per_op] [--threshold PCT] [--alpha A]

`compare` reports a change only when a permutation test on the per-invocation
medians rejects "no difference" AND the change is at least the threshold. It
needs at least 4 invocations per side (`ab --rounds 2` or more), and refuses
rows whose batch size or run settings differ between the sides.

Standard library only.
"""
import argparse
import csv
import glob
import itertools
import json
import math
import os
import random
import statistics
import subprocess
import sys


def invoke(binary, csv_path, pad, extra, plan, first):
    env = dict(os.environ)
    # Vary the environment size per invocation to vary the initial stack layout.
    env["JSON_BENCH_PAD"] = "x" * pad
    # The first invocation calibrates batch sizes and sample counts and saves
    # them; every later one replays that plan, so all of them measure the same thing.
    cmd = [binary, "--csv", csv_path, "--plan-out" if first else "--plan-in", plan] + extra
    print("+", " ".join(cmd), f"(env pad {pad})", file=sys.stderr, flush=True)
    r = subprocess.run(cmd, env=env, stdout=subprocess.DEVNULL)
    if r.returncode != 0:
        sys.exit(f"{binary} exited with {r.returncode}")


def fresh_dir(path):
    if os.path.isdir(path) and os.listdir(path):
        sys.exit(f"{path} is not empty; results from an earlier run would be mixed in. Use a new directory.")
    os.makedirs(path, exist_ok=True)


def pad_for(i):
    return 64 * i + 13 * (i % 7)


def cmd_run(a):
    fresh_dir(a.out)
    plan = os.path.join(a.out, "plan.txt")
    for i in range(a.runs):
        invoke(a.bin, os.path.join(a.out, f"run-{i:02d}.csv"), pad_for(i), a.args, plan, i == 0)


def cmd_ab(a):
    fresh_dir(a.out)
    for side in ("base", "cand"):
        os.makedirs(os.path.join(a.out, side))
    plan = os.path.join(a.out, "plan.txt")
    bins = {"base": a.base, "cand": a.cand}
    n = {"base": 0, "cand": 0}
    first = True
    for r in range(a.rounds):
        # A B B A, then B A A B, so neither side always goes first.
        order = ["base", "cand", "cand", "base"] if r % 2 == 0 else ["cand", "base", "base", "cand"]
        for side in order:
            i = n[side]
            n[side] += 1
            # Both sides see the same sequence of environment sizes.
            invoke(bins[side], os.path.join(a.out, side, f"run-{i:02d}.csv"), pad_for(i), a.args, plan, first)
            first = False


def load(path, metric):
    """Per row, the per-invocation medians of `metric`, plus what is needed to
    check that the invocations are comparable."""
    files = sorted(glob.glob(os.path.join(path, "*.csv"))) if os.path.isdir(path) else [path]
    if not files:
        sys.exit(f"no CSV files in {path}")
    medians, inners, pmus, metas = {}, {}, set(), []
    for f in files:
        per = {}
        with open(f, newline="") as fh:
            rd = csv.DictReader(fh)
            if metric not in (rd.fieldnames or []):
                sys.exit(f"{f}: no column {metric!r}; columns are {rd.fieldnames}")
            for row in rd:
                k = (row["group"], row["case"], row["impl"])
                inners.setdefault(k, set()).add(row["inner"])
                if row.get("pmu"):
                    pmus.add(row["pmu"])
                # A multiplexed or mixed-core sample is an estimate, not a count.
                if metric != "ns_per_op" and (row.get("scaled") == "1" or row.get("pmu") == "mixed"):
                    continue
                if row[metric] != "":
                    per.setdefault(k, []).append(float(row[metric]))
        for k, vals in per.items():
            medians.setdefault(k, []).append(statistics.median(vals))
        try:
            with open(f + ".meta.json") as mh:
                m = json.load(mh)
            metas.append((f, {"params": m.get("params"), "pin": m.get("pin")}))
        except (OSError, ValueError):
            metas.append((f, None))
    return {"medians": medians, "inners": inners, "pmus": pmus, "metas": metas, "files": len(files)}


def permutation_p(base, cand):
    """Two-sided permutation test on the difference of medians of the
    per-invocation medians. Exact when the number of splits is small."""
    pooled, nb = base + cand, len(base)
    observed = abs(statistics.median(cand) - statistics.median(base))
    total = math.comb(len(pooled), nb)
    if total <= 50000:
        splits = itertools.combinations(range(len(pooled)), nb)
    else:
        rng = random.Random(12345)
        splits = (rng.sample(range(len(pooled)), nb) for _ in range(50000))
        total = 50000
    hits = 0
    for idx in splits:
        s = set(idx)
        b = [pooled[i] for i in s]
        c = [pooled[i] for i in range(len(pooled)) if i not in s]
        if abs(statistics.median(c) - statistics.median(b)) >= observed - 1e-12:
            hits += 1
    return hits / total


MIN_INVOCATIONS = 4  # below this, no split of the data can reach p < 0.05


def cmd_compare(a):
    base, cand = load(a.base, a.metric), load(a.cand, a.metric)
    print(f"metric: {a.metric}; invocations: base {base['files']}, cand {cand['files']}; "
          f"threshold {a.threshold}%; alpha {a.alpha}")

    # Refuse to compare runs that were not taken the same way.
    problems = []
    known = [m for _, m in base["metas"] + cand["metas"] if m is not None]
    if len(known) != base["files"] + cand["files"]:
        problems.append("some CSV files have no .meta.json sidecar, so run settings cannot be checked")
    elif any(m != known[0] for m in known):
        problems.append("harness parameters or pinning differ between invocations (see the .meta.json sidecars)")
    if len(base["pmus"] | cand["pmus"]) > 1:
        problems.append(f"samples come from more than one core type: {sorted(base['pmus'] | cand['pmus'])}")
    if problems and not a.force:
        for p in problems:
            print("NOT COMPARABLE:", p)
        return 2
    for p in problems:
        print("WARNING (--force):", p)

    bm, cm = base["medians"], cand["medians"]
    print(f"{'group':<12} {'case':<44} {'impl':<8} {'base':>12} {'cand':>12} {'change':>8} {'p':>6}  verdict")
    bad = 0
    control = []
    for k in sorted(bm.keys() | cm.keys()):
        head = f"{k[0]:<12} {k[1]:<44} {k[2]:<8}"
        if k not in bm or k not in cm:
            print(f"{head} MISSING in {'cand' if k in bm else 'base'}")
            bad += 1
            continue
        b, c = bm[k], cm[k]
        mb, mc = statistics.median(b), statistics.median(c)
        change = (mc - mb) / mb * 100.0 if mb else float("nan")
        line = f"{head} {mb:>12.2f} {mc:>12.2f} {change:>+7.2f}%"
        if base["inners"][k] != cand["inners"][k] or len(base["inners"][k]) != 1:
            print(f"{line} {'-':>6}  NOT COMPARABLE (batch size differs: base {sorted(base['inners'][k])}, cand {sorted(cand['inners'][k])})")
            bad += 1
            continue
        if len(b) != base["files"] or len(c) != cand["files"]:
            print(f"{line} {'-':>6}  NOT COMPARABLE (missing from some invocations)")
            bad += 1
            continue
        if min(len(b), len(c)) < MIN_INVOCATIONS:
            print(f"{line} {'-':>6}  unjudged (needs {MIN_INVOCATIONS}+ invocations per side)")
            continue
        p = permutation_p(b, c)
        if k[2] == "serde":
            control.append(abs(change))
        if p >= a.alpha or abs(change) < a.threshold:
            verdict = "no change shown"
        elif change < 0:
            verdict = "LOWER"
        else:
            verdict = "HIGHER"
            if k[2] != "serde":
                bad += 1 if a.fail_on_regression else 0
        print(f"{line} {p:>6.3f}  {verdict}")
    if control:
        print(f"control: the baseline (serde) rows cannot change when only ijson changes; they moved by "
              f"median {statistics.median(control):.2f}%, max {max(control):.2f}%. "
              f"Treat ijson changes of that size as drift.")
    print("note: every row is tested separately; with many rows, expect about alpha of them to show a change by chance.")
    return 1 if bad else 0


def split_args(argv):
    return (argv[: argv.index("--")], argv[argv.index("--") + 1 :]) if "--" in argv else (argv, [])


def main():
    argv, extra = split_args(sys.argv[1:])
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("--bin", required=True)
    r.add_argument("--out", required=True)
    r.add_argument("--runs", type=int, default=5)
    r.set_defaults(fn=cmd_run)
    ab = sub.add_parser("ab")
    ab.add_argument("--base", required=True)
    ab.add_argument("--cand", required=True)
    ab.add_argument("--out", required=True)
    ab.add_argument("--rounds", type=int, default=3, help="each round is 2 invocations per side")
    ab.set_defaults(fn=cmd_ab)
    c = sub.add_parser("compare")
    c.add_argument("base")
    c.add_argument("cand")
    c.add_argument("--metric", default="ns_per_op")
    c.add_argument("--threshold", type=float, default=1.0, help="ignore changes smaller than this percent")
    c.add_argument("--alpha", type=float, default=0.05, help="significance level for the permutation test")
    c.add_argument("--fail-on-regression", action="store_true")
    c.add_argument("--force", action="store_true", help="compare even when run settings differ")
    c.set_defaults(fn=cmd_compare)
    a = p.parse_args(argv)
    a.args = extra
    sys.exit(a.fn(a) or 0)


if __name__ == "__main__":
    main()
