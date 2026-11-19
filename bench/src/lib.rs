//! Microbenchmark harness for `ijson`, mirroring RedisJSON's call paths.
//!
//! The timing loop follows Daniel Lemire's `rustquadsimd` harness: warm up
//! until a minimum wall time has passed, growing the repeat count tenfold,
//! then time that many passes. Unlike that harness, every sample is kept so
//! that noise can be judged from per-run rows, not only from the minimum.

use std::fs;
use std::hint::black_box;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use ijson::{IValue, IValueDeserSeed};

pub mod counting_alloc;

/// Harness parameters. Defaults match the loop this is modelled on.
#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub min_repeat: usize,
    pub min_time_ns: u128,
    pub max_repeat: usize,
    /// A single timed sample covers at least this long; short operations are
    /// batched so the timer's own cost stays small next to the work.
    pub min_sample_ns: u128,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            min_repeat: 10,
            min_time_ns: 400_000_000,
            max_repeat: 100_000,
            min_sample_ns: 50_000,
        }
    }
}

impl Params {
    /// Fast settings for checking that the suite runs. Not for reporting.
    pub fn quick() -> Self {
        Params {
            min_repeat: 5,
            min_time_ns: 20_000_000,
            max_repeat: 1_000,
            min_sample_ns: 20_000,
        }
    }
}

/// Per-operation times in nanoseconds, one entry per timed sample.
pub struct Samples {
    pub inner: usize,
    pub ns_per_op: Vec<f64>,
}

impl Samples {
    pub fn min(&self) -> f64 {
        self.ns_per_op.iter().copied().fold(f64::INFINITY, f64::min)
    }
    pub fn median(&self) -> f64 {
        let mut v = self.ns_per_op.clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = v.len();
        if n % 2 == 1 {
            v[n / 2]
        } else {
            (v[n / 2 - 1] + v[n / 2]) / 2.0
        }
    }
}

/// Time one sample of `inner` calls. Results are retained until the clock has
/// stopped, so the cost of dropping them is not part of the measurement.
#[inline(never)]
fn sample<R, F: FnMut() -> R>(f: &mut F, inner: usize, keep: &mut Vec<R>) -> u128 {
    keep.clear();
    let start = Instant::now();
    for _ in 0..inner {
        keep.push(black_box(f()));
    }
    let elapsed = start.elapsed().as_nanos();
    black_box(&*keep);
    keep.clear();
    elapsed
}

/// Run `f` under the harness and return every sample.
pub fn bench<R, F: FnMut() -> R>(mut f: F, p: Params) -> Samples {
    let mut keep: Vec<R> = Vec::new();

    // Calibrate the batch size from a few single calls.
    let mut one = u128::MAX;
    for _ in 0..3 {
        one = one.min(sample(&mut f, 1, &mut keep)).max(1);
    }
    let inner = ((p.min_sample_ns / one) as usize).clamp(1, 1 << 20);
    keep.reserve(inner);

    // Warm-up: grow the repeat count until the warm-up has lasted long enough.
    let mut n = p.min_repeat;
    let mut warm_total: u128 = 0;
    let mut i = 0;
    while i < n {
        warm_total += sample(&mut f, inner, &mut keep);
        if i + 1 == n && warm_total < p.min_time_ns && n < p.max_repeat {
            n = n.saturating_mul(10).min(p.max_repeat);
        }
        i += 1;
    }

    // Measurement: n samples, all kept.
    let mut ns_per_op = Vec::with_capacity(n);
    for _ in 0..n {
        let t = sample(&mut f, inner, &mut keep);
        ns_per_op.push(t as f64 / inner as f64);
    }
    Samples { inner, ns_per_op }
}

// ---------------------------------------------------------------------------
// The operations under test, written the way RedisJSON calls them.
// ---------------------------------------------------------------------------

/// `JSON.SET` parse path: `redis_json/src/ivalue_manager.rs`, `from_str` with
/// the default arguments (`limit_depth = true`, no forced float type).
#[inline(never)]
pub fn ijson_parse(text: &str) -> IValue {
    let mut de = serde_json::Deserializer::from_str(text);
    let v = IValueDeserSeed::new(None)
        .deserialize_compact_objects(&mut de)
        .expect("fixture parses");
    de.end().expect("no trailing data");
    v
}

/// Baseline: the same text into `serde_json::Value`.
#[inline(never)]
pub fn serde_parse(text: &str) -> serde_json::Value {
    let mut de = serde_json::Deserializer::from_str(text);
    let v = <serde_json::Value as serde::Deserialize>::deserialize(&mut de).expect("fixture parses");
    de.end().expect("no trailing data");
    v
}

/// `JSON.GET` default-format path: `redis_json/src/key_value.rs`,
/// `serialize_object` without indentation is `serde_json::to_string`.
#[inline(never)]
pub fn ijson_serialize(v: &IValue) -> String {
    serde_json::to_string(v).expect("serialises")
}

#[inline(never)]
pub fn serde_serialize(v: &serde_json::Value) -> String {
    serde_json::to_string(v).expect("serialises")
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

pub struct Fixture {
    pub name: String,
    pub text: String,
}

/// RedisJSON's benchmark datasets, read from the pinned submodule.
pub fn load_fixtures(dir: &Path) -> Vec<Fixture> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read fixture dir {}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|p| Fixture {
            name: p.file_stem().unwrap().to_string_lossy().into_owned(),
            text: fs::read_to_string(&p).unwrap(),
        })
        .collect()
}

pub fn default_fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../RedisJSON/tests/benchmarks/datasets")
}

/// Check, before anything is timed, that `ijson` and the baseline agree on a
/// fixture: parse with both, serialise the `ijson` value, and require that it
/// reads back as the same document the baseline parsed.
pub fn verify(fx: &Fixture) -> Result<(), String> {
    let reference = serde_parse(&fx.text);
    let out = ijson_serialize(&ijson_parse(&fx.text));
    let back: serde_json::Value =
        serde_json::from_str(&out).map_err(|e| format!("ijson output does not parse: {e}"))?;
    if back != reference {
        return Err("ijson round trip differs from serde_json::Value".into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------------

pub struct Row<'a> {
    pub group: &'a str,
    pub fixture: &'a str,
    pub imp: &'a str,
    pub bytes: usize,
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
            writeln!(f, "group,fixture,impl,bytes,inner,sample,ns_per_op").unwrap();
            f
        });
        println!(
            "{:<10} {:<38} {:<6} {:>8} {:>12} {:>12} {:>9} {:>7}",
            "group", "fixture", "impl", "bytes", "min ns/op", "median", "MB/s", "samples"
        );
        Report { csv }
    }

    pub fn record(&mut self, row: &Row, s: &Samples) {
        let min = s.min();
        println!(
            "{:<10} {:<38} {:<6} {:>8} {:>12.1} {:>12.1} {:>9.1} {:>7}",
            row.group,
            row.fixture,
            row.imp,
            row.bytes,
            min,
            s.median(),
            row.bytes as f64 * 1000.0 / min,
            s.ns_per_op.len()
        );
        if let Some(f) = self.csv.as_mut() {
            for (i, ns) in s.ns_per_op.iter().enumerate() {
                writeln!(
                    f,
                    "{},{},{},{},{},{},{:.3}",
                    row.group, row.fixture, row.imp, row.bytes, s.inner, i, ns
                )
                .unwrap();
            }
        }
    }
}
