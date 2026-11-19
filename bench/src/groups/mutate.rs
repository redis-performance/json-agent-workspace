//! In-place mutation of a stored document, as RedisJSON's write holder does it.
//!
//! Every case mirrors a function of `IValueKeyHolderWrite` in
//! `RedisJSON/redis_json/src/ivalue_manager.rs` and uses the same `ijson`
//! calls with the same argument shapes. The path walk is `follow_path`
//! (`destructure_mut` + `IObject::get_mut`); it is only part of the timed
//! region where a case says so.
//!
//! | case | RedisJSON call site |
//! |---|---|
//! | `obj-insert-new` | `dict_add`: `as_object_mut`, `contains_key(&str)`, `insert(key.to_string(), v.take())` (`JSON.SET` of a new key; also what `auto_create.rs` ends in) |
//! | `obj-overwrite` | `set_value` -> `update`/`follow_path`: `destructure_mut`, `obj.get_mut(&str)`, `*val = v.take()` |
//! | `obj-remove` | `delete_path` -> `remove`: `destructure_mut`, `obj.remove(&str)` (`JSON.DEL`) |
//! | `obj-clear`, `arr-clear` | `clear`: `destructure_mut`, `obj.clear()` / `arr.clear()` (`JSON.CLEAR`) |
//! | `arr-push`, `arr-push-first-after-clone` | `arr_append`: `as_array_mut`, `arr.try_extend(args)` with one argument (`JSON.ARRAPPEND`) |
//! | `arr-insert-front`, `arr-insert-mid` | `arr_insert_with_depth`: `try_extend(args.iter().cloned())`, then `as_mut_slice()` and `rotate_right` (`JSON.ARRINSERT`) |
//! | `arr-pop-back`, `arr-remove-front` | `arr_pop`: `array.remove(index)` (`JSON.ARRPOP` with index -1 and 0) |
//! | `arr-trim` | `arr_trim`: `as_mut_slice()`, `rotate_left(start)`, `truncate` (`JSON.ARRTRIM`) |
//! | `num-incr-*` | `do_num_op`, `PathValue::IValue` arm, with `number.rs` `number_op_result` and the `SelectValue for IValue` accessors in `json_path/src/json_node.rs` (`get_type`, `get_long`, `get_double`): read an `INumber`, build a new one, `*v = IValue::from(new_val.clone())` (`JSON.NUMINCRBY`) |
//! | `str-append` | `str_append`: `as_string_mut`, `[old, suffix].concat()`, `*v_str = IString::intern(&new_str)` (`JSON.STRAPPEND`) |
//! | `set-nested` | `set_value` on path `$.a.b`: `follow_path` walk is timed, then `*val = v.take()`; the old subtree is dropped there |
//! | `build-obj-insert` | repeated `dict_add` on an empty `IObject` (`auto_create.rs` builds missing parents from `IObject::new()` + `insert`) |
//! | `build-arr-append` | `JSON.ARRAPPEND` in a loop on an empty array |
//!
//! Conventions:
//! - `units` is the number of operations in one timed call (K). Cheap
//!   operations are batched so that cloning the starting document in the
//!   untimed setup does not dominate wall time. A batch of inserts or removes
//!   changes the container size by K (at most an eighth or a quarter of it).
//! - Values to insert are built in the setup: RedisJSON parses them before it
//!   takes the write holder. Keys are `&str` and are turned into a `String`
//!   and interned inside the timed region, because `dict_add` does that.
//! - The value an operation replaces or removes is dropped inside the timed
//!   region, as in RedisJSON.
//! - Starting documents are clones of a parsed document. A clone may have a
//!   tighter capacity than a document that grew in place, so the first insert
//!   or push can reallocate; `arr-push` reserves in the setup to exclude that
//!   and `arr-push-first-after-clone` measures it.
//! - `str-append`: every state starts from a string no other state holds, so
//!   the result of the append is not in the intern cache (cache miss, the
//!   usual case for an append). The cache-hit case is not measured.
//! - Before timing, each case runs once on both implementations and the
//!   results are compared.

use std::cell::Cell;
use std::hint::black_box;
use std::mem::take;

use ijson::array::{ArraySliceMut, TryExtend};
use ijson::{DestructuredMut, INumber, IString, IValue};
use serde_json::Value;

use crate::{ijson_parse, ijson_serialize, serde_parse, Ctx, Params, Row, Work};

const GROUP: &str = "mutate";

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
        (self.next() % n as u64) as usize
    }
}

fn row(case: &str, imp: &'static str, k: usize) -> Row {
    Row { group: GROUP, case: case.to_string(), imp, work: Work::Items(k) }
}

/// Run the operation once on both implementations, compare the results, then
/// time both. `vi` renders the ijson result as JSON text, `vs` gives the
/// baseline's result as a `Value`.
#[allow(clippy::too_many_arguments)]
fn case<SI, RI, SS, RS>(
    ctx: &mut Ctx,
    name: &str,
    k: usize,
    mut si: impl FnMut() -> SI,
    mut oi: impl FnMut(&mut SI) -> RI,
    vi: impl Fn(&SI, &RI) -> String,
    mut ss: impl FnMut() -> SS,
    mut os: impl FnMut(&mut SS) -> RS,
    vs: impl Fn(&SS, &RS) -> Value,
) {
    if !ctx.wants(&row(name, "ijson", k)) {
        return;
    }
    {
        let mut a = si();
        let ra = oi(&mut a);
        let mut b = ss();
        let rb = os(&mut b);
        let got = serde_parse(&vi(&a, &ra));
        let want = vs(&b, &rb);
        assert!(got == want, "mutate/{name}: ijson and serde_json results differ after the operation");
    }
    ctx.bench_with_setup(row(name, "ijson", k), &mut si, &mut oi);
    ctx.bench_with_setup(row(name, "serde", k), &mut ss, &mut os);
}

/// `{"field_0": .., ..}` with `n` keys; values are small integers and short strings.
fn object_text(n: usize, rng: &mut Rng) -> String {
    let mut s = String::from("{");
    for i in 0..n {
        if i > 0 {
            s.push(',');
        }
        if i % 4 == 3 {
            s.push_str(&format!("\"field_{i}\":\"value {}\"", rng.below(1000)));
        } else {
            s.push_str(&format!("\"field_{i}\":{}", rng.below(100_000)));
        }
    }
    s.push('}');
    s
}

/// A heterogeneous array of `n` scalars: mostly integers, with strings,
/// booleans and nulls mixed in so that it is never stored in typed form.
fn array_text(n: usize, rng: &mut Rng) -> String {
    let mut s = String::from("[");
    for i in 0..n {
        if i > 0 {
            s.push(',');
        }
        match i % 8 {
            0 => s.push_str(&format!("\"item {}\"", rng.below(1000))),
            3 => s.push_str("true"),
            5 => s.push_str("null"),
            _ => s.push_str(&rng.below(1_000_000).to_string()),
        }
    }
    s.push(']');
    s
}

fn scalar_values(k: usize, rng: &mut Rng) -> (Vec<IValue>, Vec<Value>) {
    let mut a = Vec::with_capacity(k);
    let mut b = Vec::with_capacity(k);
    for i in 0..k {
        if i % 4 == 3 {
            let s = format!("new value {}", rng.below(1000));
            a.push(IValue::from(s.as_str()));
            b.push(Value::from(s));
        } else {
            let n = rng.below(100_000) as i64;
            a.push(IValue::from(n));
            b.push(Value::from(n));
        }
    }
    (a, b)
}

fn doc_i<A, R>(s: &(IValue, A), _: &R) -> String {
    ijson_serialize(&s.0)
}
fn doc_s<A, R>(s: &(Value, A), _: &R) -> Value {
    s.0.clone()
}

/// `follow_path` in `ivalue_manager.rs`, for object-only paths.
fn follow<'a>(root: &'a mut IValue, path: &[String]) -> &'a mut IValue {
    path.iter().fold(root, |target, token| match target.destructure_mut() {
        DestructuredMut::Object(obj) => obj.get_mut(token.as_str()).expect("path exists"),
        _ => panic!("path walks through a non-object"),
    })
}

fn follow_serde<'a>(root: &'a mut Value, path: &[String]) -> &'a mut Value {
    path.iter().fold(root, |target, token| match target {
        Value::Object(obj) => obj.get_mut(token.as_str()).expect("path exists"),
        _ => panic!("path walks through a non-object"),
    })
}

/// `do_num_op` on a plain value: `number_op_result` followed by the write.
#[inline]
fn ijson_numincr(v: &mut IValue, operand: &Value) {
    let n = v.as_number().expect("number");
    // `get_type`: Long unless it has a decimal point or does not fit i64.
    let is_long = !(n.has_decimal_point() | n.to_i64().is_none());
    let new_val: INumber = match (is_long, operand.as_i64()) {
        (true, Some(num2)) => {
            let num1 = n.to_i64().expect("long");
            (num1 as i128)
                .checked_add(num2 as i128)
                .and_then(|r| i64::try_from(r).ok())
                .expect("no overflow")
                .into()
        }
        _ => {
            let num1 = n.to_f64_lossy();
            let num2 = operand.as_f64().expect("operand is a number");
            INumber::try_from(num1 + num2).unwrap_or_else(|_| panic!("not a number"))
        }
    };
    *v = IValue::from(new_val.clone());
    black_box(new_val);
}

#[inline]
fn serde_numincr(v: &mut Value, operand: &Value) {
    let Value::Number(n) = &*v else { panic!("number") };
    let new_val = match (n.is_i64(), operand.as_i64()) {
        (true, Some(num2)) => serde_json::Number::from(
            (n.as_i64().unwrap() as i128)
                .checked_add(num2 as i128)
                .and_then(|r| i64::try_from(r).ok())
                .expect("no overflow"),
        ),
        _ => serde_json::Number::from_f64(n.as_f64().unwrap() + operand.as_f64().unwrap()).expect("finite"),
    };
    *v = Value::Number(new_val);
}

pub fn run(ctx: &mut Ctx) {
    let quick = ctx.params.min_time_ns < Params::default().min_time_ns;
    objects(ctx);
    arrays(ctx, quick);
    numbers(ctx);
    strings(ctx);
    nested(ctx);
    build(ctx);
}

fn objects(ctx: &mut Ctx) {
    for n in [4usize, 64, 1024] {
        let mut rng = Rng(0x0b1e_c700 + n as u64);
        let text = object_text(n, &mut rng);
        let (base_i, base_s) = (ijson_parse(&text), serde_parse(&text));
        let k = (n / 8).max(1);
        let (vals_i, vals_s) = scalar_values(k, &mut rng);

        // dict_add: keys that are not present.
        let new_keys: Vec<String> = (0..k).map(|i| format!("added_{i}")).collect();
        case(
            ctx,
            &format!("obj-insert-new/{n}keys"),
            k,
            || (base_i.clone(), vals_i.clone()),
            |(doc, vals)| {
                let o = doc.as_object_mut().expect("object");
                for (key, v) in new_keys.iter().zip(vals.iter_mut()) {
                    let key: &str = black_box(key.as_str());
                    if !o.contains_key(key) {
                        o.insert(key.to_string(), take(v)).expect("insert");
                    }
                }
            },
            doc_i,
            || (base_s.clone(), vals_s.clone()),
            |(doc, vals)| {
                let o = doc.as_object_mut().expect("object");
                for (key, v) in new_keys.iter().zip(vals.iter_mut()) {
                    let key: &str = black_box(key.as_str());
                    if !o.contains_key(key) {
                        o.insert(key.to_string(), take(v));
                    }
                }
            },
            doc_s,
        );

        // K distinct existing keys, spread over the object.
        let mut picked: Vec<usize> = (0..n).collect();
        for i in 0..k {
            let j = i + rng.below(n - i);
            picked.swap(i, j);
        }
        let hit_keys: Vec<String> = picked[..k].iter().map(|i| format!("field_{i}")).collect();

        case(
            ctx,
            &format!("obj-overwrite/{n}keys"),
            k,
            || (base_i.clone(), vals_i.clone()),
            |(doc, vals)| {
                for (key, v) in hit_keys.iter().zip(vals.iter_mut()) {
                    let key: &str = black_box(key.as_str());
                    match doc.destructure_mut() {
                        DestructuredMut::Object(o) => *o.get_mut(key).expect("key") = take(v),
                        _ => unreachable!(),
                    }
                }
            },
            doc_i,
            || (base_s.clone(), vals_s.clone()),
            |(doc, vals)| {
                for (key, v) in hit_keys.iter().zip(vals.iter_mut()) {
                    let key: &str = black_box(key.as_str());
                    match doc {
                        Value::Object(o) => *o.get_mut(key).expect("key") = take(v),
                        _ => unreachable!(),
                    }
                }
            },
            doc_s,
        );

        case(
            ctx,
            &format!("obj-remove/{n}keys"),
            k,
            || (base_i.clone(), ()),
            |(doc, _)| {
                let mut removed = 0usize;
                for key in &hit_keys {
                    let key: &str = black_box(key.as_str());
                    if let DestructuredMut::Object(o) = doc.destructure_mut() {
                        removed += o.remove(key).is_some() as usize;
                    }
                }
                removed
            },
            doc_i,
            || (base_s.clone(), ()),
            |(doc, _)| {
                let mut removed = 0usize;
                for key in &hit_keys {
                    let key: &str = black_box(key.as_str());
                    if let Value::Object(o) = doc {
                        removed += o.remove(key).is_some() as usize;
                    }
                }
                removed
            },
            doc_s,
        );

        if n >= 64 {
            case(
                ctx,
                &format!("obj-clear/{n}keys"),
                1,
                || (base_i.clone(), ()),
                |(doc, _)| {
                    if let DestructuredMut::Object(o) = doc.destructure_mut() {
                        o.clear()
                    }
                },
                doc_i,
                || (base_s.clone(), ()),
                |(doc, _)| {
                    if let Value::Object(o) = doc {
                        o.clear()
                    }
                },
                doc_s,
            );
        }
    }
}

/// `arr_insert_with_depth` for one argument: the argument is cloned inside
/// the timed region because RedisJSON receives `&[IValue]`.
fn ijson_arr_insert(doc: &mut IValue, args: &[IValue], idx: usize) -> usize {
    let arr = doc.as_array_mut().expect("array");
    arr.try_extend(args.iter().cloned()).expect("extend");
    match arr.as_mut_slice() {
        ArraySliceMut::Heterogeneous(slice) => slice[idx..].rotate_right(args.len()),
        _ => panic!("expected a heterogeneous array"),
    }
    arr.len() as usize
}

fn serde_arr_insert(doc: &mut Value, args: &[Value], idx: usize) -> usize {
    let arr = doc.as_array_mut().expect("array");
    arr.extend(args.iter().cloned());
    arr[idx..].rotate_right(args.len());
    arr.len()
}

fn arrays(ctx: &mut Ctx, quick: bool) {
    let big = if quick { 4096 } else { 16384 };
    for n in [8usize, 1024, big] {
        let mut rng = Rng(0xa77a_0000 + n as u64);
        let text = array_text(n, &mut rng);
        let (base_i, base_s) = (ijson_parse(&text), serde_parse(&text));
        assert!(
            base_i.as_array().is_some_and(|a| a.as_slice().is_heterogeneous()),
            "mutate: array fixture must be heterogeneous"
        );

        // Push is O(1) whatever the length, so the large size is left out.
        if n <= 1024 {
            let k = (n / 4).max(1);
            let (vals_i, vals_s) = scalar_values(k, &mut rng);
            // One `try_extend` per value, as one `JSON.ARRAPPEND` with one
            // argument. `Option` stands in for the one-element `Vec` so that
            // freeing the argument vector is not timed; ijson sees the same
            // `reserve(1)` + `push`.
            case(
                ctx,
                &format!("arr-push/{n}"),
                k,
                || {
                    let mut doc = base_i.clone();
                    doc.as_array_mut().unwrap().reserve(k).expect("reserve");
                    (doc, vals_i.clone())
                },
                |(doc, vals)| {
                    let arr = doc.as_array_mut().expect("array");
                    for v in vals.iter_mut() {
                        arr.try_extend(Some(take(v))).expect("push");
                    }
                    arr.len()
                },
                doc_i,
                || {
                    let mut doc = base_s.clone();
                    doc.as_array_mut().unwrap().reserve(k);
                    (doc, vals_s.clone())
                },
                |(doc, vals)| {
                    let arr = doc.as_array_mut().expect("array");
                    for v in vals.iter_mut() {
                        arr.extend(Some(take(v)));
                    }
                    arr.len()
                },
                doc_s,
            );
        }

        let (one_i, one_s) = scalar_values(1, &mut rng);

        // No reserve: whatever capacity the clone has is what the push finds.
        case(
            ctx,
            &format!("arr-push-first-after-clone/{n}"),
            1,
            || (base_i.clone(), one_i.clone()),
            |(doc, vals)| {
                let arr = doc.as_array_mut().expect("array");
                arr.try_extend(Some(take(&mut vals[0]))).expect("push");
                arr.len()
            },
            doc_i,
            || (base_s.clone(), one_s.clone()),
            |(doc, vals)| {
                let arr = doc.as_array_mut().expect("array");
                arr.extend(Some(take(&mut vals[0])));
                arr.len()
            },
            doc_s,
        );

        for (what, idx) in [("front", 0usize), ("mid", n / 2)] {
            case(
                ctx,
                &format!("arr-insert-{what}/{n}"),
                1,
                || (base_i.clone(), ()),
                |(doc, _)| ijson_arr_insert(doc, black_box(&one_i[..]), black_box(idx)),
                doc_i,
                || (base_s.clone(), ()),
                |(doc, _)| serde_arr_insert(doc, black_box(&one_s[..]), black_box(idx)),
                doc_s,
            );
        }

        // JSON.ARRPOP key path -1, K times.
        let k = (n / 4).max(1);
        case(
            ctx,
            &format!("arr-pop-back/{n}"),
            k,
            || (base_i.clone(), ()),
            |(doc, _)| {
                let arr = doc.as_array_mut().expect("array");
                for _ in 0..k {
                    let index = arr.len() as usize - 1;
                    black_box(arr.remove(index));
                }
            },
            doc_i,
            || (base_s.clone(), ()),
            |(doc, _)| {
                let arr = doc.as_array_mut().expect("array");
                for _ in 0..k {
                    let index = arr.len() - 1;
                    black_box(arr.remove(index));
                }
            },
            doc_s,
        );

        // JSON.ARRPOP key path 0. The popped value is returned, so its drop is not timed.
        case(
            ctx,
            &format!("arr-remove-front/{n}"),
            1,
            || (base_i.clone(), ()),
            |(doc, _)| doc.as_array_mut().expect("array").remove(black_box(0)),
            doc_i,
            || (base_s.clone(), ()),
            |(doc, _)| doc.as_array_mut().expect("array").remove(black_box(0)),
            doc_s,
        );

        // JSON.ARRTRIM keeping the middle half.
        let (start, stop) = (n / 4, n / 4 + n / 2 - 1);
        case(
            ctx,
            &format!("arr-trim-mid-half/{n}"),
            1,
            || (base_i.clone(), ()),
            |(doc, _)| {
                let array = doc.as_array_mut().expect("array");
                let range = black_box(start)..(black_box(stop) + 1);
                match array.as_mut_slice() {
                    ArraySliceMut::Heterogeneous(slice) => slice[0..].rotate_left(range.start),
                    _ => panic!("expected a heterogeneous array"),
                }
                array.truncate(range.end - range.start);
                array.len() as usize
            },
            doc_i,
            || (base_s.clone(), ()),
            |(doc, _)| {
                let array = doc.as_array_mut().expect("array");
                let range = black_box(start)..(black_box(stop) + 1);
                array[0..].rotate_left(range.start);
                array.truncate(range.end - range.start);
                array.len()
            },
            doc_s,
        );

        if n >= 1024 {
            case(
                ctx,
                &format!("arr-clear/{n}"),
                1,
                || (base_i.clone(), ()),
                |(doc, _)| {
                    if let DestructuredMut::Array(a) = doc.destructure_mut() {
                        a.clear()
                    }
                },
                doc_i,
                || (base_s.clone(), ()),
                |(doc, _)| {
                    if let Value::Array(a) = doc {
                        a.clear()
                    }
                },
                doc_s,
            );
        }
    }
}

fn numbers(ctx: &mut Ctx) {
    const K: usize = 64;
    // (case, starting values, operand text as given to JSON.NUMINCRBY)
    let mut rng = Rng(0x0000_4e55);
    let small: Vec<String> = (0..K).map(|_| rng.below(1000).to_string()).collect();
    let large: Vec<String> = (0..K).map(|_| ((1u64 << 40) + rng.next() % (1 << 40)).to_string()).collect();
    let floats: Vec<String> = (0..K).map(|_| format!("{}.25", rng.below(100_000))).collect();
    let cases: [(&str, &Vec<String>, &str); 4] = [
        ("num-incr-int-small", &small, "1"),
        ("num-incr-int-large", &large, "1"),
        ("num-incr-float", &floats, "1.5"),
        // An integer plus a non-integer operand: the result is stored as a float.
        ("num-incr-int-to-float", &small, "0.5"),
    ];
    for (name, texts, operand) in cases {
        // RedisJSON parses the operand with serde_json inside `do_num_op`;
        // that is not an ijson operation, so it is done here.
        let operand: Value = serde_json::from_str(operand).unwrap();
        // Standalone values (as found at a path), not elements of one array:
        // an all-number array would be stored in typed form.
        let base_i: Vec<IValue> = texts.iter().map(|t| ijson_parse(t)).collect();
        let base_s: Vec<Value> = texts.iter().map(|t| serde_parse(t)).collect();
        case(
            ctx,
            name,
            K,
            || base_i.clone(),
            |vals| {
                let operand = black_box(&operand);
                for v in vals.iter_mut() {
                    ijson_numincr(v, operand);
                }
            },
            |vals, _| format!("[{}]", vals.iter().map(ijson_serialize).collect::<Vec<_>>().join(",")),
            || base_s.clone(),
            |vals| {
                let operand = black_box(&operand);
                for v in vals.iter_mut() {
                    serde_numincr(v, operand);
                }
            },
            |vals, _| Value::Array(vals.clone()),
        );
    }
}

fn strings(ctx: &mut Ctx) {
    let suffix = "-suffix!";
    for len in [16usize, 1024] {
        // Each state gets a string of its own (see the module comment), with
        // a fixed length. Both closures draw from their own counter, starting
        // at the same value, so the verification run sees the same string.
        let make = |counter: &Cell<u64>| {
            let id = counter.get();
            counter.set(id + 1);
            let mut s = format!("{id:012}:");
            while s.len() < len {
                s.push((b'a' + (s.len() % 26) as u8) as char);
            }
            s
        };
        let (ci, cs) = (Cell::new(0u64), Cell::new(0u64));
        case(
            ctx,
            &format!("str-append/{len}b+{}b", suffix.len()),
            1,
            || IValue::from(make(&ci).as_str()),
            |v| {
                let s: &str = black_box(suffix);
                let v_str = v.as_string_mut().expect("string");
                let new_str = [v_str.as_str(), s].concat();
                *v_str = IString::intern(&new_str);
                new_str.len()
            },
            |v, _| ijson_serialize(v),
            || Value::from(make(&cs)),
            |v| {
                let s: &str = black_box(suffix);
                let Value::String(v_str) = v else { panic!("string") };
                v_str.push_str(s);
                v_str.len()
            },
            |v, _| v.clone(),
        );
    }
}

fn nested(ctx: &mut Ctx) {
    for n in [16usize, 256] {
        let mut rng = Rng(0x5e7_0000 + n as u64);
        let old = object_text(n, &mut rng);
        let new = object_text(n, &mut rng);
        let siblings = object_text(8, &mut rng);
        let text = format!(
            "{{\"id\":7,\"name\":\"doc\",\"a\":{{\"x\":1,\"b\":{old},\"c\":[1,\"two\",3]}},\"meta\":{siblings}}}"
        );
        let (base_i, base_s) = (ijson_parse(&text), serde_parse(&text));
        let (new_i, new_s) = (ijson_parse(&new), serde_parse(&new));
        let path = vec!["a".to_string(), "b".to_string()];
        case(
            ctx,
            &format!("set-nested/$.a.b/{n}keys"),
            1,
            || (base_i.clone(), new_i.clone()),
            |(doc, v)| *follow(doc, black_box(&path)) = take(v),
            doc_i,
            || (base_s.clone(), new_s.clone()),
            |(doc, v)| *follow_serde(doc, black_box(&path)) = take(v),
            doc_s,
        );
    }
}

fn build(ctx: &mut Ctx) {
    for n in [64usize, 1024] {
        let mut rng = Rng(0xb01d_0000 + n as u64);
        let keys: Vec<String> = (0..n).map(|i| format!("field_{i}")).collect();
        let (vals_i, vals_s) = scalar_values(n, &mut rng);
        case(
            ctx,
            &format!("build-obj-insert/{n}keys"),
            n,
            || vals_i.clone(),
            |vals| {
                let mut doc = IValue::from(ijson::IObject::new());
                let o = doc.as_object_mut().expect("object");
                for (key, v) in keys.iter().zip(vals.iter_mut()) {
                    let key: &str = key.as_str();
                    if !o.contains_key(key) {
                        o.insert(key.to_string(), take(v)).expect("insert");
                    }
                }
                doc
            },
            |_, doc| ijson_serialize(doc),
            || vals_s.clone(),
            |vals| {
                let mut doc = Value::Object(serde_json::Map::new());
                let o = doc.as_object_mut().expect("object");
                for (key, v) in keys.iter().zip(vals.iter_mut()) {
                    let key: &str = key.as_str();
                    if !o.contains_key(key) {
                        o.insert(key.to_string(), take(v));
                    }
                }
                doc
            },
            |_, doc| doc.clone(),
        );

        case(
            ctx,
            &format!("build-arr-append/{n}"),
            n,
            || vals_i.clone(),
            |vals| {
                let mut doc = IValue::from(ijson::IArray::new());
                let arr = doc.as_array_mut().expect("array");
                for v in vals.iter_mut() {
                    arr.try_extend(Some(take(v))).expect("push");
                }
                doc
            },
            |_, doc| ijson_serialize(doc),
            || vals_s.clone(),
            |vals| {
                let mut doc = Value::Array(Vec::new());
                let arr = doc.as_array_mut().expect("array");
                for v in vals.iter_mut() {
                    arr.extend(Some(take(v)));
                }
                doc
            },
            |_, doc| doc.clone(),
        );
    }
}
