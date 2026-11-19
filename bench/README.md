# bench — microbenchmarks for ijson

Measures `ijson` on the call paths RedisJSON uses ([docs/USAGE-PATTERN.md](../docs/USAGE-PATTERN.md)),
with `serde_json::Value` as the baseline in the same binary.

**Status: first slice, not frozen.** Parse and serialise only. It is not yet the referee
that `AGENTS.md` requires; see "Not built yet".

## Run

```bash
cd bench
cargo build --release
./target/release/time --csv ../scratch/time.csv   # timing, a few minutes
./target/release/time --quick                     # smoke test only, never for reporting
./target/release/time --pin 2 --csv ../scratch/time.csv   # pinned to CPU 2
./target/release/mem --csv ../scratch/mem.csv     # heap cost of parse and clone
```

Both binaries take `--filter SUBSTR` to select fixtures, `--fixtures DIR` to point elsewhere and
`--csv PATH`. `time` also takes `--group NAME`, `--case SUBSTR`, `--list`, `--pin CPU`
(`sched_setaffinity` for the benchmark thread, before fixtures are loaded) and `--no-counters`.

## What is measured

| Group | ijson path | Mirrors |
|---|---|---|
| `parse` | `IValueDeserSeed::new(None).deserialize_compact_objects` over a `serde_json::Deserializer`, recursion limit on | `JSON.SET`: `redis_json/src/ivalue_manager.rs`, `from_str` with default arguments |
| `serialize` | `serde_json::to_string(&IValue)` | `JSON.GET` default format: `redis_json/src/key_value.rs`, `serialize_object` |
| `mem` | allocator calls, peak bytes and retained bytes for parse and for clone | the memory side of both |

Fixtures are RedisJSON's own datasets, read from `RedisJSON/tests/benchmarks/datasets` in the
pinned submodule. Before anything is timed, every fixture is parsed by both implementations,
and the `ijson` value is serialised and read back; a mismatch aborts the run.

## How timing works

The loop follows Daniel Lemire's [`rustquadsimd`](https://github.com/lemire/rustquadsimd)
harness: warm up until 0.4 s has passed, growing the repeat count tenfold, then time that many
samples. Three deliberate differences:

- **Every sample is written to the CSV**, one row each, so noise can be judged. The table
  prints the minimum and the median, plus two dispersion figures per row: `mad%` (median
  absolute deviation over the median) and `p10-90%` (p90 minus p10 over the median).
- **Short operations are batched** so one timed sample lasts at least 50 µs; the `inner`
  column records the batch size.
- **Results are kept alive until the clock stops**, so dropping the value is not part of
  `parse` or `serialize`. Drop gets its own group later.

## Hardware counters (Linux)

Each timed sample also reads instructions, cycles, branches, branch misses, cache misses and
cache references for the benchmark thread, user space only (`exclude_kernel`, `exclude_hv`),
through `perf_event_open` (`src/counters.rs`, a direct `libc` wrapper).

- The events form one group per PMU, so they are read atomically. The group is enabled just
  before the clock starts and disabled just after it stops: setup, drop and the read itself
  are outside. There is no counter syscall inside the per-call loop.
- If the kernel multiplexed the group, values are scaled by `time_enabled / time_running`
  and the sample's `scaled` CSV column is 1.
- Table columns `ins/op`, `cyc/op`, `brmiss/op`, `cmiss/op` are medians over the counted
  samples; `IPC` is the ratio of the first two. CSV columns: `instructions, cycles, branches,
  branch_misses, cache_misses, cache_refs` (per operation), `pmu`, `scaled`.
- **Degrading.** If counters cannot be opened (not Linux, `perf_event_paranoid` too high,
  no PMU in a VM), or open but fail a start-up self-test, the run continues on wall-clock
  time and prints one `counters: OFF, ...` line on stderr with the reason. Absent values are
  empty in the CSV and `-` in the table; nothing is zero-filled. A single event that cannot
  join the group is absent on its own and named in the status line.
- **Hybrid CPUs** (Intel `cpu_core` + `cpu_atom`). Each core type has its own PMU and an
  event counts on one of them only. The harness opens one group on each PMU (the kernel's
  extended hardware-event encoding, PMU type in the high half of `config`) and attributes a
  sample to the one that ran; the `pmu` column says which. If the thread changed core type
  inside a sample the two parts are added and `pmu` is `mixed`. Instructions per operation
  are comparable across core types; cycles, IPC and miss counts are not, so pin the run and
  never mix `pmu` values in one comparison. Checked on a hybrid machine with the thread
  pinned to a P core and to an E core, and unpinned: both PMUs count. The non-Linux and
  counters-refused paths have not been exercised on a real machine.
- **Harness overhead.** At start-up an empty routine is run through the same sampling
  function, one call per sample. Its time and instruction count are printed on stderr and
  written to the metadata. That is the fixed cost of one sample's bracketing; a row carries
  it divided by its `inner`. It is reported, never subtracted.

## Run metadata and hygiene

With `--csv PATH`, both binaries write `PATH.meta.json` (`src/meta.rs`), and print a one-line
summary on stderr: UTC timestamp; rustc version, target, profile, opt level (captured at build
time by `build.rs`); the `ijson` dependency line from `Cargo.toml`; allocator; git revision
and dirty flag of the workspace (dirtiness judged on `bench/`), `ijson` and `RedisJSON`
(tracked files); harness parameters; CPU model, logical and physical core counts, P/E CPU
lists, the CPU the thread is on and its core type, its affinity mask, governor, current/min/max
frequency, turbo and SMT state; kernel version and `perf_event_paranoid`; counter status;
harness overhead. Unreadable values are `null`. No host name, user name, address or path.

`time` prints a `WARNING` on stderr, and keeps going, when the thread is not pinned to one
CPU, the governor is not `performance`, turbo is on, or SMT is active (or any of these cannot
be read). A run with warnings is not reportable.

## Read the numbers with these in mind

- **Warm string cache.** `ijson` interns strings in a process-wide cache. Parsing the same
  document repeatedly measures parsing with those strings already interned. A cold-cache
  variant is not built yet.
- **System allocator.** RedisJSON runs on Redis's allocator; these binaries use the system
  one. Absolute numbers and allocation-heavy comparisons will differ in production.
- **Same build as RedisJSON**: its `serde_json` version constraint, `ijson` without default
  features and without `thread_safe`, its release profile (no LTO), the toolchain in
  `rust-toolchain.toml`.
- **`mem` retained bytes for `ijson` include newly interned strings**, and `clone` shares them.
- Run on a quiet, pinned core of a dedicated machine before quoting anything, and publish the
  `.meta.json` with the result. SMT is reported system-wide; whether the pinned core's sibling
  is idle is not checked.

## Not built yet

1. Groups for lookup and traversal (hot and cold), in-place mutation, typed arrays,
   clone/drop timing, CBOR stream, defrag.
2. Seeded synthetic documents for sizes and shapes the datasets do not cover.
3. A cold string-cache variant of `parse`.
