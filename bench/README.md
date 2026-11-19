# bench — microbenchmarks for ijson

Measures `ijson` on the call paths RedisJSON uses ([docs/USAGE-PATTERN.md](../docs/USAGE-PATTERN.md)),
with `serde_json::Value` as the baseline in the same binary.

**Status: complete enough to use, not frozen.** Every call path has a group, but the suite has
only been smoke-run on a noisy laptop, and the limits under "Known limits" are open. Do not
accept or reject an optimisation on it until those marked *blocking* are closed and a baseline
exists on a dedicated machine.

## Run

```bash
cd bench
cargo build --release
cargo test --release -- --test-threads=1             # generator tests; ijson's string cache is not thread-safe

./target/release/time --list                         # groups
./target/release/time --quick --pin 2                # smoke test only, never for reporting
./target/release/mem                                 # heap cost of parse and clone

# A reportable measurement: several invocations, one fixed plan.
scripts/bench.py run --bin ./target/release/time --out ../scratch/base --runs 6 -- --pin 2

# Comparing two builds of ijson: interleaved A B B A, then a verdict per row.
scripts/bench.py ab --base PATH/TO/base/time --cand PATH/TO/cand/time --out ../scratch/ab --rounds 3 -- --pin 2
scripts/bench.py compare ../scratch/ab/base ../scratch/ab/cand
scripts/bench.py compare ../scratch/ab/base ../scratch/ab/cand --metric instructions
```

`time` flags: `--group NAME` (repeatable), `--filter SUBSTR` (datasets), `--case SUBSTR` (rows),
`--pin CPU`, `--no-counters`, `--csv PATH`, `--plan-out PATH` / `--plan-in PATH`, `--fixtures DIR`,
`--quick`.

## Groups

| Group | What it measures | RedisJSON path it mirrors |
|---|---|---|
| `parse` | text to value; datasets, synthetic documents, and warm / keys-warm / cold string-cache variants | `JSON.SET`: `ivalue_manager.rs` `from_str` |
| `serialize` | value to text, default format | `JSON.GET`: `key_value.rs` `serialize_object` |
| `lookup` | object key hit/miss, array index, deep path, iteration, scalar reads; hot (one document) and cold (working set larger than cache) | `json_path/src/json_node.rs`, `impl SelectValue for IValue` |
| `mutate` | object insert/overwrite/remove, array push/insert/pop/trim, number increment, string append, nested set, build by insertion | `ivalue_manager.rs` write paths |
| `typed_array` | homogeneous numeric arrays per width: parse, read, sum, push, extend, widening, retagging | `ivalue_manager.rs`, `commands.rs` (FPHA) |
| `clone_drop` | clone and drop of documents, subtrees and scalars | `ivalue_manager.rs` `clone_value`, `commands.rs` |
| `cbor` | plain, self-contained and stream encode/decode, against the JSON text round trip | `rdb_persistence.rs` |

Each group's module doc names the exact call sites. Datasets are RedisJSON's own, read from
`RedisJSON/tests/benchmarks/datasets` in the pinned submodule; synthetic documents come from the
seeded generator in `src/synth.rs`. Every case is checked against the baseline (or round-tripped)
before it is timed, and a mismatch aborts the run.

## How a row is measured

The loop follows Daniel Lemire's [`rustquadsimd`](https://github.com/lemire/rustquadsimd) harness:
warm up, then time N samples. What differs:

- **Every sample is kept** and written to the CSV. The table prints min, median, MAD and the
  p10-p90 spread. `compare` uses the per-invocation median; the minimum is for reading, not deciding.
- **Short operations are batched.** The batch (`inner`) is found by doubling until one measured
  sample reaches 50 µs.
- **Batch size and sample count are a saved plan.** Both change what a row measures (the share
  of a batch that sees cold state, how many results are alive, how much bracketing cost each
  operation carries). The first invocation calibrates and writes the plan; later ones replay it.
  `compare` refuses rows whose batch size differs between the sides.
- **Results are kept alive until the clock stops**, so dropping them is not timed. Operations
  that change their input get a fresh state per call, built outside the timed region.
- **Each row has a wall-clock budget** (10 s; warm-up stops growing beyond it), so rows with
  slow setup cannot run for minutes.
- **Hardware counters** (Linux): instructions, cycles, branches, branch misses, cache misses and
  references per operation, user space only, for the benchmark thread, bracketing exactly the
  timed region. If they cannot be opened the run continues on wall-clock and says so once; missing
  values are empty, never zero. On hybrid CPUs each sample is attributed to the core type it ran
  on (`pmu` column); do not mix core types in a comparison.
- **Harness overhead** (an empty routine through the same path) is printed and recorded, not subtracted.

## What is recorded

Next to each CSV, `<csv>.meta.json`: time, toolchain, target, profile, the `ijson` revision and
diff hash the binary was **built** from, lockfile hash, workspace revisions, harness parameters,
CPU model and topology, the pinned CPU, governor, frequency, turbo and SMT state, kernel,
`perf_event_paranoid`, counter availability and harness overhead. Unknown values are null. No
hostname, user name or path is written. The binary and its stderr do contain absolute paths, so
publish CSVs and sidecars only.

The run warns when it is not pinned, the governor is not `performance`, turbo is on or SMT is
active. Any of those makes timings non-reportable. Instruction counts are far more stable than
time and are the better first signal on a shared machine.

## How `compare` decides

Per row it takes the median of each invocation, then runs a permutation test on those medians.
It reports `LOWER` or `HIGHER` only if the test rejects "no difference" at `--alpha` (0.05) **and**
the change is at least `--threshold` (1%). It needs four or more invocations per side, refuses to
compare runs whose parameters or pinning differ, and prints how far the baseline (`serde`) rows
moved: they cannot change when only `ijson` changes, so that figure is the drift floor. Rows are
tested one by one; with hundreds of rows a few will cross the line by chance.

## Known limits

Blocking before this can referee a decision:

1. **Not run on a dedicated machine.** No baseline exists. A full invocation has about 770 rows
   and is expected to take tens of minutes; that has not been measured.
2. **System allocator.** RedisJSON runs on Redis's allocator. `ijson`'s design trades allocation
   count and size, which is exactly where allocators differ, so results on allocation-heavy rows
   (parse, clone, drop, CBOR encode, mutation) need confirming under jemalloc. Not built.
3. **The string-intern table is small and hot.** In Redis it holds strings for the whole keyspace,
   so an intern or a last-owner drop is likely a cache miss; here it holds one document. A change
   that adds or removes intern lookups would be mis-scored. A pre-populated-cache variant is not built.
4. **`ijson` always runs before the baseline** in a process, so the baseline inherits its heap layout.
5. **Transitive dependency versions differ from RedisJSON's lockfile** (`itoa`, `memchr`, `serde`,
   `smallvec`, `indexmap`, `zstd`), and `ijson`'s `ciborium` dependency tracks a branch. The
   `serde_json` version, `ijson` revision, toolchain and release profile do match.

Other limits:

- Wall time includes kernel time; counters do not. Page faults and system time per row are not recorded.
- For small documents several parses share a batch, so only the first sees an empty string cache;
  large documents (one per sample) always do. The `-warmstr` / `-keywarm-coldstr` / `-coldstr`
  synthetic rows control this explicitly; the dataset rows do not.
- A last-owner drop cannot be isolated for small documents, for the same reason.
- Cold lookup sets hold the same number of documents for both implementations, not the same bytes.
- Verification rejects floats with large exponents, because `serde_json` as RedisJSON builds it
  (no `float_roundtrip`) can be one unit in the last place off. Synthetic documents avoid them.
- `mem` charges lazily initialised state to whichever fixture triggers it, and counts requested
  bytes, not allocator size classes.
- Rows where setup costs far more than the operation (large-array mutation) are slow and have few samples.
- Not covered: `JSON.GET` with a `$` path (serialises a wrapper), `JSON.MERGE`, toggle/multiply/power,
  F16/BF16 and unsigned typed arrays, CBOR dictionary reset and budget exhaustion, defrag.
- Metadata does not record command-line arguments, `RUSTFLAGS`, libc version or THP setting, and
  its `lto` and `allocator` fields are fixed strings.
