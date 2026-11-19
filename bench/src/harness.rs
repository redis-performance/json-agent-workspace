//! Timing loop. Modelled on Daniel Lemire's `rustquadsimd` harness, but every
//! sample is kept so noise can be judged from per-run rows.

use std::hint::black_box;
use std::time::Instant;

use crate::counters::{self, CounterSet, Counts, N};

/// Harness parameters. Defaults match the loop this is modelled on.
#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub min_repeat: usize,
    pub min_time_ns: u128,
    pub max_repeat: usize,
    /// A single timed sample covers at least this long; short operations are
    /// batched so the timer's own cost stays small next to the work.
    pub min_sample_ns: u128,
    /// Wall-clock budget for one row, setup included. The repeat count stops
    /// growing once reaching the next size would exceed it, and measurement
    /// stops early (never below `min_repeat` samples) at twice this. It bounds
    /// run time for rows with slow setup; it does not change how a sample is timed.
    pub max_row_ns: u128,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            min_repeat: 10,
            min_time_ns: 400_000_000,
            max_repeat: 100_000,
            min_sample_ns: 50_000,
            max_row_ns: 10_000_000_000,
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
            max_row_ns: 500_000_000,
        }
    }
}

/// A row's batch size and sample count, fixed in advance. Both change what a
/// row measures (the share of each batch that sees cold state, how many
/// results are alive, how much bracketing cost each operation carries), so a
/// comparison is only valid when both sides ran the same plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    pub inner: usize,
    pub n: usize,
}

/// Per-operation results, one entry per timed sample.
pub struct Samples {
    pub inner: usize,
    pub ns_per_op: Vec<f64>,
    /// Hardware counters per operation, parallel to `ns_per_op`. Empty when
    /// counters are off; an entry is `None` when that sample was not counted.
    pub counters: Vec<Option<Counts>>,
}

fn sorted(v: impl Iterator<Item = f64>) -> Vec<f64> {
    let mut v: Vec<f64> = v.collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v
}

fn median_sorted(v: &[f64]) -> f64 {
    let n = v.len();
    if n == 0 {
        f64::NAN
    } else if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

impl Samples {
    pub fn min(&self) -> f64 {
        self.ns_per_op.iter().copied().fold(f64::INFINITY, f64::min)
    }
    pub fn median(&self) -> f64 {
        median_sorted(&sorted(self.ns_per_op.iter().copied()))
    }
    /// Median absolute deviation of the times, as a fraction of the median.
    pub fn mad_rel(&self) -> f64 {
        let med = self.median();
        median_sorted(&sorted(self.ns_per_op.iter().map(|x| (x - med).abs()))) / med
    }
    /// (p90 - p10) of the times, as a fraction of the median. Nearest rank.
    pub fn spread_rel(&self) -> f64 {
        let v = sorted(self.ns_per_op.iter().copied());
        let at = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
        (at(0.9) - at(0.1)) / median_sorted(&v)
    }
    /// Median per-operation value of counter `i` (see `counters::NAMES`) over
    /// the samples that have it. `None` if no sample has it.
    pub fn counter_median(&self, i: usize) -> Option<f64> {
        let v = sorted(self.counters.iter().filter_map(|c| c.as_ref().and_then(|c| c.v[i])));
        (!v.is_empty()).then(|| median_sorted(&v))
    }
    /// Smallest per-operation value of counter `i`.
    pub fn counter_min(&self, i: usize) -> Option<f64> {
        self.counters.iter().filter_map(|c| c.as_ref().and_then(|c| c.v[i])).reduce(f64::min)
    }
}

/// One timed sample: wall-clock nanoseconds and counters for the whole batch.
struct Timed {
    ns: u128,
    counts: Option<Counts>,
}

/// Time one sample of `inner` calls. Results are retained until the clock has
/// stopped, so the cost of dropping them is not part of the measurement.
/// Counters start just before the clock and stop just after it: two `ioctl`s
/// per PMU group per sample, none per call.
#[inline(never)]
fn sample<R, F: FnMut() -> R>(f: &mut F, inner: usize, keep: &mut Vec<R>, ctr: Option<&CounterSet>) -> Timed {
    keep.clear();
    keep.reserve(inner);
    if let Some(c) = ctr {
        c.start();
    }
    let start = Instant::now();
    for _ in 0..inner {
        keep.push(black_box(f()));
    }
    let ns = start.elapsed().as_nanos();
    if let Some(c) = ctr {
        c.stop();
    }
    black_box(&*keep);
    let counts = ctr.and_then(|c| c.read());
    keep.clear();
    Timed { ns, counts }
}

/// Calibrate, warm up and measure, given a function that times one sample of
/// a given batch size. With a `plan`, nothing is calibrated: the batch size
/// and sample count are exactly the plan's.
fn drive(p: Params, max_inner: usize, plan: Option<Plan>, counting: bool, mut s: impl FnMut(usize) -> Timed) -> Samples {
    let row_start = Instant::now();
    let (inner, n) = match plan {
        Some(pl) => {
            // Warm up for as many samples as will be measured, within the budget.
            for _ in 0..pl.n {
                s(pl.inner);
                if row_start.elapsed().as_nanos() > p.max_row_ns {
                    break;
                }
            }
            (pl.inner, pl.n)
        }
        None => {
            // Calibrate by doubling the batch until one measured sample is long
            // enough. A single call is not timed on its own: for a fast
            // operation that would measure the clock, not the operation.
            let mut inner = 1usize;
            loop {
                let t = s(inner).ns.min(s(inner).ns);
                if t >= p.min_sample_ns || inner >= max_inner {
                    break;
                }
                inner = (inner * 2).min(max_inner);
            }
            // Warm-up: grow the repeat count until the warm-up has lasted long
            // enough. `row_start` covers setup and drop too, which `warm_total`
            // does not see.
            let mut n = p.min_repeat;
            let mut warm_total: u128 = 0;
            let mut i = 0;
            while i < n {
                warm_total += s(inner).ns;
                if i + 1 == n && warm_total < p.min_time_ns && n < p.max_repeat {
                    // Growing tenfold and then measuring as many samples again
                    // costs about twenty times what has been spent so far.
                    let spent = row_start.elapsed().as_nanos();
                    if spent.saturating_mul(20) <= p.max_row_ns {
                        n = n.saturating_mul(10).min(p.max_repeat);
                    }
                }
                i += 1;
            }
            (inner, n)
        }
    };

    // Measurement: exactly n samples, all kept.
    let mut ns_per_op = Vec::with_capacity(n);
    let mut ctrs = Vec::with_capacity(if counting { n } else { 0 });
    for _ in 0..n {
        let t = s(inner);
        ns_per_op.push(t.ns as f64 / inner as f64);
        if counting {
            ctrs.push(t.counts.map(|mut c| {
                for i in 0..N {
                    c.v[i] = c.v[i].map(|x| x / inner as f64);
                }
                c
            }));
        }
    }
    Samples { inner, ns_per_op, counters: ctrs }
}

/// Run `f` under the harness and return every sample.
pub fn bench<R, F: FnMut() -> R>(mut f: F, p: Params, plan: Option<Plan>) -> Samples {
    let mut keep: Vec<R> = Vec::new();
    let ctr = counters::current();
    let ctr = ctr.as_deref();
    drive(p, 1 << 20, plan, ctr.is_some(), |inner| sample(&mut f, inner, &mut keep, ctr))
}

/// Cost of the harness itself: an empty routine, one call per sample, through
/// the same `sample` function as every benchmark. The result is what one
/// sample's clock and counter bracketing adds; a benchmark's per-operation
/// figures carry this divided by its `inner`. Reported, never subtracted.
pub fn overhead() -> Samples {
    let mut keep: Vec<u64> = Vec::new();
    let mut f = || black_box(0u64);
    let ctr = counters::current();
    let ctr = ctr.as_deref();
    let p = Params { min_repeat: 2_000, min_time_ns: 0, max_repeat: 2_000, min_sample_ns: 0, max_row_ns: u128::MAX };
    drive(p, 1, Some(Plan { inner: 1, n: 2_000 }), ctr.is_some(), |inner| sample(&mut f, inner, &mut keep, ctr))
}

/// Time one sample of `inner` calls of `routine`, each on its own state built
/// by `setup` before the clock starts. States and results outlive the clock.
#[inline(never)]
fn sample_with_setup<S, R>(
    setup: &mut impl FnMut() -> S,
    routine: &mut impl FnMut(&mut S) -> R,
    inner: usize,
    states: &mut Vec<S>,
    keep: &mut Vec<R>,
    ctr: Option<&CounterSet>,
) -> Timed {
    states.clear();
    keep.clear();
    keep.reserve(inner);
    for _ in 0..inner {
        states.push(setup());
    }
    if let Some(c) = ctr {
        c.start();
    }
    let start = Instant::now();
    for s in states.iter_mut() {
        keep.push(black_box(routine(black_box(s))));
    }
    let ns = start.elapsed().as_nanos();
    if let Some(c) = ctr {
        c.stop();
    }
    black_box(&*keep);
    let counts = ctr.and_then(|c| c.read());
    keep.clear();
    states.clear();
    Timed { ns, counts }
}

/// Like [`bench`], for routines that change or consume their input.
pub fn bench_with_setup<S, R>(
    mut setup: impl FnMut() -> S,
    mut routine: impl FnMut(&mut S) -> R,
    p: Params,
    plan: Option<Plan>,
) -> Samples {
    let mut states: Vec<S> = Vec::new();
    let mut keep: Vec<R> = Vec::new();
    let ctr = counters::current();
    let ctr = ctr.as_deref();
    drive(p, 1 << 16, plan, ctr.is_some(), |inner| {
        sample_with_setup(&mut setup, &mut routine, inner, &mut states, &mut keep, ctr)
    })
}
