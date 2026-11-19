//! Timing binary: parse and serialise, `ijson` against `serde_json::Value`.
//!
//!   cargo run --release --bin time -- [--quick] [--filter SUBSTR] [--csv PATH] [--fixtures DIR]

use std::path::PathBuf;
use std::process::ExitCode;

use json_bench::*;

fn main() -> ExitCode {
    let mut params = Params::default();
    let mut filter: Option<String> = None;
    let mut csv: Option<PathBuf> = None;
    let mut dir = default_fixture_dir();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--quick" => params = Params::quick(),
            "--filter" => filter = args.next(),
            "--csv" => csv = args.next().map(PathBuf::from),
            "--fixtures" => dir = args.next().map(PathBuf::from).expect("--fixtures DIR"),
            other => {
                eprintln!("unknown argument: {other}");
                return ExitCode::from(2);
            }
        }
    }

    let fixtures: Vec<Fixture> = load_fixtures(&dir)
        .into_iter()
        .filter(|f| filter.as_ref().is_none_or(|s| f.name.contains(s.as_str())))
        .collect();
    if fixtures.is_empty() {
        eprintln!("no fixtures matched in {}", dir.display());
        return ExitCode::from(2);
    }

    // Nothing is timed until every fixture has been verified.
    let mut failed = false;
    for fx in &fixtures {
        if let Err(e) = verify(fx) {
            eprintln!("VERIFY FAILED {}: {e}", fx.name);
            failed = true;
        }
    }
    if failed {
        return ExitCode::FAILURE;
    }
    eprintln!("verified {} fixtures; params: {params:?}", fixtures.len());

    let mut report = Report::new(csv.as_deref());
    for fx in &fixtures {
        let bytes = fx.text.len();
        let row = |group: &'static str, imp: &'static str| Row { group, fixture: &fx.name, imp, bytes };

        report.record(&row("parse", "ijson"), &bench(|| ijson_parse(&fx.text), params));
        report.record(&row("parse", "serde"), &bench(|| serde_parse(&fx.text), params));

        let iv = ijson_parse(&fx.text);
        let sv = serde_parse(&fx.text);
        report.record(&row("serialize", "ijson"), &bench(|| ijson_serialize(&iv), params));
        report.record(&row("serialize", "serde"), &bench(|| serde_serialize(&sv), params));
    }
    ExitCode::SUCCESS
}
