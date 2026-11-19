//! Timing binary.
//!
//!   cargo run --release --bin time -- [--quick] [--group NAME]... [--filter SUBSTR]
//!                                     [--case SUBSTR] [--csv PATH] [--fixtures DIR] [--list]
//!                                     [--pin CPU] [--no-counters]

use std::path::PathBuf;
use std::process::ExitCode;

use json_bench::meta::{self, Overhead, RunInfo};
use json_bench::*;

fn main() -> ExitCode {
    let mut params = Params::default();
    let mut filter: Option<String> = None;
    let mut case_filter: Option<String> = None;
    let mut groups: Vec<String> = Vec::new();
    let mut csv: Option<PathBuf> = None;
    let mut dir = default_fixture_dir();
    let mut pin: Option<usize> = None;
    let mut use_counters = true;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--quick" => params = Params::quick(),
            "--filter" => filter = args.next(),
            "--case" => case_filter = args.next(),
            "--group" => groups.extend(args.next()),
            "--csv" => csv = args.next().map(PathBuf::from),
            "--fixtures" => dir = args.next().map(PathBuf::from).expect("--fixtures DIR"),
            "--no-counters" => use_counters = false,
            "--pin" => match args.next().and_then(|v| v.parse().ok()) {
                Some(cpu) => pin = Some(cpu),
                None => {
                    eprintln!("--pin needs a CPU number");
                    return ExitCode::from(2);
                }
            },
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

    // Pin before anything is loaded or opened, so memory is first touched and
    // counters are opened on the CPU the benchmarks will run on.
    if let Some(cpu) = pin {
        if let Err(e) = meta::pin(cpu) {
            eprintln!("cannot pin: {e}");
            return ExitCode::from(2);
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


    let status = if use_counters { counters::init() } else { counters::Status::off("disabled by --no-counters") };
    if status.available {
        eprintln!("counters: ON, {}", status.detail);
    } else {
        eprintln!("counters: OFF, wall-clock only: {}", status.detail);
    }

    let overhead = Overhead::from_samples(&harness::overhead());
    eprintln!(
        "harness overhead per sample (empty routine, not subtracted): min {:.0} ns, median {:.0} ns, {} instructions",
        overhead.ns_min,
        overhead.ns_median,
        overhead.instructions_median.map_or_else(|| "-".to_string(), |x| format!("{x:.0}")),
    );

    let info = RunInfo {
        tool: "time",
        allocator: "system",
        params: Some(params),
        counters: Some(&status),
        overhead: Some(overhead),
        pinned: pin,
    };
    let run_meta = meta::collect(&info);
    eprintln!("run: {}", meta::brief(&run_meta));
    for w in meta::warnings(&run_meta) {
        eprintln!("WARNING: {w}; results are not reportable");
    }
    if let Some(p) = csv.as_deref() {
        match meta::write_sidecar(p, &run_meta) {
            Ok(path) => eprintln!("metadata: {}", path.display()),
            Err(e) => eprintln!("WARNING: could not write metadata: {e}"),
        }
    }

    let mut report = Report::new(csv.as_deref());
    let mut ctx = Ctx { fixtures: &fixtures, params, report: &mut report, case_filter };
    for (name, run) in groups::ALL {
        if groups.is_empty() || groups.iter().any(|g| g == name) {
            run(&mut ctx);
        }
    }
    ExitCode::SUCCESS
}
