//! The interface benchmark groups are written against. Groups only use `Ctx`,
//! `Row` and `Work`; how a measurement is taken and recorded is behind it.

use crate::fixtures::Fixture;
use crate::harness::{self, Params};
use crate::report::{Report, Row};

pub struct Ctx<'a> {
    /// RedisJSON's datasets (already verified), after `--filter`.
    pub fixtures: &'a [Fixture],
    pub params: Params,
    pub report: &'a mut Report,
    /// `--case SUBSTR`: only rows whose case contains it are run.
    pub case_filter: Option<String>,
}

impl Ctx<'_> {
    /// Whether a row would run. Check this before doing expensive setup.
    pub fn wants(&self, row: &Row) -> bool {
        self.case_filter.as_ref().is_none_or(|s| row.case.contains(s.as_str()))
    }

    /// Time `f`. Its return value is kept alive until the clock stops, so
    /// dropping it is not measured. Use for operations that do not consume or
    /// change their input (parse, serialise, lookup).
    pub fn bench<R>(&mut self, row: Row, f: impl FnMut() -> R) {
        if !self.wants(&row) {
            return;
        }
        let s = harness::bench(f, self.params);
        self.report.record(&row, &s);
    }

    /// Time `routine` on a fresh state from `setup` for every call. `setup`
    /// runs outside the timed region, as does dropping the state and the
    /// result. Use for operations that change or consume their input
    /// (mutation, drop, building from a prepared value).
    pub fn bench_with_setup<S, R>(
        &mut self,
        row: Row,
        setup: impl FnMut() -> S,
        routine: impl FnMut(&mut S) -> R,
    ) {
        if !self.wants(&row) {
            return;
        }
        let s = harness::bench_with_setup(setup, routine, self.params);
        self.report.record(&row, &s);
    }
}
