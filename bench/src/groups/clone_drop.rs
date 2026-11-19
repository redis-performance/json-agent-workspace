//! Clone and drop of whole documents, subtrees and scalars.
//!
//! What `ijson`'s clone shares (read from `ijson/src/value.rs`, `object.rs`,
//! `array.rs`, `unsafe_string.rs`, `number.rs`; the bench builds `ijson`
//! without `thread_safe`, as RedisJSON does):
//! - objects and arrays are deep copies: a new table or buffer is allocated
//!   and every child is cloned (typed numeric arrays are copied as one block);
//! - strings longer than 7 bytes are interned and reference counted: a clone
//!   bumps the count and copies the pointer; shorter strings and the empty
//!   string live in the handle and are copied bitwise;
//! - numbers are either inline in the handle (copied bitwise) or a boxed
//!   `i64`/`u64`/`f64`, for which a clone allocates a new box.
//!
//! So a clone of a document is an independent tree whose drop frees every
//! container and boxed number, but only decrements string counts. A string is
//! freed, and removed from the intern cache, only by its last owner.
//!
//! Cases, per fixture, for `ijson` and `serde`:
//! - `<fixture>/clone`: `value.clone()` of the whole document. Mirrors
//!   `ivalue_manager.rs`, `clone_value` (called from `auto_create.rs` when a
//!   new value is placed) and `commands.rs`, `apply_updates` and
//!   `json_merge_command_impl`, which clone the parsed argument once per
//!   target path (`set_value(path, value.clone())`, `dict_add(..)`,
//!   `merge_value(path, val.clone())`). Timed: the clone. Not timed: dropping
//!   the clone.
//! - `<fixture>/drop-clone`: drop of a clone while the original is alive, the
//!   shape of overwriting or deleting a value whose strings other documents
//!   also hold. Setup (not timed) clones into an `Option`; timed: `take()` and
//!   drop. What is left to drop outside the clock is a `None`. For `ijson` no
//!   string is freed here.
//! - `<fixture>/drop-parsed`: drop of a value parsed in setup, with no other
//!   copy of the document held by this group, the shape of `DEL` on a key.
//!   Timed: `take()` and drop. Not timed: the parse. For `ijson` the strings
//!   are freed and leave the intern cache only when their last owner goes: the
//!   harness builds a batch of states before starting the clock, so within a
//!   batch only the last drop releases strings. For large fixtures the batch
//!   is one state and every timed drop is a last-owner drop; for small
//!   fixtures the string release is amortised over the batch.
//! - `<fixture>/clone-subtree`: clone of the first array or object child of
//!   the root (skipped when the root has none): the same call sites as
//!   `clone`, with a value taken out of a larger document.
//!
//! Scalar cases (fixture independent, built here): `scalar/string-heap`
//! (interned, count bump), `scalar/string-inline` (7 bytes or fewer),
//! `scalar/number-inline`, `scalar/number-i64` and `scalar/number-f64`. The
//! number clones mirror `ivalue_manager.rs`, the numeric-op macro behind
//! `JSON.NUMINCRBY` (`*v = IValue::from(new_val.clone())`). Timed: the clone.
//!
//! Every case is checked once before timing: the clone equals the original,
//! and the `serde` value serialises to the same text as the `ijson` one.

use std::hint::black_box;

use ijson::IValue;

use crate::{ijson_parse, ijson_serialize, serde_parse, Ctx, Row, Work};

const GROUP: &str = "clone_drop";

/// The first array or object directly under the root.
fn first_container_child(root: &IValue) -> Option<&IValue> {
    let is_container = |v: &&IValue| v.is_array() || v.is_object();
    if let Some(o) = root.as_object() {
        o.iter().map(|(_, v)| v).find(is_container)
    } else {
        root.get(0).filter(is_container)
    }
}

fn check_clone(case: &str, iv: &IValue, sv: &serde_json::Value) {
    let ic = iv.clone();
    assert!(ic == *iv, "{GROUP} {case}: ijson clone differs from the original");
    let text = ijson_serialize(iv);
    assert!(ijson_serialize(&ic) == text, "{GROUP} {case}: ijson clone serialises differently");
    let sc = sv.clone();
    assert!(sc == *sv, "{GROUP} {case}: serde clone differs from the original");
    let back: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{GROUP} {case}: ijson text does not parse: {e}"));
    assert!(back == *sv, "{GROUP} {case}: ijson and serde values differ");
}

/// Time `clone` of one value for both implementations.
fn bench_clone(ctx: &mut Ctx, case: String, work: Work, iv: &IValue) {
    let row = |imp| Row { group: GROUP, case: case.clone(), imp, work };
    if !ctx.wants(&row("ijson")) {
        return;
    }
    let sv = serde_parse(&ijson_serialize(iv));
    check_clone(&case, iv, &sv);
    ctx.bench(row("ijson"), || black_box(iv).clone());
    ctx.bench(row("serde"), || black_box(&sv).clone());
}

pub fn run(ctx: &mut Ctx) {
    let fixtures = ctx.fixtures;
    for fx in fixtures {
        let bytes = Work::Bytes(fx.text.len());
        let row = |case: &str, imp| Row { group: GROUP, case: format!("{}/{case}", fx.name), imp, work: bytes };

        // Whole document: clone, and drop of a clone with the original alive.
        if ctx.wants(&row("clone", "ijson")) || ctx.wants(&row("drop-clone", "ijson")) {
            let iv = ijson_parse(&fx.text);
            let sv = serde_parse(&fx.text);
            check_clone(&fx.name, &iv, &sv);
            ctx.bench(row("clone", "ijson"), || black_box(&iv).clone());
            ctx.bench(row("clone", "serde"), || black_box(&sv).clone());
            ctx.bench_with_setup(row("drop-clone", "ijson"), || Some(iv.clone()), |s| drop(s.take()));
            ctx.bench_with_setup(row("drop-clone", "serde"), || Some(sv.clone()), |s| drop(s.take()));
        }

        // Whole document parsed in setup. No other copy is alive in this scope,
        // so the interned strings belong to the states of the batch alone.
        if ctx.wants(&row("drop-parsed", "ijson")) {
            assert!(
                ijson_parse(&fx.text) == ijson_parse(&fx.text),
                "{GROUP} {}: two parses of the fixture differ",
                fx.name
            );
            ctx.bench_with_setup(row("drop-parsed", "ijson"), || Some(ijson_parse(&fx.text)), |s| drop(s.take()));
            ctx.bench_with_setup(row("drop-parsed", "serde"), || Some(serde_parse(&fx.text)), |s| drop(s.take()));
        }

        // Subtree taken out of the document.
        if ctx.wants(&row("clone-subtree", "ijson")) {
            let iv = ijson_parse(&fx.text);
            if let Some(sub) = first_container_child(&iv) {
                let len = ijson_serialize(sub).len();
                bench_clone(ctx, format!("{}/clone-subtree", fx.name), Work::Bytes(len), sub);
            }
        }
    }

    // Scalars. `Items(1)`: one value per clone.
    let scalars: [(&str, IValue); 5] = [
        ("string-heap", IValue::from("a string of more than seven bytes")),
        ("string-inline", IValue::from("short")),
        ("number-inline", IValue::from(42)),
        ("number-i64", IValue::from(i64::MAX - 1)),
        ("number-f64", IValue::from(0.1234567890123_f64)),
    ];
    for (name, v) in &scalars {
        bench_clone(ctx, format!("scalar/{name}"), Work::Items(1), v);
    }
}
