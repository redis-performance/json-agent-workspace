//! Microbenchmark referee for `ijson`, mirroring RedisJSON's call paths.
//! See `bench/README.md`.

pub mod counters;
pub mod counting_alloc;
pub mod ctx;
pub mod fixtures;
pub mod groups;
pub mod harness;
pub mod meta;
pub mod ops;
pub mod report;
pub mod synth;

pub use ctx::Ctx;
pub use fixtures::{default_fixture_dir, load_fixtures, verify, Fixture};
pub use harness::{Params, Samples};
pub use ops::*;
pub use report::{Report, Row, Work};
