//! Result table and per-sample CSV.

use std::fs;
use std::io::Write;
use std::path::Path;

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

impl Report {
    pub fn new(csv_path: Option<&Path>) -> Self {
        let csv = csv_path.map(|p| {
            if let Some(d) = p.parent() {
                fs::create_dir_all(d).ok();
            }
            let mut f = fs::File::create(p).expect("create csv");
            writeln!(f, "group,case,impl,unit,units,inner,sample,ns_per_op").unwrap();
            f
        });
        println!(
            "{:<12} {:<40} {:<8} {:>9} {:>12} {:>12} {:>11} {:>7}",
            "group", "case", "impl", "units", "min ns/op", "median", "throughput", "samples"
        );
        Report { csv }
    }

    pub fn record(&mut self, row: &Row, s: &Samples) {
        let min = s.min();
        let (unit, n, tput) = match row.work {
            Work::Bytes(n) => ("bytes", n, format!("{:.1} MB/s", n as f64 * 1000.0 / min)),
            Work::Items(n) => ("items", n, format!("{:.2} M/s", n as f64 * 1000.0 / min)),
        };
        println!(
            "{:<12} {:<40} {:<8} {:>9} {:>12.1} {:>12.1} {:>11} {:>7}",
            row.group,
            row.case,
            row.imp,
            n,
            min,
            s.median(),
            tput,
            s.ns_per_op.len()
        );
        if let Some(f) = self.csv.as_mut() {
            for (i, ns) in s.ns_per_op.iter().enumerate() {
                writeln!(f, "{},{},{},{},{},{},{},{:.3}", row.group, row.case, row.imp, unit, n, s.inner, i, ns)
                    .unwrap();
            }
        }
    }
}
