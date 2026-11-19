# Workspace memory

Durable facts verified when written. Re-check before relying on them.

- 2026-11-19: Workspace scaffolded. `ijson` pinned at `379dadf` (the rev RedisJSON `9ca1c88` builds). No benchmark referee exists; `ijson` has no `benches/`.
- 2026-11-19: `RedisJSON/jsonpath`, `RedisJSON/jsonpath_rs` and `RedisJSON/bson-rust` are not in the dependency graph (see docs/DEPENDENCIES.md).
- 2026-11-19: `ijson` lib tests pass single-threaded (default: 134 passed; `thread_safe`: 129 passed) but SIGSEGV (default) or fail 7 tests (`thread_safe`) when run in parallel. Not root-caused.
