//! Homogeneous numeric arrays, which ijson stores in compact typed form
//! (`ArrayTag::I8` .. `F64`), on the paths RedisJSON drives them through.
//!
//! | case | RedisJSON call site |
//! |---|---|
//! | `parse` | `redis_json/src/ivalue_manager.rs` `from_str`: `IValueDeserSeed::new(fpha).deserialize_compact_objects`. `fpha` is `None` by default; `JSON.SET ... FPHA FP32` (`commands.rs`, which maps `BF16`/`FP16`/`FP32`/`FP64` to `ijson::FloatType`) passes `Some(FPHAConfig::new_with_type(..))`. The `f32` kind uses that option, since without it floats parse to `F64` |
//! | `push-same`, `push-widen`, `push-retag-*` | `ivalue_manager.rs` `arr_append`: `as_array_mut`, `arr.try_extend(args)` with one argument, i.e. `IArray::push` -> `prepare_for_item` -> `promote_to_type` when the value does not fit the current tag |
//! | `push-retag-emptied` | `clear` (`arr.clear()`) followed by `arr_append` of a string: the emptied-typed-array retag in `prepare_for_item` |
//! | `extend` | `arr_append` with many arguments: `arr.try_extend(Vec<IValue>)` (the generic `TryExtend<IValue>`, one `push` per item) |
//! | `get` | `json_path/src/json_node.rs` `SelectValue for IValue::get_index`: `arr.as_slice()`, index the typed slice, `IValue::from(v)` (an owned value per read) |
//! | `sum` | `json_node.rs` `values()`: `arr.iter()` yields `ArrayIterItem` (owned `IValue` per element for typed arrays), then `get_double` (`as_number().to_f64_lossy()`) |
//! | `sum` (`ijson-slice`) | `json_node.rs` `get_array` / `get_array_type`: the raw typed slice handed out through the C API, read without building values |
//! | `numincr-inplace`, `numincr-overflow` | `ivalue_manager.rs` `do_num_op`, the `PathValue::I8..F64` arms: `as_mut_slice_of::<T>().get_mut(index)` and write in place, or `remove(index)` + `insert(index, wider)` when the result does not fit |
//!
//! Kinds: `i8`, `i16`, `i32`, `i64` (integers in that range, at least one
//! negative), `f64`, `f32` (FPHA FP32), and `hetero`: the `i32` values in a
//! heterogeneous array (parsed with a trailing string that is then popped),
//! for contrast. The expected `ArrayTag` of each kind is asserted.
//!
//! Conventions: `units` is the number of operations (or bytes for `parse`)
//! in one timed call. Values to push are built in the setup, since RedisJSON
//! parses arguments before taking the write holder. `push-same` reserves
//! room in the setup so that it times the push alone; the widening and
//! retagging cases and `extend` start from a plain clone and include
//! whatever reallocation they cause. Every case is run once on both
//! implementations and compared before it is timed.
//!
//! In `--quick` mode the large size is smaller and only a subset of the
//! kind and size combinations runs.

use std::hint::black_box;
use std::mem::take;

use ijson::array::{ArraySliceRef, ArrayTag, TryExtend};
use ijson::{FPHAConfig, FloatType, IArray, IValue, IValueDeserSeed};
use serde_json::Value;

use crate::{ijson_parse, ijson_serialize, serde_parse, Ctx, Params, Row, Work};

const GROUP: &str = "typed_array";

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in `-bound..=bound`.
    fn signed(&mut self, bound: i64) -> i64 {
        (self.next() % (2 * bound as u64 + 1)) as i64 - bound
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    I8,
    I16,
    I32,
    I64,
    F64,
    F32,
    Hetero,
}

const KINDS: [Kind; 7] = [Kind::I8, Kind::I16, Kind::I32, Kind::I64, Kind::F64, Kind::F32, Kind::Hetero];

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::I8 => "i8",
            Kind::I16 => "i16",
            Kind::I32 => "i32",
            Kind::I64 => "i64",
            Kind::F64 => "f64",
            Kind::F32 => "f32",
            Kind::Hetero => "hetero",
        }
    }
    fn tag(self) -> ArrayTag {
        match self {
            Kind::I8 => ArrayTag::I8,
            Kind::I16 => ArrayTag::I16,
            Kind::I32 => ArrayTag::I32,
            Kind::I64 => ArrayTag::I64,
            Kind::F64 => ArrayTag::F64,
            Kind::F32 => ArrayTag::F32,
            Kind::Hetero => ArrayTag::Heterogeneous,
        }
    }
    /// One element as JSON text. `first` forces a negative value that needs
    /// the full width, so the tag does not depend on the random draw.
    fn element(self, rng: &mut Rng, first: bool) -> String {
        let int = |rng: &mut Rng, bound: i64| if first { -bound } else { rng.signed(bound) };
        match self {
            Kind::I8 => int(rng, 100).to_string(),
            Kind::I16 => int(rng, 30_000).to_string(),
            Kind::I32 | Kind::Hetero => int(rng, 2_000_000_000).to_string(),
            Kind::I64 => int(rng, 1 << 50).to_string(),
            // Not representable in f32, so these stay f64.
            Kind::F64 => format!("{}.{}", rng.signed(1_000_000), 1 + rng.next() % 9),
            // Exactly representable in f32, so both sides hold the same numbers.
            Kind::F32 => format!("{}.5", rng.signed(4096)),
        }
    }
    fn text(self, n: usize, rng: &mut Rng) -> String {
        let mut s = String::from("[");
        for i in 0..n {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&self.element(rng, i == 0));
        }
        if self == Kind::Hetero {
            s.push_str(",\"x\"");
        }
        s.push(']');
        s
    }
    /// The production parse path for this kind.
    fn parse(self, text: &str) -> IValue {
        match self {
            Kind::F32 => ijson_parse_fpha(text, FloatType::F32),
            _ => ijson_parse(text),
        }
    }
    /// Both representations of an `n`-element array of this kind.
    fn base(self, n: usize, seed: u64) -> (IValue, Value) {
        let mut rng = Rng(seed);
        let text = self.text(n, &mut rng);
        let (mut a, mut b) = (self.parse(&text), serde_parse(&text));
        if self == Kind::Hetero {
            a.as_array_mut().unwrap().pop();
            b.as_array_mut().unwrap().pop();
        }
        let tag = a.as_array().expect("array").as_slice().type_tag();
        assert!(
            tag == self.tag(),
            "typed_array: {} array of {n} is stored as {tag:?}, expected {:?}",
            self.name(),
            self.tag()
        );
        same(&format!("base/{}/{n}", self.name()), &a, &b);
        (a, b)
    }
    /// `k` more values of this kind, as parsed command arguments.
    fn values(self, k: usize, seed: u64) -> (Vec<IValue>, Vec<Value>) {
        let mut rng = Rng(seed ^ 0x7a1e);
        (0..k)
            .map(|_| {
                let t = self.element(&mut rng, false);
                (ijson_parse(&t), serde_parse(&t))
            })
            .unzip()
    }
}

/// `from_str` in `ivalue_manager.rs` with `fpha_type = Some(..)`.
#[inline(never)]
fn ijson_parse_fpha(text: &str, ty: FloatType) -> IValue {
    let mut de = serde_json::Deserializer::from_str(text);
    let v = IValueDeserSeed::new(Some(FPHAConfig::new_with_type(ty)))
        .deserialize_compact_objects(&mut de)
        .expect("parses");
    de.end().expect("no trailing data");
    v
}

fn same(what: &str, a: &IValue, b: &Value) {
    assert!(
        serde_parse(&ijson_serialize(a)) == *b,
        "typed_array/{what}: ijson and serde_json documents differ"
    );
}

fn row(case: &str, imp: &'static str, work: Work) -> Row {
    Row { group: GROUP, case: case.to_string(), imp, work }
}

/// Verify once, then time a mutation on both implementations.
fn case<AI, RI, AS, RS>(
    ctx: &mut Ctx,
    name: &str,
    k: usize,
    mut si: impl FnMut() -> (IValue, AI),
    mut oi: impl FnMut(&mut (IValue, AI)) -> RI,
    mut ss: impl FnMut() -> (Value, AS),
    mut os: impl FnMut(&mut (Value, AS)) -> RS,
) {
    {
        let (mut a, mut b) = (si(), ss());
        let (_ra, _rb) = (oi(&mut a), os(&mut b));
        same(name, &a.0, &b.0);
    }
    ctx.bench_with_setup(row(name, "ijson", Work::Items(k)), &mut si, &mut oi);
    ctx.bench_with_setup(row(name, "serde", Work::Items(k)), &mut ss, &mut os);
}

fn wants(ctx: &Ctx, name: &str) -> bool {
    ctx.wants(&row(name, "ijson", Work::Items(1)))
}

/// `arr_append` with one argument. `Option` stands in for the one-element
/// `Vec` so that freeing the argument vector is not timed.
#[inline]
fn push_i(doc: &mut IValue, v: IValue) -> u32 {
    let arr = doc.as_array_mut().expect("array");
    arr.try_extend(Some(v)).expect("push");
    arr.len()
}

#[inline]
fn push_s(doc: &mut Value, v: Value) -> usize {
    let arr = doc.as_array_mut().expect("array");
    arr.extend(Some(v));
    arr.len()
}

/// `get_index` in `json_node.rs`: an owned value for typed arrays.
#[inline]
fn get_index(arr: &IArray, index: usize) -> Option<f64> {
    macro_rules! indexed {
        ($($variant:ident),*) => {
            match arr.as_slice() {
                ArraySliceRef::Heterogeneous(s) => s.get(index).map(|v| v.as_number().unwrap().to_f64_lossy()),
                $(ArraySliceRef::$variant(s) => s.get(index).map(|&v| {
                    let owned = black_box(IValue::from(v));
                    owned.as_number().unwrap().to_f64_lossy()
                }),)*
            }
        };
    }
    indexed!(I8, U8, I16, U16, F16, BF16, I32, U32, F32, I64, U64, F64)
}

/// The typed slice read directly, as a C API consumer of `get_array` would.
#[inline]
fn sum_slice(arr: &IArray) -> f64 {
    macro_rules! summed {
        ([$($lossless:ident),*], [$($wide:ident),*]) => {
            match arr.as_slice() {
                ArraySliceRef::Heterogeneous(s) => s.iter().map(|v| v.as_number().unwrap().to_f64_lossy()).sum(),
                $(ArraySliceRef::$lossless(s) => s.iter().map(|&v| f64::from(v)).sum(),)*
                $(ArraySliceRef::$wide(s) => s.iter().map(|&v| v as f64).sum(),)*
            }
        };
    }
    summed!([I8, U8, I16, U16, F16, BF16, I32, U32, F32, F64], [I64, U64])
}

fn serde_f64(v: &Value) -> f64 {
    v.as_f64().expect("number")
}

pub fn run(ctx: &mut Ctx) {
    let quick = ctx.params.min_time_ns < Params::default().min_time_ns;
    let big = if quick { 10_000 } else { 100_000 };
    let sizes = [8usize, 1024, big];

    for kind in KINDS {
        for n in sizes {
            // Quick mode is a smoke test: every kind at 1024, three kinds at the large size.
            let smoke = n == 1024 || (n == big && matches!(kind, Kind::I8 | Kind::F64 | Kind::Hetero));
            if quick && !smoke {
                continue;
            }
            let seed = 0x7a00_0000 + ((kind as u64) << 24) + n as u64;
            let tag = format!("{}/{n}", kind.name());
            read_cases(ctx, kind, n, seed, &tag);
            push_cases(ctx, kind, n, seed, &tag);
        }
    }
    let sizes = if quick { &sizes[1..] } else { &sizes[..] };
    widen_and_retag(ctx, sizes);
    numincr(ctx, sizes);
}

fn read_cases(ctx: &mut Ctx, kind: Kind, n: usize, seed: u64, tag: &str) {
    let name = format!("parse/{tag}");
    if wants(ctx, &name) {
        let text = kind.text(n, &mut Rng(seed));
        same(&name, &kind.parse(&text), &serde_parse(&text));
        let work = Work::Bytes(text.len());
        ctx.bench(row(&name, "ijson", work), || kind.parse(black_box(&text)));
        ctx.bench(row(&name, "serde", work), || serde_parse(black_box(&text)));
    }

    const READS: usize = 64;
    let name = format!("get/{tag}");
    if wants(ctx, &name) {
        let (a, b) = kind.base(n, seed);
        let (arr, vec) = (a.as_array().unwrap(), b.as_array().unwrap());
        let mut rng = Rng(seed ^ 0x6e7);
        let idx: Vec<usize> = (0..READS).map(|_| (rng.next() % n as u64) as usize).collect();
        let gi = || idx.iter().map(|&i| get_index(arr, black_box(i)).unwrap()).sum::<f64>();
        let gs = || idx.iter().map(|&i| serde_f64(vec.get(black_box(i)).unwrap())).sum::<f64>();
        assert!(gi() == gs(), "typed_array/{name}: element reads differ");
        ctx.bench(row(&name, "ijson", Work::Items(READS)), gi);
        ctx.bench(row(&name, "serde", Work::Items(READS)), gs);
    }

    let name = format!("sum/{tag}");
    if wants(ctx, &name) {
        let (a, b) = kind.base(n, seed);
        let (arr, vec) = (a.as_array().unwrap(), b.as_array().unwrap());
        let si = || black_box(arr).iter().map(|item| item.as_number().unwrap().to_f64_lossy()).sum::<f64>();
        let sl = || sum_slice(black_box(arr));
        let ss = || black_box(vec).iter().map(serde_f64).sum::<f64>();
        assert!(si() == ss() && sl() == ss(), "typed_array/{name}: sums differ");
        ctx.bench(row(&name, "ijson", Work::Items(n)), si);
        ctx.bench(row(&name, "ijson-slice", Work::Items(n)), sl);
        ctx.bench(row(&name, "serde", Work::Items(n)), ss);
    }
}

fn push_cases(ctx: &mut Ctx, kind: Kind, n: usize, seed: u64, tag: &str) {
    // Push is O(1) once there is room, whatever the length: skip the large size.
    let name = format!("push-same/{tag}");
    if n <= 1024 && wants(ctx, &name) {
        let k = (n / 4).max(8);
        let (a, b) = kind.base(n, seed);
        let (va, vb) = kind.values(k, seed);
        case(
            ctx,
            &name,
            k,
            || {
                let mut doc = a.clone();
                doc.as_array_mut().unwrap().reserve(k).expect("reserve");
                (doc, va.clone())
            },
            |(doc, vals)| vals.iter_mut().map(|v| push_i(doc, take(v))).last(),
            || {
                let mut doc = b.clone();
                doc.as_array_mut().unwrap().reserve(k);
                (doc, vb.clone())
            },
            |(doc, vals)| vals.iter_mut().map(|v| push_s(doc, take(v))).last(),
        );
        // The array must still be in its typed form afterwards.
        let mut doc = a.clone();
        for v in va.iter().cloned() {
            push_i(&mut doc, v);
        }
        assert!(
            doc.as_array().unwrap().as_slice().type_tag() == kind.tag(),
            "typed_array/{name}: same-type push changed the tag"
        );
    }

    // JSON.ARRAPPEND with many arguments.
    let name = format!("extend/{tag}");
    if wants(ctx, &name) {
        let m = n.min(1024);
        let (a, b) = kind.base(n, seed);
        let (va, vb) = kind.values(m, seed);
        case(
            ctx,
            &format!("{name}+{m}"),
            m,
            || (a.clone(), va.clone()),
            |(doc, args)| {
                let arr = doc.as_array_mut().expect("array");
                arr.try_extend(take(args)).expect("extend");
                arr.len()
            },
            || (b.clone(), vb.clone()),
            |(doc, args)| {
                let arr = doc.as_array_mut().expect("array");
                arr.extend(take(args));
                arr.len()
            },
        );
    }
}

/// One push that the current representation cannot hold.
fn widen_and_retag(ctx: &mut Ctx, sizes: &[usize]) {
    // (case, starting kind, pushed value as JSON, tag afterwards, cleared first)
    let cases: [(&str, Kind, &str, ArrayTag, bool); 9] = [
        ("push-widen/i8->i16", Kind::I8, "1000", ArrayTag::I16, false),
        ("push-widen/i8->i64", Kind::I8, "1099511627776", ArrayTag::I64, false),
        ("push-widen/i32->i64", Kind::I32, "1099511627776", ArrayTag::I64, false),
        ("push-widen/f32->f64", Kind::F32, "0.1", ArrayTag::F64, false),
        ("push-retag-str/i8", Kind::I8, "\"s\"", ArrayTag::Heterogeneous, false),
        ("push-retag-str/i64", Kind::I64, "\"s\"", ArrayTag::Heterogeneous, false),
        ("push-retag-str/f64", Kind::F64, "\"s\"", ArrayTag::Heterogeneous, false),
        ("push-retag-float-into-int/i32", Kind::I32, "1.5", ArrayTag::Heterogeneous, false),
        ("push-retag-emptied/i32", Kind::I32, "\"s\"", ArrayTag::Heterogeneous, true),
    ];
    for (what, kind, pushed, after, cleared) in cases {
        for &n in sizes {
            let name = format!("{what}/{n}");
            if !wants(ctx, &name) {
                continue;
            }
            let seed = 0x7a00_0000 + ((kind as u64) << 24) + n as u64;
            let (mut a, mut b) = kind.base(n, seed);
            if cleared {
                // JSON.CLEAR, then the append is what is timed.
                a.as_array_mut().unwrap().clear();
                b.as_array_mut().unwrap().clear();
            }
            let (pa, pb) = (ijson_parse(pushed), serde_parse(pushed));
            {
                let mut doc = a.clone();
                push_i(&mut doc, pa.clone());
                let tag = doc.as_array().unwrap().as_slice().type_tag();
                assert!(tag == after, "typed_array/{name}: tag after the push is {tag:?}, expected {after:?}");
            }
            case(
                ctx,
                &name,
                1,
                || (a.clone(), pa.clone()),
                |(doc, v)| push_i(doc, take(v)),
                || (b.clone(), pb.clone()),
                |(doc, v)| push_s(doc, take(v)),
            );
        }
    }
}

/// `JSON.NUMINCRBY` on an element of a typed array.
fn numincr(ctx: &mut Ctx, sizes: &[usize]) {
    for &n in sizes {
        // Enough increments per call that cloning the array in the setup
        // does not dominate the run.
        let k = (n / 2).max(64);
        let name = format!("numincr-inplace/i32/{n}");
        if wants(ctx, &name) {
            let seed = 0x7a00_0000 + ((Kind::I32 as u64) << 24) + n as u64;
            let (a, b) = Kind::I32.base(n, seed);
            let mut rng = Rng(seed ^ 0x1c4);
            let idx: Vec<usize> = (0..k).map(|_| (rng.next() % n as u64) as usize).collect();
            case(
                ctx,
                &name,
                k,
                || (a.clone(), ()),
                |(doc, _)| {
                    let arr = doc.as_array_mut().expect("array");
                    for &i in &idx {
                        let current = arr.as_mut_slice_of::<i32>().unwrap().get_mut(black_box(i)).unwrap();
                        let result = (*current as i128).checked_add(black_box(1)).unwrap();
                        black_box(i64::try_from(result).unwrap());
                        // Values are far from i32::MAX, so the result always fits.
                        *current = i32::try_from(result).expect("fits i32");
                    }
                },
                || (b.clone(), ()),
                |(doc, _)| {
                    let arr = doc.as_array_mut().expect("array");
                    for &i in &idx {
                        let v = arr.get_mut(black_box(i)).unwrap();
                        *v = Value::from(v.as_i64().unwrap().checked_add(black_box(1)).unwrap());
                    }
                },
            );
        }

        // The result leaves the i8 range: RedisJSON removes the element and
        // inserts the wider value, which promotes the whole array.
        let name = format!("numincr-overflow/i8->i16/{n}");
        if wants(ctx, &name) {
            let seed = 0x7a00_0000 + ((Kind::I8 as u64) << 24) + n as u64;
            let (a, b) = Kind::I8.base(n, seed);
            let index = n / 2;
            case(
                ctx,
                &name,
                1,
                || (a.clone(), ()),
                |(doc, _)| {
                    let arr = doc.as_array_mut().expect("array");
                    let index = black_box(index);
                    let current = arr.as_mut_slice_of::<i8>().unwrap().get_mut(index).unwrap();
                    let result = (*current as i128).checked_add(black_box(1000)).unwrap();
                    let ret = i64::try_from(result).unwrap();
                    if let Ok(fit) = i8::try_from(result) {
                        *current = fit;
                    } else {
                        arr.remove(index);
                        arr.insert(index, ret).expect("insert");
                    }
                    ret
                },
                || (b.clone(), ()),
                |(doc, _)| {
                    let v = doc.as_array_mut().expect("array").get_mut(black_box(index)).unwrap();
                    *v = Value::from(v.as_i64().unwrap().checked_add(black_box(1000)).unwrap());
                },
            );
        }
    }
}
