//! Run metadata: what was measured, with what, on what. Written next to the
//! CSV as `<csv>.meta.json`. Values that cannot be read are `null`, never
//! guessed. No host name, user name, address or path is recorded.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::counters::{Status, CYCLES, INSTRUCTIONS};
use crate::harness::{Params, Samples};

/// Cost of one empty sample through the harness. See `harness::overhead`.
#[derive(Clone, Copy, Debug)]
pub struct Overhead {
    pub ns_min: f64,
    pub ns_median: f64,
    pub instructions_min: Option<f64>,
    pub instructions_median: Option<f64>,
    pub cycles_median: Option<f64>,
}

impl Overhead {
    pub fn from_samples(s: &Samples) -> Self {
        Overhead {
            ns_min: s.min(),
            ns_median: s.median(),
            instructions_min: s.counter_min(INSTRUCTIONS),
            instructions_median: s.counter_median(INSTRUCTIONS),
            cycles_median: s.counter_median(CYCLES),
        }
    }
}

/// What the binary knows about its own run.
pub struct RunInfo<'a> {
    /// `time` or `mem`.
    pub tool: &'a str,
    pub allocator: &'a str,
    pub params: Option<Params>,
    /// `None`: this tool does not use counters.
    pub counters: Option<&'a Status>,
    pub overhead: Option<Overhead>,
    /// CPU given to `--pin`, if any.
    pub pinned: Option<usize>,
}

fn read(p: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

fn read_u64(p: impl AsRef<Path>) -> Option<u64> {
    read(p)?.parse().ok()
}

/// Pin the calling thread to one CPU.
#[cfg(target_os = "linux")]
pub fn pin(cpu: usize) -> Result<(), String> {
    if cpu >= libc::CPU_SETSIZE as usize {
        return Err(format!("cpu {cpu} out of range"));
    }
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_SET(cpu, &mut set);
        if libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) != 0 {
            return Err(format!("sched_setaffinity({cpu}): {}", std::io::Error::last_os_error()));
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn pin(_cpu: usize) -> Result<(), String> {
    Err("--pin is only implemented for Linux".into())
}

/// (CPU the thread is on now, CPUs it is allowed on).
#[cfg(target_os = "linux")]
fn placement() -> (Option<usize>, Option<Vec<usize>>) {
    unsafe {
        let cpu = libc::sched_getcpu();
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        let mask = (libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set) == 0)
            .then(|| (0..libc::CPU_SETSIZE as usize).filter(|i| libc::CPU_ISSET(*i, &set)).collect());
        ((cpu >= 0).then_some(cpu as usize), mask)
    }
}

#[cfg(not(target_os = "linux"))]
fn placement() -> (Option<usize>, Option<Vec<usize>>) {
    (None, None)
}

/// Parse a sysfs CPU list such as `0-3,8`.
fn cpu_list(s: &str) -> Vec<usize> {
    let mut v = Vec::new();
    for part in s.split(',').filter(|p| !p.is_empty()) {
        let mut it = part.splitn(2, '-').map(|x| x.trim().parse::<usize>());
        match (it.next(), it.next()) {
            (Some(Ok(a)), None) => v.push(a),
            (Some(Ok(a)), Some(Ok(b))) => v.extend(a..=b),
            _ => {}
        }
    }
    v
}

fn utc_now() -> Option<String> {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    Some(format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60))
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Revision and dirtiness of the git tree containing `dir`. `pathspec` limits
/// the dirtiness check; `untracked` says whether untracked files count.
fn git_info(dir: &Path, pathspec: Option<&str>, untracked: bool) -> Value {
    let rev = git(dir, &["rev-parse", "HEAD"]);
    let mut args = vec!["status", "--porcelain", if untracked { "-unormal" } else { "-uno" }];
    if let Some(p) = pathspec {
        args.extend(["--", p]);
    }
    let dirty = rev.as_ref().and_then(|_| git(dir, &args)).map(|s| !s.is_empty());
    json!({ "rev": rev, "dirty": dirty })
}

fn cpu_info(cpu: Option<usize>, affinity: Option<&Vec<usize>>) -> Value {
    let cpuinfo = fs::read_to_string("/proc/cpuinfo").ok();
    let field = |block: &str, key: &str| {
        block.lines().find(|l| l.split(':').next().is_some_and(|k| k.trim() == key)).and_then(|l| l.split(':').nth(1)).map(|v| v.trim().to_string())
    };
    let model = cpuinfo.as_deref().and_then(|c| field(c, "model name"));
    let logical = cpuinfo.as_deref().map(|c| c.lines().filter(|l| l.starts_with("processor")).count()).filter(|n| *n > 0);
    let physical = cpuinfo.as_deref().and_then(|c| {
        let mut ids: Vec<(String, String)> = c
            .split("\n\n")
            .filter_map(|b| Some((field(b, "physical id")?, field(b, "core id")?)))
            .collect();
        ids.sort();
        ids.dedup();
        (!ids.is_empty()).then_some(ids.len())
    });
    let p_cpus = read("/sys/devices/cpu_core/cpus");
    let e_cpus = read("/sys/devices/cpu_atom/cpus");
    let core_type = cpu.and_then(|c| {
        if p_cpus.as_deref().is_some_and(|l| cpu_list(l).contains(&c)) {
            Some("cpu_core")
        } else if e_cpus.as_deref().is_some_and(|l| cpu_list(l).contains(&c)) {
            Some("cpu_atom")
        } else {
            None
        }
    });
    let freq = |name: &str| cpu.and_then(|c| read_u64(format!("/sys/devices/system/cpu/cpu{c}/cpufreq/{name}")));
    let governor = cpu.and_then(|c| read(format!("/sys/devices/system/cpu/cpu{c}/cpufreq/scaling_governor")));
    // Turbo is on when intel_pstate says no_turbo=0, or acpi-cpufreq says boost=1.
    let no_turbo = read_u64("/sys/devices/system/cpu/intel_pstate/no_turbo");
    let boost = read_u64("/sys/devices/system/cpu/cpufreq/boost");
    let turbo = no_turbo.map(|v| v == 0).or(boost.map(|v| v == 1));
    json!({
        "model": model,
        "logical_cpus": logical,
        "physical_cores": physical,
        "hybrid": p_cpus.is_some() && e_cpus.is_some(),
        "cpu_core_cpus": p_cpus,
        "cpu_atom_cpus": e_cpus,
        "thread_cpu": cpu,
        "thread_cpu_core_type": core_type,
        "affinity": affinity,
        "governor": governor,
        "freq_khz": { "current": freq("scaling_cur_freq"), "min": freq("cpuinfo_min_freq"), "max": freq("cpuinfo_max_freq") },
        "turbo_enabled": turbo,
        "intel_pstate_no_turbo": no_turbo,
        "cpufreq_boost": boost,
        "smt_active": read_u64("/sys/devices/system/cpu/smt/active").map(|v| v == 1),
    })
}

fn opt(s: &'static str) -> Option<&'static str> {
    (!s.is_empty()).then_some(s)
}

/// Gather everything. Call from the benchmark thread, after `--pin`.
pub fn collect(info: &RunInfo) -> Value {
    let ws = Path::new(env!("CARGO_MANIFEST_DIR")).parent().map(Path::to_path_buf).unwrap_or_default();
    let (cpu, affinity) = placement();
    let pinned_to = affinity.as_ref().filter(|a| a.len() == 1).map(|a| a[0]);
    json!({
        "tool": info.tool,
        "timestamp_utc": utc_now(),
        "build": {
            "rustc": opt(env!("BENCH_RUSTC_VERSION")),
            "target": opt(env!("BENCH_TARGET")),
            "profile": opt(env!("BENCH_PROFILE")),
            "opt_level": opt(env!("BENCH_OPT_LEVEL")),
            "debuginfo": opt(env!("BENCH_DEBUG")),
            "debug_assertions": cfg!(debug_assertions),
            "lto": "off (profile default; see Cargo.toml)",
            "ijson_dependency": opt(env!("BENCH_IJSON_DEP")),
            "allocator": info.allocator,
        },
        "git": {
            // Dirtiness of the workspace is judged on bench/ only: the
            // sibling checkouts are reported on their own lines.
            "workspace": git_info(&ws, Some("bench"), true),
            "ijson": git_info(&ws.join("ijson"), None, false),
            "RedisJSON": git_info(&ws.join("RedisJSON"), None, false),
        },
        "params": info.params.map(|p| json!({
            "min_repeat": p.min_repeat,
            "min_time_ns": p.min_time_ns as u64,
            "max_repeat": p.max_repeat,
            "min_sample_ns": p.min_sample_ns as u64,
            "max_row_ns": p.max_row_ns as u64,
        })),
        "cpu": cpu_info(cpu, affinity.as_ref()),
        "pin": { "requested": info.pinned, "pinned_to": pinned_to },
        "os": {
            "kernel": read("/proc/sys/kernel/osrelease"),
            "perf_event_paranoid": read("/proc/sys/kernel/perf_event_paranoid").and_then(|s| s.parse::<i64>().ok()),
        },
        "counters": info.counters.map(|c| json!({
            "available": c.available,
            "detail": c.detail,
            "pmus": c.pmus,
            "events": c.events,
        })),
        "harness_overhead_per_sample": info.overhead.map(|o| json!({
            "ns_min": o.ns_min,
            "ns_median": o.ns_median,
            "instructions_min": o.instructions_min,
            "instructions_median": o.instructions_median,
            "cycles_median": o.cycles_median,
            "note": "empty routine, inner=1; a row's per-op figures include this divided by its inner; not subtracted",
        })),
    })
}

/// Conditions that make the results unfit for reporting.
pub fn warnings(meta: &Value) -> Vec<String> {
    let cpu = &meta["cpu"];
    let mut w = Vec::new();
    if meta["pin"]["pinned_to"].is_null() {
        w.push("benchmark thread is not pinned to one CPU (use --pin CPU)".to_string());
    }
    match cpu["governor"].as_str() {
        Some("performance") => {}
        Some(g) => w.push(format!("scaling governor is '{g}', not 'performance'")),
        None => w.push("scaling governor could not be read".to_string()),
    }
    match cpu["turbo_enabled"].as_bool() {
        Some(true) => w.push("turbo/boost is on".to_string()),
        Some(false) => {}
        None => w.push("turbo/boost state could not be read".to_string()),
    }
    match cpu["smt_active"].as_bool() {
        Some(true) => w.push("SMT is active".to_string()),
        Some(false) => {}
        None => w.push("SMT state could not be read".to_string()),
    }
    w
}

/// One line for stderr.
pub fn brief(meta: &Value) -> String {
    let s = |v: &Value| v.as_str().map_or_else(|| "?".to_string(), str::to_string);
    let n = |v: &Value| v.as_u64().map_or_else(|| "?".to_string(), |x| x.to_string());
    let short = |v: &Value| v.as_str().map_or_else(|| "?".to_string(), |r| r.chars().take(12).collect());
    let dirty = |v: &Value| if v.as_bool() == Some(true) { "+dirty" } else { "" };
    let g = &meta["git"];
    format!(
        "{}; {}; cpu {} ({}); governor {}; kernel {}; ijson {}{}; RedisJSON {}{}; bench {}{}",
        s(&meta["build"]["rustc"]),
        s(&meta["cpu"]["model"]),
        n(&meta["cpu"]["thread_cpu"]),
        meta["cpu"]["thread_cpu_core_type"].as_str().unwrap_or("core type n/a"),
        s(&meta["cpu"]["governor"]),
        s(&meta["os"]["kernel"]),
        short(&g["ijson"]["rev"]),
        dirty(&g["ijson"]["dirty"]),
        short(&g["RedisJSON"]["rev"]),
        dirty(&g["RedisJSON"]["dirty"]),
        short(&g["workspace"]["rev"]),
        dirty(&g["workspace"]["dirty"]),
    )
}

/// Write `<csv>.meta.json` and return its path.
pub fn write_sidecar(csv: &Path, meta: &Value) -> std::io::Result<PathBuf> {
    let mut name = csv.as_os_str().to_os_string();
    name.push(".meta.json");
    let path = PathBuf::from(name);
    if let Some(d) = path.parent() {
        fs::create_dir_all(d).ok();
    }
    fs::write(&path, serde_json::to_string_pretty(meta).map_err(std::io::Error::other)? + "\n")?;
    Ok(path)
}
