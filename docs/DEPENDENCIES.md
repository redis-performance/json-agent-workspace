# Dependencies in scope

Only crates that are compiled into RedisJSON are candidates. Checked against RedisJSON's
`Cargo.lock` at `9ca1c88` and `ijson`'s `Cargo.toml` at `379dadf`.

| Crate | Source | Where ijson uses it | Status here |
|---|---|---|---|
| `ijson` | `RedisJSON/ijson` (fork of `Diggsey/ijson`, 77 commits at the pinned rev) | n/a | Submodule, primary target |
| `ciborium`, `ciborium-ll` | `RedisJSON/ciborium` (fork of `enarx/ciborium`) | `cbor.rs`, `cbor/stream.rs` | Submodule, change only on profile evidence |
| `dashmap` 5.x (`raw-api`) | crates.io | `string.rs` (string interning) | Not vendored |
| `hashbrown` 0.13 | crates.io | `de.rs`, `cbor/stream.rs`, `unsafe_string.rs` | Not vendored |
| `indexmap` 2.13 | crates.io | `object.rs`, `de.rs` | Not vendored |
| `smallvec` | crates.io | `de.rs` | Not vendored |
| `serde_json` (RedisJSON pins `<1.0.147`) | crates.io | `ser.rs`, `de.rs`, `cbor.rs`, `value.rs`, `array.rs` | Not vendored; the pin is deliberate |
| `half` | crates.io | number, array, value and codec files | Not vendored |
| `zstd` | crates.io | `cbor.rs` | Not vendored |

Promote a crates.io dependency to a submodule only when a profile of a `USAGE-PATTERN.md` call
path attributes measurable cost to it. Record the profile in the experiment that promotes it.

## Deliberately excluded

These RedisJSON-org repos are **not** in RedisJSON's or `ijson`'s dependency graph, so
optimising them changes nothing in RedisJSON:

- `RedisJSON/jsonpath` (fork of `freestrings/jsonpath`, last pushed 2023-03)
- `RedisJSON/jsonpath_rs` (last pushed 2022-09). JSONPath now lives in-tree as the `json_path`
  crate inside `RedisJSON/RedisJSON`.
- `RedisJSON/bson-rust` (fork of `mongodb/bson-rust`, last pushed 2022-05). RedisJSON takes
  `bson` from crates.io.

`json_path`, `pest` and `regex` belong to RedisJSON's path engine, not to the data structure,
and are out of scope for this campaign.
