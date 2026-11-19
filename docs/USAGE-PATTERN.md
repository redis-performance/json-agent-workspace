# How RedisJSON uses ijson

This is the map from RedisJSON to the `ijson` operations worth optimising. It is a **static
reading of the source**, not a runtime profile: the counts below are matching source lines in
`RedisJSON/redis_json/src` and `RedisJSON/json_path/src` (test files excluded) at RedisJSON
`9ca1c88` against `ijson` `379dadf`. They show where `ijson` is touched, not how often each
path runs. Runtime weights are an open task (see the end).

## Where ijson is touched

| ijson surface | Matching lines | Main RedisJSON files |
|---|---|---|
| Type dispatch: `destructure`, `DestructuredRef`, `ValueType`, `type_()` | 174 | `commands.rs`, `ivalue_manager.rs`, `json_path/src/json_node.rs` |
| `.clone()` (not all are `IValue` clones) | 128 | `commands.rs`, `json_path/src/json_path.rs` |
| Container mutation: `insert`, `push`, `remove`, `pop`, `truncate`, `drain`, `extend` | 98 | `ivalue_manager.rs`, `auto_create.rs` |
| Strings: `IString`, `as_string`, interning | 58 | `ivalue_manager.rs`, `key_value.rs`, `rdb_persistence.rs` |
| Parse: `from_str`, `Deserializer`, `deserialize` | 54 | `manager.rs`, `ivalue_manager.rs`, `commands.rs` |
| Serialise: `serialize`, `Serializer` | 49 | `formatter.rs`, `key_value.rs` |
| Lookup: `.get(`, `.get_mut(`, `get_key_value` | 46 | `ivalue_manager.rs`, `json_path/src/select_value.rs` |
| Typed arrays: `ArraySliceRef`, `ArraySliceMut`, `ArrayTag`, `TryExtend`, `FloatType` | 44 | `ivalue_manager.rs`, `commands.rs` |
| Numbers: `INumber`, `as_number`, `to_f64`, `to_i64`, `has_decimal_point` | 40 | `number.rs`, `ivalue_manager.rs` |
| CBOR stream: `StreamEncoder`, `StreamDecoder`, dictionaries | 36 | `rdb_persistence.rs` |
| Arrays: `IArray`, `as_array`, `as_array_mut` | 21 | `ivalue_manager.rs`, `array_index.rs` |
| Objects: `IObject`, `as_object`, `as_object_mut` | 14 | `ivalue_manager.rs` |
| Defrag: `Defrag`, `DefragAllocator` | 12 | `defrag.rs` |

## Call paths to benchmark

Each microbenchmark should correspond to one of these, with RedisJSON's argument shapes:

1. **Parse a document into an `IValue`** (write commands). Goes through `serde_json` into
   `ijson`'s deserialiser (`ijson/src/de.rs`), including string interning and typed-array
   detection.
2. **Serialise an `IValue`** (read commands), through `redis_json/src/formatter.rs` and
   `ijson/src/ser.rs`, with and without indentation.
3. **Path traversal**: object key lookup and array indexing as driven by `json_path` through
   `select_value.rs` and `json_node.rs`.
4. **In-place mutation**: object insert/remove, array push/insert/pop/trim, numeric
   increment, string append, as done in `ivalue_manager.rs`.
5. **Typed numeric arrays**: building, extending, retagging on a non-numeric insert.
6. **Clone and drop** of values and subtrees.
7. **CBOR stream encode/decode** used for persistence (`rdb_persistence.rs`), including zstd.
8. **Memory accounting and defrag**.

## Open: runtime weights

Nothing here says which path dominates under load. Before ranking hypotheses, profile real
RedisJSON command workloads (RedisJSON's own benchmark workflows are the starting point) and
record, per command, the share of time spent in each `ijson` function. Replace the "matching
lines" column with those measurements when they exist.
