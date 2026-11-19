#!/usr/bin/env python3
"""Drive the `time` binary across several process invocations and compare runs.

A single process fixes one memory layout (link order, environment size, ASLR
draw), and that layout alone can shift a result by more than a small
optimisation does. So a reportable measurement is several invocations, each
with a different environment size, and an A/B comparison interleaves the two
binaries (A B B A ...) so drift over time hits both sides equally.

  bench.py run     --bin BIN --out DIR [--runs N] [-- ARGS for the binary]
  bench.py ab      --base BIN --cand BIN --out DIR [--rounds N] [-- ARGS]
  bench.py compare BASE_DIR_OR_CSV CAND_DIR_OR_CSV [--metric ns_per_op] [--threshold PCT]

Standard library only.
"""
import argparse
import csv
import glob
import os
import statistics
import subprocess
import sys


def invoke(binary, csv_path, pad, extra):
    env = dict(os.environ)
    # Vary the environment size per invocation to vary the initial stack layout.
    env["JSON_BENCH_PAD"] = "x" * pad
    cmd = [binary, "--csv", csv_path] + extra
    print("+", " ".join(cmd), f"(env pad {pad})", file=sys.stderr, flush=True)
    r = subprocess.run(cmd, env=env, stdout=subprocess.DEVNULL)
    if r.returncode != 0:
        sys.exit(f"{binary} exited with {r.returncode}")


def cmd_run(a):
    os.makedirs(a.out, exist_ok=True)
    for i in range(a.runs):
        invoke(a.bin, os.path.join(a.out, f"run-{i:02d}.csv"), 64 * i + 13 * (i % 7), a.args)


def cmd_ab(a):
    for side in ("base", "cand"):
        os.makedirs(os.path.join(a.out, side), exist_ok=True)
    bins = {"base": a.base, "cand": a.cand}
    n = {"base": 0, "cand": 0}
    for r in range(a.rounds):
        # A B B A, then B A A B, so neither side always goes first.
        order = ["base", "cand", "cand", "base"] if r % 2 == 0 else ["cand", "base", "base", "cand"]
        for side in order:
            i = n[side]
            n[side] += 1
            # Both sides see the same sequence of environment sizes.
            invoke(bins[side], os.path.join(a.out, side, f"run-{i:02d}.csv"), 64 * i + 13 * (i % 7), a.args)


def load(path, metric):
    """Return {(group, case, impl): [per-invocation median of `metric`]}."""
    files = sorted(glob.glob(os.path.join(path, "*.csv"))) if os.path.isdir(path) else [path]
    if not files:
        sys.exit(f"no CSV files in {path}")
    out = {}
    for f in files:
        per = {}
        with open(f, newline="") as fh:
            rd = csv.DictReader(fh)
            if metric not in (rd.fieldnames or []):
                sys.exit(f"{f}: no column {metric!r}; columns are {rd.fieldnames}")
            for row in rd:
                v = row[metric]
                if v == "":
                    continue  # metric not available for this sample (e.g. counters off)
                per.setdefault((row["group"], row["case"], row["impl"]), []).append(float(v))
        for k, vals in per.items():
            out.setdefault(k, []).append(statistics.median(vals))
    return out, len(files)


def spread(xs):
    """Relative range of per-invocation medians: how much one side disagrees with itself."""
    m = statistics.median(xs)
    return (max(xs) - min(xs)) / m if m and len(xs) > 1 else float("nan")


def cmd_compare(a):
    base, nb = load(a.base, a.metric)
    cand, nc = load(a.cand, a.metric)
    print(f"metric: {a.metric}; invocations: base {nb}, cand {nc}; threshold {a.threshold}%")
    if min(nb, nc) < 3:
        print("WARNING: fewer than 3 invocations per side; run-to-run noise cannot be judged, verdicts are unreliable.")
    print(f"{'group':<12} {'case':<40} {'impl':<8} {'base':>12} {'cand':>12} {'change':>8} {'noise':>7}  verdict")
    worse = 0
    for k in sorted(base.keys() | cand.keys()):
        if k not in base or k not in cand:
            print(f"{k[0]:<12} {k[1]:<40} {k[2]:<8} {'only in ' + ('base' if k in base else 'cand'):>34}")
            continue
        b, c = statistics.median(base[k]), statistics.median(cand[k])
        change = (c - b) / b * 100.0
        noise = max(spread(base[k]), spread(cand[k])) * 100.0
        if noise != noise:  # NaN: a single invocation on a side
            verdict = "unjudged (1 invocation)"
        elif abs(change) <= noise or abs(change) < a.threshold:
            verdict = "within noise"
        elif change < 0:
            verdict = "FASTER" if a.metric == "ns_per_op" else "lower"
        else:
            verdict = "SLOWER" if a.metric == "ns_per_op" else "higher"
            worse += 1
        print(f"{k[0]:<12} {k[1]:<40} {k[2]:<8} {b:>12.2f} {c:>12.2f} {change:>+7.2f}% {noise:>6.2f}%  {verdict}")
    return 1 if (worse and a.fail_on_regression) else 0


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
    c.add_argument("--fail-on-regression", action="store_true")
    c.set_defaults(fn=cmd_compare)
    a = p.parse_args(argv)
    a.args = extra
    sys.exit(a.fn(a) or 0)


if __name__ == "__main__":
    main()
