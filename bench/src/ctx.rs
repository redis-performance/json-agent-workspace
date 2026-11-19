//! The interface benchmark groups are written against. Groups only use `Ctx`,
//! `Row` and `Work`; how a measurement is taken and recorded is behind it.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::Path;

use crate::fixtures::Fixture;
use crate::harness::{self, Params, Plan, Samples};
use crate::report::{Report, Row};

pub struct Ctx<'a> {
    /// RedisJSON's datasets (already verified), after `--filter`.
    pub fixtures: &'a [Fixture],
    pub params: Params,
    pub report: &'a mut Report,
    /// `--case SUBSTR`: only rows whose case contains it are run.
    pub case_filter: Option<String>,
    /// Batch size and sample count per row, from `--plan-in`. Rows found here
    /// are not calibrated, so two runs with the same plan measure the same thing.
    pub plan_in: Option<HashMap<RowKey, Plan>>,
    /// What every row actually ran with, for `--plan-out`.
    pub plan_used: Vec<(RowKey, Plan)>,
}

/// `(group, case, impl)`.
pub type RowKey = (String, String, String);

fn key(row: &Row) -> RowKey {
    (row.group.to_string(), row.case.clone(), row.imp.to_string())
}

/// Read a plan written by [`Ctx::write_plan`].
pub fn read_plan(path: &Path) -> Result<HashMap<RowKey, Plan>, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = HashMap::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        let bad = || format!("bad plan line: {line}");
        if f.len() != 5 {
            return Err(bad());
        }
        let plan = Plan { inner: f[3].parse().map_err(|_| bad())?, n: f[4].parse().map_err(|_| bad())? };
        out.insert((f[0].into(), f[1].into(), f[2].into()), plan);
    }
    Ok(out)
}

impl Ctx<'_> {
    fn plan_for(&self, row: &Row) -> Option<Plan> {
        let plans = self.plan_in.as_ref()?;
        let found = plans.get(&key(row)).copied();
        if found.is_none() {
            eprintln!("WARNING: {}/{}/{} is not in the plan; calibrating it, so it is not comparable", row.group, row.case, row.imp);
        }
        found
    }

    fn done(&mut self, row: &Row, s: &Samples) {
        self.plan_used.push((key(row), Plan { inner: s.inner, n: s.ns_per_op.len() }));
        self.report.record(row, s);
    }

    /// Write the plan every row ran with.
    pub fn write_plan(&self, path: &Path) -> std::io::Result<()> {
        let mut f = fs::File::create(path)?;
        writeln!(f, "group,case,impl,inner,n")?;
        for ((g, c, i), p) in &self.plan_used {
            writeln!(f, "{g},{c},{i},{},{}", p.inner, p.n)?;
        }
        Ok(())
    }

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
        let s = harness::bench(f, self.params, self.plan_for(&row));
        self.done(&row, &s);
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
        let s = harness::bench_with_setup(setup, routine, self.params, self.plan_for(&row));
        self.done(&row, &s);
    }
}
