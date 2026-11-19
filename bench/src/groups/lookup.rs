//! Read-side access, driven the way RedisJSON's JSONPath engine drives `ijson`.
//!
//! Every case runs for `ijson` (`IValue`) and for the `serde` baseline
//! (`serde_json::Value`, same logical operation). Inputs are built through
//! `ijson_parse` / `serde_parse`, so arrays have the representation the
//! `JSON.SET` path produces (homogeneous numeric arrays become typed arrays).
//!
//! Sub-groups and the RedisJSON call sites they mirror
//! (`J` = `RedisJSON/json_path/src/json_node.rs`, `impl SelectValue for IValue`):
//!
//! * `obj-{N}keys-{hit|miss|miss-interned}-{hot|cold}`: `J::get_key`,
//!   `self.as_object().and_then(|o| o.get(key))` with `key: &str`. The same
//!   `IObject::get(&str)` shape is used by `redis_json/src/ivalue_manager.rs`
//!   (`can_merge`: `map.get(key.as_str())`; `follow_path` uses the `get_mut`
//!   twin). `miss` uses keys that exist nowhere in the process; `miss-interned`
//!   uses keys that are alive in ijson's string cache but absent from the object.
//! * `arr-{hetero|ints|floats}-len{N}-index-{hot|cold}`: `J::get_index`,
//!   `arr.as_slice()` matched on `ArraySliceRef`, borrowing from heterogeneous
//!   arrays and building an owned `IValue::from(elem)` for typed arrays. The
//!   element is then read as `J::get_type` + `J::get_double` would.
//! * `path-depth{D}-{hot|cold}`: a static path `$.node_0. ... .node_{D-1}.items[3].e`
//!   resolved as a chain of `J::get_key` / `J::get_index` steps, the sequence
//!   `json_path`'s static-path evaluation issues for `KeyValue::get_first` /
//!   `get_values` in `redis_json/src/key_value.rs`, ending in `J::get_long`.
//!   The `json_path` crate itself is not used.
//! * `iter-obj-{N}keys-{values|keys}-*`, `iter-arr-{kind}-len{N}-*`: `J::values`
//!   (`destructure_ref`, then `IObject::values()` or `IArray::iter()`) and
//!   `J::keys` (`o.keys().map(IString::as_str)`), as consumed by
//!   `key_value.rs` `resp_serialize_inner` / `value_to_resp3` and
//!   `arr_first_index_single`. The `Box<dyn Iterator>` that `SelectValue` wraps
//!   around these is left out: it is not part of the data structure.
//! * `scalar-type-*`: `J::get_type` (`type_()`, plus the Long/Double split via
//!   `as_number`, `has_decimal_point`, `to_i64`), used by `key_value.rs`
//!   `value_name` / `get_type`. `scalar-destructure-*`: `destructure_ref()`
//!   match with a payload read (`J::len`, `J::values`).
//!   `scalar-str-*`: `J::as_str` (`as_string().map(IString::as_str)`), as in
//!   `key_value.rs` `str_len`. `scalar-i64-*`: `J::get_long`. `scalar-f64-*`:
//!   `J::get_double` (`to_f64_lossy`). `scalar-bool-*`: `J::get_bool` (`to_bool`).
//!
//! Hot and cold: `-hot` reuses one small document. `-cold` uses a working set
//! of many distinct documents (sized by an estimate of ijson's heap bytes, see
//! `cold_bytes`; shrunk under `--quick`, where it is NOT larger than cache and
//! the numbers mean nothing) visited in a seeded random order, so each
//! operation starts on memory that is not resident in cache. Both
//! implementations get the same number of documents and the same visit order;
//! their byte sizes differ.
//!
//! Verification: every document set is compared between the two
//! implementations when built (all documents of small sets, an even sample of
//! large ones), and for every case the folded results of the first batches are
//! asserted equal between `ijson` and `serde` (and equal to the expected value
//! where one is known) before anything is timed.

use std::fmt::Write as _;
use std::hint::black_box;

use ijson::array::ArraySliceRef;
use ijson::{DestructuredRef, IString, IValue, ValueType};
use serde_json::Value;

use crate::{ijson_parse, serde_parse, Ctx, Params, Row, Work};

/// Operations per timed call for the single-operation cases.
const K: usize = 64;

// ---------------------------------------------------------------- utilities

/// SplitMix64. Seeded, no OS randomness.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (((self.next() >> 32) * n as u64) >> 32) as usize
    }
}

fn row(case: &str, imp: &'static str, items: usize) -> Row {
    Row { group: "lookup", case: case.to_string(), imp, work: Work::Items(items) }
}

fn wants(ctx: &Ctx, case: &str) -> bool {
    ctx.wants(&row(case, "ijson", 1))
}

fn is_quick(ctx: &Ctx) -> bool {
    ctx.params.min_time_ns < Params::default().min_time_ns
}

/// Target size of a cold working set, in estimated ijson heap bytes.
fn cold_bytes(ctx: &Ctx) -> usize {
    if is_quick(ctx) {
        4 << 20
    } else {
        48 << 20
    }
}

/// Verify `rounds` calls of both closures agree (and match `expected` on the
/// first call, when given), then time both.
fn pair(
    ctx: &mut Ctx,
    case: &str,
    items: usize,
    rounds: usize,
    expected: Option<u64>,
    mut fi: impl FnMut() -> u64,
    mut fs: impl FnMut() -> u64,
) {
    if !wants(ctx, case) {
        return;
    }
    for r in 0..rounds.max(1) {
        let (a, b) = (fi(), fs());
        assert_eq!(a, b, "lookup/{case}: ijson result {a} != serde result {b} (round {r})");
        if let (0, Some(e)) = (r, expected) {
            assert_eq!(a, e, "lookup/{case}: result {a} != expected {e}");
        }
    }
    ctx.bench(row(case, "ijson", items), fi);
    ctx.bench(row(case, "serde", items), fs);
}

/// Turn a per-operation function into a closure doing `K` operations per call,
/// walking `ops` cyclically.
fn batched<'a, T>(ops: &'a [T], mut f: impl FnMut(&T) -> u64 + 'a) -> impl FnMut() -> u64 + 'a {
    assert!(!ops.is_empty() && ops.len() % K == 0);
    let mut cur = 0;
    move || {
        let mut acc = 0u64;
        for op in &ops[cur..cur + K] {
            acc = acc.wrapping_add(f(op));
        }
        cur += K;
        if cur == ops.len() {
            cur = 0;
        }
        acc
    }
}

/// `len` (document, index) pairs in seeded random order.
fn mk_ops(seed: u64, len: usize, ndocs: usize, nidx: usize) -> Vec<(u32, u32)> {
    let mut rng = Rng(seed);
    let len = len.next_multiple_of(K);
    (0..len).map(|_| (rng.below(ndocs) as u32, rng.below(nidx) as u32)).collect()
}

fn ops_len(ndocs: usize) -> usize {
    if ndocs == 1 {
        4096
    } else {
        ndocs.clamp(1 << 16, 1 << 20)
    }
}

fn verify_rounds(ops: usize) -> usize {
    (ops / K).min(256)
}

/// Build `ndocs` documents with both implementations (all ijson documents
/// first, so each implementation's heap is contiguous-ish as after a bulk
/// load) and check a sample for equality.
fn build(what: &str, ndocs: usize, gen: impl Fn(usize, &mut String)) -> (Vec<IValue>, Vec<Value>) {
    let mut buf = String::new();
    let mut ij = Vec::with_capacity(ndocs);
    for d in 0..ndocs {
        buf.clear();
        gen(d, &mut buf);
        ij.push(ijson_parse(&buf));
    }
    let mut sj = Vec::with_capacity(ndocs);
    let stride = (ndocs / 256).max(1);
    for d in 0..ndocs {
        buf.clear();
        gen(d, &mut buf);
        let s = serde_parse(&buf);
        if d % stride == 0 {
            let back = serde_json::to_value(&ij[d]).expect("ijson value serialises");
            assert!(back == s, "lookup/{what}: ijson and serde documents differ (doc {d})");
        }
        sj.push(s);
    }
    (ij, sj)
}

// ------------------------------------------------- the operations under test

/// `SelectValue::get_key` for `IValue`.
#[inline(always)]
fn ij_get_key<'a>(v: &'a IValue, key: &str) -> Option<&'a IValue> {
    v.as_object().and_then(|o| o.get(key))
}

#[inline(always)]
fn sj_get_key<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.as_object().and_then(|o| o.get(key))
}

/// What `SelectValue::get_index` hands back: `ValueRef::{Borrowed, Owned}`.
enum Elem<'a> {
    Borrowed(&'a IValue),
    Owned(IValue),
}

impl Elem<'_> {
    #[inline(always)]
    fn get(&self) -> &IValue {
        match self {
            Elem::Borrowed(v) => v,
            Elem::Owned(v) => v,
        }
    }
}

/// `SelectValue::get_index` for `IValue`.
#[inline(always)]
fn ij_get_index(v: &IValue, index: usize) -> Option<Elem<'_>> {
    let arr = v.as_array()?;
    macro_rules! indexed {
        ($($variant:ident),*) => {
            match arr.as_slice() {
                ArraySliceRef::Heterogeneous(s) => s.get(index).map(Elem::Borrowed),
                $(ArraySliceRef::$variant(s) =>
                    s.get(index).map(|&v| Elem::Owned(IValue::from(v))),)*
            }
        }
    }
    indexed!(I8, U8, I16, U16, F16, BF16, I32, U32, F32, I64, U64, F64)
}

#[inline(always)]
fn sj_get_index(v: &Value, index: usize) -> Option<&Value> {
    v.as_array().and_then(|a| a.get(index))
}

/// Read an element the way a consumer does after `get_type`: numbers through
/// `get_double` (`to_f64_lossy`), everything else by its type.
#[inline(always)]
fn ij_fold(v: &IValue) -> u64 {
    match v.type_() {
        ValueType::Number => v.as_number().map_or(0, |n| n.to_f64_lossy() as u64),
        ValueType::Null => 1,
        ValueType::Bool => 2,
        ValueType::String => 3,
        ValueType::Array => 4,
        ValueType::Object => 5,
    }
}

#[inline(always)]
fn sj_fold(v: &Value) -> u64 {
    match v {
        Value::Number(n) => n.as_f64().map_or(0, |f| f as u64),
        Value::Null => 1,
        Value::Bool(_) => 2,
        Value::String(_) => 3,
        Value::Array(_) => 4,
        Value::Object(_) => 5,
    }
}

/// `SelectValue::values` without the box.
#[inline(always)]
fn ij_values(v: &IValue) -> u64 {
    let mut acc = 0u64;
    match v.destructure_ref() {
        DestructuredRef::Array(arr) => {
            for e in arr.iter() {
                acc = acc.wrapping_add(ij_fold(e.as_ref()));
            }
        }
        DestructuredRef::Object(o) => {
            for e in o.values() {
                acc = acc.wrapping_add(ij_fold(e));
            }
        }
        _ => {}
    }
    acc
}

#[inline(always)]
fn sj_values(v: &Value) -> u64 {
    let mut acc = 0u64;
    match v {
        Value::Array(arr) => {
            for e in arr {
                acc = acc.wrapping_add(sj_fold(e));
            }
        }
        Value::Object(o) => {
            for e in o.values() {
                acc = acc.wrapping_add(sj_fold(e));
            }
        }
        _ => {}
    }
    acc
}

/// `SelectValue::keys` without the box.
#[inline(always)]
fn ij_keys(v: &IValue) -> u64 {
    match v.destructure_ref() {
        DestructuredRef::Object(o) => o.keys().map(IString::as_str).map(|k| k.len() as u64).sum(),
        _ => 0,
    }
}

#[inline(always)]
fn sj_keys(v: &Value) -> u64 {
    v.as_object().map_or(0, |o| o.keys().map(|k| k.len() as u64).sum())
}

// ------------------------------------------------------------ object lookup

const STEMS: [&str; 16] = [
    "id",
    "name",
    "email",
    "created_at",
    "updated_at",
    "customer_id",
    "shipping_address",
    "status",
    "price",
    "quantity",
    "description",
    "category",
    "tags",
    "metadata",
    "last_login_timestamp",
    "is_active",
];

fn key_name(i: usize) -> String {
    if i < STEMS.len() {
        STEMS[i].to_string()
    } else {
        format!("{}_{}", STEMS[i % STEMS.len()], i / STEMS.len())
    }
}

fn obj_value(d: usize, i: usize) -> u64 {
    (d as u64) * 7919 + i as u64
}

fn gen_obj(keys: &[String]) -> impl Fn(usize, &mut String) + '_ {
    move |d, buf| {
        buf.push('{');
        for (i, k) in keys.iter().enumerate() {
            if i > 0 {
                buf.push(',');
            }
            write!(buf, "\"{k}\":{}", obj_value(d, i)).unwrap();
        }
        buf.push('}');
    }
}

fn objects(ctx: &mut Ctx, n: usize, cold: bool) {
    let temp = if cold { "cold" } else { "hot" };
    let c_hit = format!("obj-{n}keys-hit-{temp}");
    let c_miss = format!("obj-{n}keys-miss-{temp}");
    let c_missi = format!("obj-{n}keys-miss-interned-{temp}");
    let c_vals = format!("iter-obj-{n}keys-values-{temp}");
    let c_keys = format!("iter-obj-{n}keys-keys-{temp}");
    if ![&c_hit, &c_miss, &c_missi, &c_vals, &c_keys].iter().any(|c| wants(ctx, c)) {
        return;
    }
    let ndocs = if cold { (cold_bytes(ctx) / (21 * n + 32)).max(8) } else { 1 };
    let keys: Vec<String> = (0..n).map(key_name).collect();
    // Same shape as the real keys; never interned by anything else.
    let absent: Vec<String> = keys.iter().map(|k| format!("{k}_x")).collect();
    // Absent from the objects, but alive in ijson's string cache.
    let other: Vec<String> = keys.iter().map(|k| format!("{k}_y")).collect();
    let _alive: Vec<IString> = other.iter().map(|k| IString::intern(k)).collect();
    let (ij, sj) = build(&c_hit, ndocs, gen_obj(&keys));
    let ops = mk_ops(0x10 + n as u64, ops_len(ndocs), ndocs, n);
    let rounds = verify_rounds(ops.len());

    // Values of every op, checked once against the generator.
    if wants(ctx, &c_hit) {
        for &(d, k) in ops.iter().take(1 << 14) {
            let want = obj_value(d as usize, k as usize);
            let a = ij_get_key(&ij[d as usize], &keys[k as usize]).and_then(IValue::to_u64);
            let b = sj_get_key(&sj[d as usize], &keys[k as usize]).and_then(Value::as_u64);
            assert!(a == Some(want) && b == Some(want), "lookup/{c_hit}: got {a:?} / {b:?}, expected {want}");
        }
    }

    for (case, names, expect) in [(&c_hit, &keys, K as u64), (&c_miss, &absent, 0), (&c_missi, &other, 0)] {
        pair(
            ctx,
            case,
            K,
            rounds,
            Some(expect),
            batched(&ops, |&(d, k)| {
                ij_get_key(black_box(&ij[d as usize]), &names[k as usize]).is_some() as u64
            }),
            batched(&ops, |&(d, k)| {
                sj_get_key(black_box(&sj[d as usize]), &names[k as usize]).is_some() as u64
            }),
        );
    }

    // Iteration over whole objects, `per` documents per call.
    if n == 1 || n == 16 || n == 256 {
        return;
    }
    let per = (K / n).max(1);
    let order: Vec<u32> = ops.iter().map(|&(d, _)| d).collect();
    let chunks = || {
        let mut cur = 0;
        let order = &order;
        move || {
            let c = &order[cur..cur + per];
            cur = if cur + 2 * per > order.len() { 0 } else { cur + per };
            c
        }
    };
    let vr = rounds.min(32);
    let (mut ni, mut ns) = (chunks(), chunks());
    pair(
        ctx,
        &c_vals,
        per * n,
        vr,
        None,
        || ni().iter().fold(0u64, |a, &d| a.wrapping_add(ij_values(black_box(&ij[d as usize])))),
        || ns().iter().fold(0u64, |a, &d| a.wrapping_add(sj_values(black_box(&sj[d as usize])))),
    );
    let (mut ni, mut ns) = (chunks(), chunks());
    let key_bytes: u64 = keys.iter().map(|k| k.len() as u64).sum();
    pair(
        ctx,
        &c_keys,
        per * n,
        vr,
        Some(key_bytes * per as u64),
        || ni().iter().fold(0u64, |a, &d| a.wrapping_add(ij_keys(black_box(&ij[d as usize])))),
        || ns().iter().fold(0u64, |a, &d| a.wrapping_add(sj_keys(black_box(&sj[d as usize])))),
    );
}

// ------------------------------------------------------------------- arrays

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Hetero,
    Ints,
    Floats,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Hetero => "hetero",
            Kind::Ints => "ints",
            Kind::Floats => "floats",
        }
    }
}

fn gen_arr(kind: Kind, len: usize) -> impl Fn(usize, &mut String) {
    move |d, buf| {
        buf.push('[');
        for i in 0..len {
            if i > 0 {
                buf.push(',');
            }
            let h = (d * 31 + i * 7) % 100_000;
            match (kind, i % 6) {
                (Kind::Ints, _) | (Kind::Hetero, 0) => write!(buf, "{h}").unwrap(),
                (Kind::Floats, _) | (Kind::Hetero, 1) => write!(buf, "{}.{:04}", h / 100, (h * 37) % 10_000).unwrap(),
                (Kind::Hetero, 2) => write!(buf, "\"item-{d}-{i}\"").unwrap(),
                (Kind::Hetero, 3) => buf.push_str("true"),
                (Kind::Hetero, 4) => buf.push_str("null"),
                (Kind::Hetero, _) => write!(buf, "{{\"k\":{i}}}").unwrap(),
            }
        }
        buf.push(']');
    }
}

fn arrays(ctx: &mut Ctx, kind: Kind, len: usize, cold: bool) {
    let temp = if cold { "cold" } else { "hot" };
    let c_idx = format!("arr-{}-len{len}-index-{temp}", kind.name());
    let c_iter = format!("iter-arr-{}-len{len}-{temp}", kind.name());
    if !wants(ctx, &c_idx) && !wants(ctx, &c_iter) {
        return;
    }
    let ndocs = if cold { (cold_bytes(ctx) / (8 * len + 16)).max(8) } else { 1 };
    let (ij, sj) = build(&c_idx, ndocs, gen_arr(kind, len));

    // The representation must be the one the parse path produces: typed for
    // homogeneous numbers (with these inputs the parser picks the narrowest
    // integer tag that fits, I8/I16/I32, and F64 for fractional numbers),
    // heterogeneous otherwise.
    let typed = ij[0].as_array().expect("array").as_slice().is_typed();
    assert_eq!(typed, kind != Kind::Hetero, "lookup/{c_idx}: unexpected array representation (typed = {typed})");

    let ops = mk_ops(0x20 + len as u64, ops_len(ndocs), ndocs, len);
    pair(
        ctx,
        &c_idx,
        K,
        verify_rounds(ops.len()),
        None,
        batched(&ops, |&(d, i)| {
            ij_get_index(black_box(&ij[d as usize]), i as usize).map_or(u64::MAX, |e| ij_fold(e.get()))
        }),
        batched(&ops, |&(d, i)| sj_get_index(black_box(&sj[d as usize]), i as usize).map_or(u64::MAX, sj_fold)),
    );

    let per = (K / len).max(1);
    let order: Vec<u32> = ops.iter().map(|&(d, _)| d).collect();
    let chunks = || {
        let mut cur = 0;
        let order = &order;
        move || {
            let c = &order[cur..cur + per];
            cur = if cur + 2 * per > order.len() { 0 } else { cur + per };
            c
        }
    };
    let (mut ni, mut ns) = (chunks(), chunks());
    pair(
        ctx,
        &c_iter,
        per * len,
        8,
        None,
        || ni().iter().fold(0u64, |a, &d| a.wrapping_add(ij_values(black_box(&ij[d as usize])))),
        || ns().iter().fold(0u64, |a, &d| a.wrapping_add(sj_values(black_box(&sj[d as usize])))),
    );
}

// --------------------------------------------------------------- deep paths

enum Step {
    Key(String),
    Index(usize),
}

fn path_value(d: usize) -> u64 {
    d as u64 * 13 + 3
}

/// `depth` nested objects, each with four sibling members, then
/// `items[3].e`.
fn gen_path(depth: usize) -> impl Fn(usize, &mut String) {
    move |d, buf| {
        for j in 0..depth {
            write!(buf, "{{\"created_at\":{d},\"name\":\"level-{j}\",\"node_{j}\":").unwrap();
        }
        write!(
            buf,
            "{{\"count\":5,\"items\":[{{\"e\":0}},{{\"e\":1}},{{\"e\":2}},{{\"e\":{}}},{{\"e\":4}}]}}",
            path_value(d)
        )
        .unwrap();
        for _ in 0..depth {
            buf.push_str(",\"is_active\":true,\"status\":null}");
        }
    }
}

#[inline(always)]
fn ij_walk(root: &IValue, steps: &[Step]) -> u64 {
    let mut cur = root;
    for s in steps {
        let next = match s {
            Step::Key(k) => ij_get_key(cur, k),
            // Intermediate containers are always in heterogeneous arrays, the
            // borrowing arm of `get_index`.
            Step::Index(i) => match ij_get_index(cur, *i) {
                Some(Elem::Borrowed(v)) => Some(v),
                _ => None,
            },
        };
        match next {
            Some(v) => cur = v,
            None => return u64::MAX,
        }
    }
    // `get_long`
    match cur.type_() {
        ValueType::Number => cur.as_number().and_then(|n| n.to_i64()).map_or(u64::MAX, |x| x as u64),
        _ => u64::MAX,
    }
}

#[inline(always)]
fn sj_walk(root: &Value, steps: &[Step]) -> u64 {
    let mut cur = root;
    for s in steps {
        let next = match s {
            Step::Key(k) => sj_get_key(cur, k),
            Step::Index(i) => sj_get_index(cur, *i),
        };
        match next {
            Some(v) => cur = v,
            None => return u64::MAX,
        }
    }
    cur.as_i64().map_or(u64::MAX, |x| x as u64)
}

fn paths(ctx: &mut Ctx, depth: usize, cold: bool) {
    let temp = if cold { "cold" } else { "hot" };
    let case = format!("path-depth{depth}-{temp}");
    if !wants(ctx, &case) {
        return;
    }
    let ndocs = if cold { (cold_bytes(ctx) / (112 * depth + 320)).max(8) } else { 1 };
    let (ij, sj) = build(&case, ndocs, gen_path(depth));
    let mut steps: Vec<Step> = (0..depth).map(|j| Step::Key(format!("node_{j}"))).collect();
    steps.extend([Step::Key("items".into()), Step::Index(3), Step::Key("e".into())]);
    let ops = mk_ops(0x30 + depth as u64, ops_len(ndocs), ndocs, 1);
    for &(d, _) in ops.iter().take(1 << 12) {
        let got = ij_walk(&ij[d as usize], &steps);
        assert_eq!(got, path_value(d as usize), "lookup/{case}: wrong value at the end of the path");
    }
    let steps = &steps[..];
    pair(
        ctx,
        &case,
        K,
        verify_rounds(ops.len()),
        None,
        batched(&ops, |&(d, _)| ij_walk(black_box(&ij[d as usize]), black_box(steps))),
        batched(&ops, |&(d, _)| sj_walk(black_box(&sj[d as usize]), black_box(steps))),
    );
}

// ------------------------------------------------------------------ scalars

#[derive(Clone, Copy, PartialEq)]
enum Scalar {
    Mixed,
    StrShort,
    StrLong,
    IntSmall,
    IntLarge,
    Float,
    Bool,
}

/// One array of `n` scalars. A leading empty string keeps the array
/// heterogeneous, so numbers are stored as `INumber` values, as they are in
/// object members.
fn gen_scalars(kind: Scalar, n: usize) -> impl Fn(usize, &mut String) {
    move |_, buf| {
        buf.push_str("[\"\"");
        for i in 0..n {
            buf.push(',');
            let k = match kind {
                Scalar::Mixed => [
                    Scalar::IntSmall,
                    Scalar::StrLong,
                    Scalar::Float,
                    Scalar::Bool,
                    Scalar::StrShort,
                    Scalar::IntLarge,
                    Scalar::Mixed,
                ][i % 7],
                k => k,
            };
            match k {
                Scalar::Mixed => buf.push_str(["null", "[1,2,3]", "{\"a\":1,\"b\":2}"][(i / 7) % 3]),
                Scalar::StrShort => write!(buf, "\"s{}\"", i % 1000).unwrap(),
                Scalar::StrLong => write!(buf, "\"customer-{i:08}-shipping-address-line\"").unwrap(),
                Scalar::IntSmall => write!(buf, "{}", i % 10_000).unwrap(),
                Scalar::IntLarge => write!(buf, "{}", (1u64 << 40) + i as u64).unwrap(),
                Scalar::Float => write!(buf, "{}.{:03}", i, (i * 7) % 1000 | 1).unwrap(),
                Scalar::Bool => buf.push_str(if i % 3 == 0 { "true" } else { "false" }),
            }
        }
        buf.push(']');
    }
}

#[inline(always)]
fn ij_type(v: &IValue) -> u64 {
    // `SelectValue::get_type`
    match v.type_() {
        ValueType::Bool => 0,
        ValueType::String => 1,
        ValueType::Null => 2,
        ValueType::Array => 3,
        ValueType::Object => 4,
        ValueType::Number => {
            let num = v.as_number().unwrap();
            if num.has_decimal_point() | num.to_i64().is_none() {
                5
            } else {
                6
            }
        }
    }
}

#[inline(always)]
fn sj_type(v: &Value) -> u64 {
    match v {
        Value::Bool(_) => 0,
        Value::String(_) => 1,
        Value::Null => 2,
        Value::Array(_) => 3,
        Value::Object(_) => 4,
        Value::Number(n) => {
            if n.is_i64() {
                6
            } else {
                5
            }
        }
    }
}

#[inline(always)]
fn ij_destructure(v: &IValue) -> u64 {
    match v.destructure_ref() {
        DestructuredRef::Null => 7,
        DestructuredRef::Bool(b) => b as u64,
        DestructuredRef::Number(n) => n.to_f64_lossy() as u64,
        DestructuredRef::String(s) => s.len() as u64,
        DestructuredRef::Array(a) => a.len() as u64,
        DestructuredRef::Object(o) => o.len() as u64,
    }
}

#[inline(always)]
fn sj_destructure(v: &Value) -> u64 {
    match v {
        Value::Null => 7,
        Value::Bool(b) => *b as u64,
        Value::Number(n) => n.as_f64().map_or(0, |f| f as u64),
        Value::String(s) => s.len() as u64,
        Value::Array(a) => a.len() as u64,
        Value::Object(o) => o.len() as u64,
    }
}

#[inline(always)]
fn ij_str(v: &IValue) -> u64 {
    // `SelectValue::as_str`, then the bytes are looked at.
    v.as_string().map(IString::as_str).map_or(u64::MAX, |s| s.len() as u64 + s.as_bytes()[0] as u64)
}

#[inline(always)]
fn sj_str(v: &Value) -> u64 {
    v.as_str().map_or(u64::MAX, |s| s.len() as u64 + s.as_bytes()[0] as u64)
}

#[inline(always)]
fn ij_long(v: &IValue) -> u64 {
    // `SelectValue::get_long`
    match v.type_() {
        ValueType::Number => v.as_number().and_then(|n| n.to_i64()).map_or(u64::MAX, |x| x as u64),
        _ => u64::MAX,
    }
}

#[inline(always)]
fn sj_long(v: &Value) -> u64 {
    v.as_i64().map_or(u64::MAX, |x| x as u64)
}

#[inline(always)]
fn ij_double(v: &IValue) -> u64 {
    // `SelectValue::get_double`
    match v.type_() {
        ValueType::Number => v.as_number().map_or(u64::MAX, |n| n.to_f64_lossy().to_bits()),
        _ => u64::MAX,
    }
}

#[inline(always)]
fn sj_double(v: &Value) -> u64 {
    v.as_f64().map_or(u64::MAX, f64::to_bits)
}

#[inline(always)]
fn ij_bool(v: &IValue) -> u64 {
    v.to_bool().map_or(u64::MAX, |b| b as u64)
}

#[inline(always)]
fn sj_bool(v: &Value) -> u64 {
    v.as_bool().map_or(u64::MAX, |b| b as u64)
}

/// `fi` / `fs` are generic so each operation is monomorphised and inlined
/// into its loop; a function pointer here would time an indirect call.
fn scalars(
    ctx: &mut Ctx,
    kind: Scalar,
    case: &str,
    fi: impl Fn(&IValue) -> u64,
    fs: impl Fn(&Value) -> u64,
) {
    if !wants(ctx, case) {
        return;
    }
    let cold = case.ends_with("-cold");
    // Cold: one large array of distinct heap scalars, read in random order.
    let n = if cold { cold_bytes(ctx) / if kind == Scalar::StrLong { 72 } else { 24 } } else { 1024 };
    let (ij, sj) = build(case, 1, gen_scalars(kind, n));
    let ij: &[IValue] = match ij[0].as_array().expect("array").as_slice() {
        ArraySliceRef::Heterogeneous(s) => &s[1..],
        _ => panic!("lookup/{case}: scalar holder is not a heterogeneous array"),
    };
    let sj: &[Value] = &sj[0].as_array().expect("array")[1..];
    assert_eq!(ij.len(), n);
    if cold {
        let order = mk_ops(0x40, n.clamp(1 << 16, 1 << 20), 1, n);
        pair(
            ctx,
            case,
            K,
            verify_rounds(order.len()),
            None,
            batched(&order, |&(_, i)| fi(black_box(&ij[i as usize]))),
            batched(&order, |&(_, i)| fs(black_box(&sj[i as usize]))),
        );
    } else {
        pair(
            ctx,
            case,
            n,
            1,
            None,
            || black_box(ij).iter().fold(0u64, |a, v| a.wrapping_add(fi(v))),
            || black_box(sj).iter().fold(0u64, |a, v| a.wrapping_add(fs(v))),
        );
    }
}

// ---------------------------------------------------------------------- run

pub fn run(ctx: &mut Ctx) {
    for n in [1, 4, 16, 64, 256, 4096] {
        objects(ctx, n, false);
    }
    for n in [4, 64, 4096] {
        objects(ctx, n, true);
    }

    for kind in [Kind::Hetero, Kind::Ints, Kind::Floats] {
        for len in [4, 64, 1024, 65536] {
            arrays(ctx, kind, len, false);
        }
        arrays(ctx, kind, 64, true);
    }

    for depth in [1, 4, 16] {
        paths(ctx, depth, false);
    }
    paths(ctx, 4, true);

    scalars(ctx, Scalar::Mixed, "scalar-type-mixed-hot", ij_type, sj_type);
    scalars(ctx, Scalar::Mixed, "scalar-destructure-mixed-hot", ij_destructure, sj_destructure);
    scalars(ctx, Scalar::StrShort, "scalar-str-short-hot", ij_str, sj_str);
    scalars(ctx, Scalar::StrLong, "scalar-str-long-hot", ij_str, sj_str);
    scalars(ctx, Scalar::IntSmall, "scalar-i64-small-hot", ij_long, sj_long);
    scalars(ctx, Scalar::IntSmall, "scalar-f64-small-hot", ij_double, sj_double);
    scalars(ctx, Scalar::IntLarge, "scalar-i64-large-hot", ij_long, sj_long);
    scalars(ctx, Scalar::Float, "scalar-f64-fractional-hot", ij_double, sj_double);
    scalars(ctx, Scalar::Float, "scalar-type-fractional-hot", ij_type, sj_type);
    scalars(ctx, Scalar::Bool, "scalar-bool-values-hot", ij_bool, sj_bool);
    scalars(ctx, Scalar::StrLong, "scalar-str-long-cold", ij_str, sj_str);
    scalars(ctx, Scalar::Float, "scalar-f64-fractional-cold", ij_double, sj_double);
    scalars(ctx, Scalar::IntLarge, "scalar-i64-large-cold", ij_long, sj_long);
}
