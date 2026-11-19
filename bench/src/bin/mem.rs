//! Memory binary: heap cost of parse and clone, `ijson` against `serde_json::Value`.
//!
//!   cargo run --release --bin mem -- [--filter SUBSTR] [--fixtures DIR]

use std::path::PathBuf;

use json_bench::counting_alloc::{measure, Counting};
use json_bench::*;

#[global_allocator]
static ALLOC: Counting = Counting;

fn line(fixture: &str, imp: &str, op: &str, bytes: usize, u: json_bench::counting_alloc::Usage) {
    println!(
        "{:<38} {:<6} {:<6} {:>8} {:>8} {:>11} {:>11} {:>7.2}",
        fixture,
        imp,
        op,
        bytes,
        u.allocs,
        u.peak_bytes,
        u.retained_bytes,
        u.retained_bytes as f64 / bytes as f64
    );
}

fn main() {
    let mut filter: Option<String> = None;
    let mut dir = default_fixture_dir();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--filter" => filter = args.next(),
            "--fixtures" => dir = args.next().map(PathBuf::from).expect("--fixtures DIR"),
            other => panic!("unknown argument: {other}"),
        }
    }
    println!(
        "{:<38} {:<6} {:<6} {:>8} {:>8} {:>11} {:>11} {:>7}",
        "fixture", "impl", "op", "bytes", "allocs", "peak B", "retained B", "ret/in"
    );
    for fx in load_fixtures(&dir) {
        if filter.as_ref().is_some_and(|s| !fx.name.contains(s.as_str())) {
            continue;
        }
        let bytes = fx.text.len();

        let (iv, u) = measure(|| ijson_parse(&fx.text));
        line(&fx.name, "ijson", "parse", bytes, u);
        let (ic, u) = measure(|| iv.clone());
        line(&fx.name, "ijson", "clone", bytes, u);
        drop((iv, ic));

        let (sv, u) = measure(|| serde_parse(&fx.text));
        line(&fx.name, "serde", "parse", bytes, u);
        let (sc, u) = measure(|| sv.clone());
        line(&fx.name, "serde", "clone", bytes, u);
        drop((sv, sc));
    }
}
