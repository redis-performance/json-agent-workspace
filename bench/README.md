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
./target/release/mem                              # heap cost of parse and clone
```

Both binaries take `--filter SUBSTR` to select fixtures and `--fixtures DIR` to point elsewhere.

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
  prints the minimum and the median.
- **Short operations are batched** so one timed sample lasts at least 50 µs; the `inner`
  column records the batch size.
- **Results are kept alive until the clock stops**, so dropping the value is not part of
  `parse` or `serialize`. Drop gets its own group later.

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
- Wall-clock only. Run on a quiet, pinned core of a dedicated machine before quoting anything,
  and state the machine, core, frequency and SMT state with the result.

## Not built yet

1. Hardware counters per operation (instructions, cycles, branch misses, cache misses).
2. Groups for lookup and traversal (hot and cold), in-place mutation, typed arrays,
   clone/drop timing, CBOR stream, defrag.
3. Seeded synthetic documents for sizes and shapes the datasets do not cover.
4. A cold string-cache variant of `parse`.
5. Run metadata (machine, toolchain, revisions) written next to the CSV.
