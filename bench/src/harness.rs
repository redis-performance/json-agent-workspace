//! Timing loop. Modelled on Daniel Lemire's `rustquadsimd` harness, but every
//! sample is kept so noise can be judged from per-run rows.

use std::hint::black_box;
use std::time::Instant;

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

/// Time one sample of `inner` calls of `routine`, each on its own state built
/// by `setup` before the clock starts. States and results outlive the clock.
#[inline(never)]
fn sample_with_setup<S, R>(
    setup: &mut impl FnMut() -> S,
    routine: &mut impl FnMut(&mut S) -> R,
    inner: usize,
    states: &mut Vec<S>,
    keep: &mut Vec<R>,
) -> u128 {
    states.clear();
    keep.clear();
    for _ in 0..inner {
        states.push(setup());
    }
    let start = Instant::now();
    for s in states.iter_mut() {
        keep.push(black_box(routine(black_box(s))));
    }
    let elapsed = start.elapsed().as_nanos();
    black_box(&*keep);
    keep.clear();
    states.clear();
    elapsed
}

/// Like [`bench`], for routines that change or consume their input.
pub fn bench_with_setup<S, R>(
    mut setup: impl FnMut() -> S,
    mut routine: impl FnMut(&mut S) -> R,
    p: Params,
) -> Samples {
    let mut states: Vec<S> = Vec::new();
    let mut keep: Vec<R> = Vec::new();
    let mut one = u128::MAX;
    for _ in 0..3 {
        one = one.min(sample_with_setup(&mut setup, &mut routine, 1, &mut states, &mut keep)).max(1);
    }
    let inner = ((p.min_sample_ns / one) as usize).clamp(1, 1 << 16);
    let mut n = p.min_repeat;
    let mut warm_total: u128 = 0;
    let mut i = 0;
    while i < n {
        warm_total += sample_with_setup(&mut setup, &mut routine, inner, &mut states, &mut keep);
        if i + 1 == n && warm_total < p.min_time_ns && n < p.max_repeat {
            n = n.saturating_mul(10).min(p.max_repeat);
        }
        i += 1;
    }
    let mut ns_per_op = Vec::with_capacity(n);
    for _ in 0..n {
        let t = sample_with_setup(&mut setup, &mut routine, inner, &mut states, &mut keep);
        ns_per_op.push(t as f64 / inner as f64);
    }
    Samples { inner, ns_per_op }
}
