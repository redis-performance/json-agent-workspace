# Handoff

Next: (1) run `bench/` on a dedicated, pinned machine and record a baseline; (2) close the blocking limits in bench/README.md (jemalloc build, pre-populated intern cache, order alternation, lockfile parity); (3) profile RedisJSON command workloads to weight docs/USAGE-PATTERN.md; (4) freeze the suite.

Unverified leads from the first smoke runs (noisy laptop, quick mode; confirm before acting): `IObject::get(&str)` interns the key on every lookup; reading typed f64 arrays element-wise builds an owned value per element; retagging a typed array to heterogeneous converts per element; new-key insert into `IObject` is slow next to the baseline; CBOR decode is slower than JSON parse of the same document; stream decode is slower per document than plain decode.
