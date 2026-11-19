# json-agent-workspace

Agent workspace for optimising the data structure underneath
[RedisJSON](https://github.com/RedisJSON/RedisJSON): [`ijson`](https://github.com/RedisJSON/ijson)
and the dependencies it builds on.

**Status: early.** A first microbenchmark slice exists in [`bench/`](bench/README.md) (parse, serialise, memory). It is not frozen, and no baseline or experiment exists yet.

## Layout

| Path | What |
|---|---|
| `ijson/` | The data structure, pinned to the revision RedisJSON builds (`379dadf`). Primary target. |
| `ciborium/` | RedisJSON's CBOR fork, a dependency of `ijson`. |
| `RedisJSON/` | The consumer, pinned at `9ca1c88`. Reference only: it defines which operations matter. |
| `bench/` | Microbenchmark harness for `ijson`, with `serde_json::Value` as the baseline. |
| `docs/USAGE-PATTERN.md` | How RedisJSON calls `ijson`, and the call paths to benchmark. |
| `docs/DEPENDENCIES.md` | Which dependencies are in scope, and which RedisJSON-org repos are not. |
| `docs/PERFORMANCE-MCP.md`, `docs/TMA-REPORTING.md` | Required per-session and per-experiment rules. |
| `experiments/`, `EXPERIMENTS.md` | The experiment ledger. |
| `AGENTS.md`, `.claude/` | Agent instructions. |

## Getting started

```bash
git clone --recurse-submodules https://github.com/redis-performance/json-agent-workspace.git
cd json-agent-workspace/ijson && cargo test --lib -- --test-threads=1
```

## Next steps

1. Profile RedisJSON command workloads to weight the call paths in `docs/USAGE-PATTERN.md`.
2. Finish and freeze the `ijson` microbenchmark suite in `bench/` (remaining groups and hardware counters are listed in its README).
3. Record a baseline (time and memory) per architecture, then start experiments.
