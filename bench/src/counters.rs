//! Hardware counters for the calling thread, user space only, through
//! `perf_event_open(2)`.
//!
//! One event group per core PMU, so the events of a group are read atomically.
//! On a hybrid CPU (Intel `cpu_core` + `cpu_atom`) a generic hardware event
//! counts on one core type only, so a group is opened on each PMU and a sample
//! is attributed to whichever one was running. Counts are scaled by
//! `time_enabled / time_running` when the kernel multiplexed the group.
//!
//! Absent values are `None`. Nothing here ever invents a number.

use std::cell::RefCell;
use std::rc::Rc;

/// Number of events per group.
pub const N: usize = 6;
/// Event names, in the order of [`Counts::v`] and of the CSV columns.
pub const NAMES: [&str; N] =
    ["instructions", "cycles", "branches", "branch_misses", "cache_misses", "cache_refs"];
pub const INSTRUCTIONS: usize = 0;
pub const CYCLES: usize = 1;
pub const BRANCHES: usize = 2;
pub const BRANCH_MISSES: usize = 3;
pub const CACHE_MISSES: usize = 4;
pub const CACHE_REFS: usize = 5;

/// Counter values for one sample. The harness divides them by the batch size.
#[derive(Clone, Copy, Debug)]
pub struct Counts {
    /// Indexed like [`NAMES`]. `None`: that event could not be opened.
    pub v: [Option<f64>; N],
    /// PMU the sample ran on (`cpu`, `cpu_core`, `cpu_atom`), or `mixed` when
    /// the thread moved between core types inside the sample.
    pub pmu: &'static str,
    /// The kernel multiplexed the group and the values were scaled up.
    pub scaled: bool,
}

/// What happened when counters were set up; goes to stderr and the metadata.
#[derive(Clone, Debug)]
pub struct Status {
    pub available: bool,
    /// Why they are off, or what is being counted.
    pub detail: String,
    pub pmus: Vec<String>,
    pub events: Vec<&'static str>,
}

impl Status {
    pub fn off(reason: impl Into<String>) -> Self {
        Status { available: false, detail: reason.into(), pmus: Vec::new(), events: Vec::new() }
    }
}

thread_local! {
    static ACTIVE: RefCell<Option<Rc<CounterSet>>> = const { RefCell::new(None) };
}

/// Open counters for the calling thread and make the harness use them on this
/// thread. On failure the harness keeps running on wall-clock time only.
pub fn init() -> Status {
    match CounterSet::open() {
        Ok(set) => {
            let st = set.status();
            ACTIVE.with(|a| *a.borrow_mut() = Some(Rc::new(set)));
            st
        }
        Err(reason) => Status::off(reason),
    }
}

/// The calling thread's counters, if [`init`] succeeded on it.
pub fn current() -> Option<Rc<CounterSet>> {
    ACTIVE.with(|a| a.borrow().clone())
}

pub use imp::CounterSet;

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::cell::Cell;
    use std::fs;

    const PERF_TYPE_HARDWARE: u32 = 0;
    /// `PERF_COUNT_HW_*` ids, in the order of `NAMES`.
    const HW_IDS: [u64; N] = [1, 0, 4, 5, 3, 2];
    const FORMAT_TOTAL_TIME_ENABLED: u64 = 1;
    const FORMAT_TOTAL_TIME_RUNNING: u64 = 2;
    const FORMAT_GROUP: u64 = 8;
    const ATTR_DISABLED: u64 = 1 << 0;
    const ATTR_EXCLUDE_KERNEL: u64 = 1 << 5;
    const ATTR_EXCLUDE_HV: u64 = 1 << 6;
    const IOC_ENABLE: libc::c_ulong = 0x2400;
    const IOC_DISABLE: libc::c_ulong = 0x2401;
    const IOC_FLAG_GROUP: libc::c_ulong = 1;
    const FLAG_FD_CLOEXEC: libc::c_ulong = 8;

    /// `struct perf_event_attr`, first published layout (`PERF_ATTR_SIZE_VER0`).
    #[repr(C)]
    #[derive(Default)]
    struct Attr {
        type_: u32,
        size: u32,
        config: u64,
        sample_period: u64,
        sample_type: u64,
        read_format: u64,
        flags: u64,
        wakeup_events: u32,
        bp_type: u32,
        config1: u64,
    }

    fn open_event(config: u64, leader: i32) -> Result<i32, i32> {
        let mut flags = ATTR_EXCLUDE_KERNEL | ATTR_EXCLUDE_HV;
        if leader < 0 {
            flags |= ATTR_DISABLED;
        }
        let attr = Attr {
            type_: PERF_TYPE_HARDWARE,
            size: std::mem::size_of::<Attr>() as u32,
            config,
            read_format: FORMAT_GROUP | FORMAT_TOTAL_TIME_ENABLED | FORMAT_TOTAL_TIME_RUNNING,
            flags,
            ..Attr::default()
        };
        // pid 0, cpu -1: this thread, on whichever CPU it runs.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_perf_event_open,
                &attr as *const Attr,
                0 as libc::pid_t,
                -1 as libc::c_int,
                leader as libc::c_int,
                FLAG_FD_CLOEXEC,
            )
        };
        if fd < 0 {
            Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
        } else {
            Ok(fd as i32)
        }
    }

    #[derive(Clone, Copy, Default)]
    struct Raw {
        enabled: u64,
        running: u64,
        v: [u64; N],
    }

    struct Group {
        name: &'static str,
        leader: i32,
        /// Which of `NAMES` each value of a group read belongs to.
        slots: Vec<usize>,
        fds: Vec<i32>,
        prev: Cell<Raw>,
    }

    impl Group {
        /// `pmu_type`: the PMU's dynamic type on a hybrid CPU, placed in the
        /// high half of `config` (the kernel's extended hardware-type encoding).
        fn open(name: &'static str, pmu_type: Option<u64>) -> Result<Group, i32> {
            let cfg = |id: u64| id | (pmu_type.unwrap_or(0) << 32);
            let leader = open_event(cfg(HW_IDS[0]), -1)?;
            let mut slots = vec![0];
            let mut fds = vec![leader];
            for (i, id) in HW_IDS.iter().enumerate().skip(1) {
                if let Ok(fd) = open_event(cfg(*id), leader) {
                    slots.push(i);
                    fds.push(fd);
                }
            }
            let g = Group { name, leader, slots, fds, prev: Cell::new(Raw::default()) };
            g.prev.set(g.read().ok_or(libc::EIO)?);
            Ok(g)
        }

        fn read(&self) -> Option<Raw> {
            let mut buf = [0u64; 3 + N];
            let want = (3 + self.slots.len()) * 8;
            let got = unsafe { libc::read(self.leader, buf.as_mut_ptr().cast(), want) };
            if got != want as isize || buf[0] as usize != self.slots.len() {
                return None;
            }
            let mut r = Raw { enabled: buf[1], running: buf[2], v: [0; N] };
            for (k, slot) in self.slots.iter().enumerate() {
                r.v[*slot] = buf[3 + k];
            }
            Some(r)
        }

        /// Change since the previous read. Counters only run between `start`
        /// and `stop`, so this is exactly the last bracketed region.
        fn delta(&self) -> Option<Raw> {
            let now = self.read()?;
            let p = self.prev.replace(now);
            let mut d = Raw {
                enabled: now.enabled.wrapping_sub(p.enabled),
                running: now.running.wrapping_sub(p.running),
                v: [0; N],
            };
            for i in 0..N {
                d.v[i] = now.v[i].wrapping_sub(p.v[i]);
            }
            Some(d)
        }
    }

    impl Drop for Group {
        fn drop(&mut self) {
            for fd in self.fds.iter().rev() {
                unsafe { libc::close(*fd) };
            }
        }
    }

    pub struct CounterSet {
        groups: Vec<Group>,
        notes: Vec<String>,
    }

    fn read_trim(p: &str) -> Option<String> {
        fs::read_to_string(p).ok().map(|s| s.trim().to_string())
    }

    fn explain(errno: i32) -> String {
        let paranoid = read_trim("/proc/sys/kernel/perf_event_paranoid").unwrap_or_else(|| "unreadable".into());
        let why = match errno {
            libc::EACCES | libc::EPERM => {
                "permission denied; user-space counting of own thread needs perf_event_paranoid <= 2 or CAP_PERFMON"
            }
            libc::ENOENT | libc::EOPNOTSUPP | libc::ENODEV => "no hardware PMU exposed (virtual machine or container?)",
            libc::ENOSYS => "kernel built without perf events",
            libc::EINVAL => "kernel rejected the event encoding",
            _ => "unexpected error",
        };
        format!(
            "perf_event_open failed: {} (errno {errno}): {why}; perf_event_paranoid={paranoid}",
            std::io::Error::from_raw_os_error(errno)
        )
    }

    impl CounterSet {
        pub fn open() -> Result<CounterSet, String> {
            // Hybrid CPUs expose one PMU per core type; a plain generic event
            // would count on only one of them.
            let hybrid: Vec<(&'static str, u64)> = ["cpu_core", "cpu_atom"]
                .into_iter()
                .filter_map(|n| {
                    let t = read_trim(&format!("/sys/bus/event_source/devices/{n}/type"))?.parse().ok()?;
                    Some((n, t))
                })
                .collect();
            let wanted: Vec<(&'static str, Option<u64>)> = if hybrid.len() == 2 {
                hybrid.into_iter().map(|(n, t)| (n, Some(t))).collect()
            } else {
                vec![("cpu", None)]
            };
            let mut groups = Vec::new();
            let mut notes = Vec::new();
            let mut first_err = None;
            for (name, ty) in wanted {
                match Group::open(name, ty) {
                    Ok(g) => groups.push(g),
                    Err(e) => {
                        notes.push(format!("{name}: not opened (errno {e}), samples on that core type have no counters"));
                        first_err.get_or_insert(e);
                    }
                }
            }
            if groups.is_empty() {
                return Err(explain(first_err.unwrap_or(0)));
            }
            let set = CounterSet { groups, notes };
            // Prove that the counters count before trusting them.
            set.start();
            let mut x = 0u64;
            for i in 0..20_000u64 {
                x = std::hint::black_box(x.wrapping_add(i));
            }
            set.stop();
            match set.read() {
                Some(c) if c.v[INSTRUCTIONS].is_some_and(|n| n >= 20_000.0) => Ok(set),
                Some(c) => Err(format!(
                    "counters opened but a 20000-iteration loop counted {:?} instructions; not trusted",
                    c.v[INSTRUCTIONS]
                )),
                None => Err(format!(
                    "counters opened but never ran on this CPU (cpu {}); PMU not available to this thread",
                    unsafe { libc::sched_getcpu() }
                )),
            }
        }

        pub fn status(&self) -> Status {
            let pmus: Vec<String> = self.groups.iter().map(|g| g.name.to_string()).collect();
            let events: Vec<&'static str> =
                (0..N).filter(|i| self.groups.iter().any(|g| g.slots.contains(i))).map(|i| NAMES[i]).collect();
            let mut detail = format!("user-space only, this thread, one group per PMU [{}]", pmus.join(", "));
            for g in &self.groups {
                let missing: Vec<&str> = (0..N).filter(|i| !g.slots.contains(i)).map(|i| NAMES[i]).collect();
                if !missing.is_empty() {
                    detail.push_str(&format!("; {}: no {}", g.name, missing.join("/")));
                }
            }
            for n in &self.notes {
                detail.push_str("; ");
                detail.push_str(n);
            }
            Status { available: true, detail, pmus, events }
        }

        /// Start counting. One `ioctl` per PMU group.
        #[inline(always)]
        pub fn start(&self) {
            for g in &self.groups {
                unsafe { libc::ioctl(g.leader, IOC_ENABLE, IOC_FLAG_GROUP) };
            }
        }

        /// Stop counting. One `ioctl` per PMU group.
        #[inline(always)]
        pub fn stop(&self) {
            for g in self.groups.iter().rev() {
                unsafe { libc::ioctl(g.leader, IOC_DISABLE, IOC_FLAG_GROUP) };
            }
        }

        /// Counts for the region between the last `start` and `stop`. `None`
        /// when no group ran (for example the thread sat on a core type whose
        /// PMU could not be opened).
        pub fn read(&self) -> Option<Counts> {
            let mut ran: Vec<(&Group, Raw)> = Vec::with_capacity(2);
            let mut failed = false;
            for g in &self.groups {
                match g.delta() {
                    Some(d) if d.running > 0 => ran.push((g, d)),
                    Some(_) => {}
                    None => failed = true,
                }
            }
            if failed || ran.is_empty() {
                return None;
            }
            let mut out = Counts { v: [None; N], pmu: "mixed", scaled: false };
            if let [(g, d)] = ran[..] {
                // The usual case: the whole sample ran on one core type.
                let scale = if d.running < d.enabled { d.enabled as f64 / d.running as f64 } else { 1.0 };
                out.pmu = g.name;
                out.scaled = scale > 1.0;
                for slot in &g.slots {
                    out.v[*slot] = Some(d.v[*slot] as f64 * scale);
                }
            } else {
                // The thread changed core type inside the sample. Each group
                // counted its own part; add them. An event must exist in every
                // group that ran, or the sum would be an undercount.
                let enabled = ran.iter().map(|(_, d)| d.enabled).min().unwrap_or(0);
                let running: u64 = ran.iter().map(|(_, d)| d.running).sum();
                let scale = if running < enabled { enabled as f64 / running as f64 } else { 1.0 };
                out.scaled = scale > 1.0;
                for i in 0..N {
                    if ran.iter().all(|(g, _)| g.slots.contains(&i)) {
                        out.v[i] = Some(ran.iter().map(|(_, d)| d.v[i] as f64).sum::<f64>() * scale);
                    }
                }
            }
            Some(out)
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::*;

    /// Hardware counters are only implemented for Linux.
    pub struct CounterSet(());

    impl CounterSet {
        pub fn open() -> Result<CounterSet, String> {
            Err("hardware counters are only implemented for Linux (perf_event_open)".into())
        }
        pub fn status(&self) -> Status {
            Status::off("not Linux")
        }
        pub fn start(&self) {}
        pub fn stop(&self) {}
        pub fn read(&self) -> Option<Counts> {
            None
        }
    }
}
