//! The operations under test, written the way RedisJSON calls them.

use ijson::{IValue, IValueDeserSeed};

/// `JSON.SET` parse path: `redis_json/src/ivalue_manager.rs`, `from_str` with
/// the default arguments (`limit_depth = true`, no forced float type).
#[inline(never)]
pub fn ijson_parse(text: &str) -> IValue {
    let mut de = serde_json::Deserializer::from_str(text);
    let v = IValueDeserSeed::new(None)
        .deserialize_compact_objects(&mut de)
        .expect("fixture parses");
    de.end().expect("no trailing data");
    v
}

/// Baseline: the same text into `serde_json::Value`.
#[inline(never)]
pub fn serde_parse(text: &str) -> serde_json::Value {
    let mut de = serde_json::Deserializer::from_str(text);
    let v = <serde_json::Value as serde::Deserialize>::deserialize(&mut de).expect("fixture parses");
    de.end().expect("no trailing data");
    v
}

/// `JSON.GET` default-format path: `redis_json/src/key_value.rs`,
/// `serialize_object` without indentation is `serde_json::to_string`.
#[inline(never)]
pub fn ijson_serialize(v: &IValue) -> String {
    serde_json::to_string(v).expect("serialises")
}

#[inline(never)]
pub fn serde_serialize(v: &serde_json::Value) -> String {
    serde_json::to_string(v).expect("serialises")
}
