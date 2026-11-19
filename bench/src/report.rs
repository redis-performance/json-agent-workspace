//! Result table and per-sample CSV.

use std::fs;
use std::io::Write;
use std::path::Path;

use crate::counters::{self, BRANCH_MISSES, CACHE_MISSES, CYCLES, INSTRUCTIONS};
use crate::harness::Samples;

/// What one operation processes, used to print a throughput.
#[derive(Clone, Copy, Debug)]
pub enum Work {
    /// Bytes of JSON or CBOR text. Throughput is MB/s.
    Bytes(usize),
    /// Elements, keys or values touched. Throughput is million items/s.
    Items(usize),
}

/// Identifies one benchmark line.
#[derive(Clone, Debug)]
pub struct Row {
    /// Group name, e.g. `parse`. Must match the group's module name.
    pub group: &'static str,
    /// Fixture or scenario, e.g. `issue674` or `obj-16keys-hit-cold`.
    pub case: String,
    /// `ijson` or `serde` (the baseline), or a named variant.
    pub imp: &'static str,
    pub work: Work,
}

pub struct Report {
    csv: Option<fs::File>,
}

/// A counter value for the table: `-` when absent.
fn cell(v: Option<f64>, decimals: usize) -> String {
    v.map_or_else(|| "-".to_string(), |x| format!("{x:.decimals$}"))
}

impl Report {
    pub fn new(csv_path: Option<&Path>) -> Self {
        let csv = csv_path.map(|p| {
            if let Some(d) = p.parent() {
                fs::create_dir_all(d).ok();
            }
            let mut f = fs::File::create(p).expect("create csv");
            writeln!(
                f,
                "group,case,impl,unit,units,inner,sample,ns_per_op,{},pmu,scaled",
                counters::NAMES.join(",")
            )
            .unwrap();
            f
        });
        println!(
            "{:<12} {:<40} {:<8} {:>9} {:>12} {:>12} {:>6} {:>8} {:>11} {:>12} {:>12} {:>5} {:>9} {:>9} {:>7}",
            "group",
            "case",
            "impl",
            "units",
            "min ns/op",
            "median",
            "mad%",
            "p10-90%",
            "throughput",
            "ins/op",
            "cyc/op",
            "IPC",
            "brmiss/op",
            "cmiss/op",
            "samples"
        );
        Report { csv }
    }

    pub fn record(&mut self, row: &Row, s: &Samples) {
        let min = s.min();
        let (unit, n, tput) = match row.work {
            Work::Bytes(n) => ("bytes", n, format!("{:.1} MB/s", n as f64 * 1000.0 / min)),
            Work::Items(n) => ("items", n, format!("{:.2} M/s", n as f64 * 1000.0 / min)),
        };
        // Counter columns are medians over the samples that were counted.
        let ins = s.counter_median(INSTRUCTIONS);
        let cyc = s.counter_median(CYCLES);
        let ipc = ins.zip(cyc).filter(|(_, c)| *c > 0.0).map(|(i, c)| i / c);
        println!(
            "{:<12} {:<40} {:<8} {:>9} {:>12.1} {:>12.1} {:>6.2} {:>8.2} {:>11} {:>12} {:>12} {:>5} {:>9} {:>9} {:>7}",
            row.group,
            row.case,
            row.imp,
            n,
            min,
            s.median(),
            s.mad_rel() * 100.0,
            s.spread_rel() * 100.0,
            tput,
            cell(ins, 1),
            cell(cyc, 1),
            cell(ipc, 2),
            cell(s.counter_median(BRANCH_MISSES), 2),
            cell(s.counter_median(CACHE_MISSES), 2),
            s.ns_per_op.len()
        );
        if let Some(f) = self.csv.as_mut() {
            for (i, ns) in s.ns_per_op.iter().enumerate() {
                write!(f, "{},{},{},{},{},{},{},{:.3}", row.group, row.case, row.imp, unit, n, s.inner, i, ns)
                    .unwrap();
                // Absent counters are empty fields, never zero.
                match s.counters.get(i).and_then(|c| c.as_ref()) {
                    Some(c) => {
                        for v in c.v {
                            match v {
                                Some(x) => write!(f, ",{x:.3}").unwrap(),
                                None => write!(f, ",").unwrap(),
                            }
                        }
                        writeln!(f, ",{},{}", c.pmu, c.scaled as u8).unwrap();
                    }
                    None => writeln!(f, "{}", ",".repeat(counters::N + 2)).unwrap(),
                }
            }
        }
    }
}
