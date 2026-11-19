//! Benchmark groups, one module per RedisJSON call path
//! (`docs/USAGE-PATTERN.md`). Each exposes `pub fn run(ctx: &mut Ctx)`.

use crate::Ctx;

pub mod cbor;
pub mod clone_drop;
pub mod lookup;
pub mod mutate;
pub mod parse;
pub mod serialize;
pub mod typed_array;

pub type GroupFn = fn(&mut Ctx);

/// Every group, in run order.
pub const ALL: &[(&str, GroupFn)] = &[
    ("parse", parse::run),
    ("serialize", serialize::run),
    ("lookup", lookup::run),
    ("mutate", mutate::run),
    ("typed_array", typed_array::run),
    ("clone_drop", clone_drop::run),
    ("cbor", cbor::run),
];
