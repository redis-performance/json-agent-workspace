# Agent Notes — json-agent-workspace

Shared instructions for every coding agent (Claude Code, Codex, others). Codex reads this
file natively and `.claude/CLAUDE.md` imports it.

> **PUBLIC REPO.** No secrets, credentials, tokens, private IPs, internal hostnames,
> customer names, Slack, or ticket references. Describe machines generically (arch + microarch only).

Start every session by reading this file, `.workspace-memory/MEMORY.md`,
`.workspace-memory/LIVE-STATUS.md` and `.workspace-memory/handoff.md`.

---

## Scope

Optimise the data structure underneath RedisJSON: **`ijson`** (`IValue`, `IObject`,
`IArray`, `IString`, `INumber`, its JSON and CBOR codecs) and the dependencies it builds on.

| Path | Role | Mutable? |
|---|---|---|
| `ijson/` | The data structure. Primary target. | Yes |
| `ciborium/` | RedisJSON's CBOR fork, used by `ijson`'s CBOR codec. | Only when a profile attributes cost to it |
| `RedisJSON/` | The consumer. Read it to learn which `ijson` operations matter and how they are called. | **No, reference only** |

RedisJSON is here to answer "what matters", not to be changed. Do not add Redis server,
module command, persistence-format or client work to this campaign. A change that needs a
RedisJSON edit to pay off is written up as a proposal, not implemented here.

crates.io dependencies (`hashbrown`, `dashmap`, `indexmap`, `smallvec`, `serde_json`, `zstd`,
`half`) are not vendored. Promote one to a submodule only after a profile attributes
measurable cost to it; see [docs/DEPENDENCIES.md](docs/DEPENDENCIES.md).

Every hypothesis must name the RedisJSON call path it serves, taken from
[docs/USAGE-PATTERN.md](docs/USAGE-PATTERN.md). An `ijson` speedup on an operation RedisJSON
never performs is out of scope.

---

## Mandatory performance MCP use — every session

Every investigation or optimization work session, whether run by Codex or Claude,
must use the `cpu-perf` performance MCP from
[usamahz/cpu-performance-engineering](https://github.com/usamahz/cpu-performance-engineering).
This is a required part of the investigation, not an optional reference.

At session start, discover the server/tools and check `library_status`; before
proposing changes or interpreting performance, call `ask` with the actual question
and relevant measured counters, assembly or compiler output. Use its source lookup
and evidence-checking tools as needed. Record the calls, source/version information,
findings, limitations and resulting decisions in the experiment record.
Follow [docs/PERFORMANCE-MCP.md](docs/PERFORMANCE-MCP.md) for the shared workflow.
If unavailable, record the attempted connection and concrete blocker, use the linked
primary sources as a disclosed fallback, and restore MCP use when possible. Never
claim a consultation happened when it did not. Keep indexing and retrieval off the
benchmark runner; the immutable referee and existing acceptance gates remain.

---

## Required TMA classification — every experiment

Codex and Claude must use [topdown-profiler](https://github.com/redis-performance/topdown-profiler)
to assess further experiments. Every experiment's Markdown report must contain a
**TMA classification**: Frontend_Bound, Bad_Speculation, Backend_Bound and Retiring,
with their measured shares of total pipeline slots, plus measured child categories.
The four L1 categories sum to 100% within reported rounding/measurement error.
Show baseline and candidate separately; link raw captures, tool revision, binary,
CPU/collector scope, workload, run IDs and collection quality. Follow
[docs/TMA-REPORTING.md](docs/TMA-REPORTING.md). Never substitute hotspot sample
shares, IPC, instruction counts, another machine, or illustrative numbers for TMA.
If unsupported or uncollected, show all four as unavailable and state the blocker.
Reports written before this rule are not rewritten.

---

## Measurement and source rules

- **No benchmark referee exists yet.** `ijson` ships no `benches/`. The first deliverable is a
  microbenchmark suite that mirrors the RedisJSON call paths in `docs/USAGE-PATTERN.md`. Until
  it is reviewed and frozen, no optimisation is accepted.
- Once frozen, the benchmark suite and `ijson`'s tests and fuzz targets are the immutable
  referee during a candidate comparison. Harness fixes are separate commits followed by a
  new baseline. Never weaken a test to let a candidate pass.
- One falsifiable hypothesis per experiment. Baseline and candidate share the exact source
  commit, toolchain (`ijson/rust-toolchain.toml`), flags, allocator and machine.
- Build `ijson` with the features RedisJSON uses (`default-features = false`; `thread_safe`
  only when measuring that configuration) and say which.
- Run `ijson`'s unit tests single-threaded: `cargo test --lib -- --test-threads=1`. Run in
  parallel they crash or fail in both the default and `thread_safe` builds (observed 2026-11-19
  at `379dadf`; not yet root-caused, the tests share the global string cache).
- Report memory as well as time. `ijson` exists to save memory; a speedup that grows the
  per-value or per-document footprint is a trade-off to declare before running, not after.
- Preserve behaviour RedisJSON depends on: number formatting, typed-array tagging and
  retagging, string interning semantics, CBOR stream compatibility, defrag.
- Never manufacture results, and never carry timings across machines.

## Logging and publishing

Record every experiment under `experiments/` and in `EXPERIMENTS.md`, including rejected ones.
Keep losing variants as patches. Upstream PRs, issues and comments on `RedisJSON/ijson`,
`RedisJSON/ciborium` or `RedisJSON/RedisJSON` need the maintainer's direction first.
Never force-push. Never change agent permission settings to bypass a restriction.
