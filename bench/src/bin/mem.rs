//! Memory binary: heap cost of parse and clone, `ijson` against `serde_json::Value`.
//!
//!   cargo run --release --bin mem -- [--filter SUBSTR] [--fixtures DIR] [--csv PATH]

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use json_bench::counting_alloc::{measure, Counting};
use json_bench::meta::{self, RunInfo};
use json_bench::*;

#[global_allocator]
static ALLOC: Counting = Counting;

fn line(
    csv: &mut Option<fs::File>,
    fixture: &str,
    imp: &str,
    op: &str,
    bytes: usize,
    u: json_bench::counting_alloc::Usage,
) {
    if let Some(f) = csv.as_mut() {
        writeln!(f, "{fixture},{imp},{op},{bytes},{},{},{}", u.allocs, u.peak_bytes, u.retained_bytes).unwrap();
    }
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
    let mut csv_path: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--filter" => filter = args.next(),
            "--csv" => csv_path = args.next().map(PathBuf::from),
            "--fixtures" => dir = args.next().map(PathBuf::from).expect("--fixtures DIR"),
            other => panic!("unknown argument: {other}"),
        }
    }
    let mut csv = csv_path.as_deref().map(|p| {
        if let Some(d) = p.parent() {
            fs::create_dir_all(d).ok();
        }
        let mut f = fs::File::create(p).expect("create csv");
        writeln!(f, "fixture,impl,op,input_bytes,allocs,peak_bytes,retained_bytes").unwrap();
        let info = RunInfo {
            tool: "mem",
            allocator: "system, wrapped by the counting allocator",
            params: None,
            counters: None,
            overhead: None,
            pinned: None,
        };
        let run_meta = meta::collect(&info);
        eprintln!("run: {}", meta::brief(&run_meta));
        match meta::write_sidecar(p, &run_meta) {
            Ok(path) => eprintln!("metadata: {}", path.display()),
            Err(e) => eprintln!("WARNING: could not write metadata: {e}"),
        }
        f
    });
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
        line(&mut csv, &fx.name, "ijson", "parse", bytes, u);
        let (ic, u) = measure(|| iv.clone());
        line(&mut csv, &fx.name, "ijson", "clone", bytes, u);
        drop((iv, ic));

        let (sv, u) = measure(|| serde_parse(&fx.text));
        line(&mut csv, &fx.name, "serde", "parse", bytes, u);
        let (sc, u) = measure(|| sv.clone());
        line(&mut csv, &fx.name, "serde", "clone", bytes, u);
        drop((sv, sc));
    }
}
