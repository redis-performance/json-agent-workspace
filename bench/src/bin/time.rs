//! Timing binary.
//!
//!   cargo run --release --bin time -- [--quick] [--group NAME]... [--filter SUBSTR]
//!                                     [--case SUBSTR] [--csv PATH] [--fixtures DIR] [--list]

use std::path::PathBuf;
use std::process::ExitCode;

use json_bench::*;

fn main() -> ExitCode {
    let mut params = Params::default();
    let mut filter: Option<String> = None;
    let mut case_filter: Option<String> = None;
    let mut groups: Vec<String> = Vec::new();
    let mut csv: Option<PathBuf> = None;
    let mut dir = default_fixture_dir();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--quick" => params = Params::quick(),
            "--filter" => filter = args.next(),
            "--case" => case_filter = args.next(),
            "--group" => groups.extend(args.next()),
            "--csv" => csv = args.next().map(PathBuf::from),
            "--fixtures" => dir = args.next().map(PathBuf::from).expect("--fixtures DIR"),
            "--list" => {
                for (name, _) in groups::ALL {
                    println!("{name}");
                }
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unknown argument: {other}");
                return ExitCode::from(2);
            }
        }
    }
    if let Some(bad) = groups.iter().find(|g| !groups::ALL.iter().any(|(n, _)| n == g)) {
        eprintln!("unknown group: {bad} (see --list)");
        return ExitCode::from(2);
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
    let mut ctx = Ctx { fixtures: &fixtures, params, report: &mut report, case_filter };
    for (name, run) in groups::ALL {
        if groups.is_empty() || groups.iter().any(|g| g == name) {
            run(&mut ctx);
        }
    }
    ExitCode::SUCCESS
}
