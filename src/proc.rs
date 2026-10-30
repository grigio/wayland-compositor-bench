//! The measurement layer: everything read from /proc, /sys and the session's
//! cgroup v2. Nothing here schedules, renders or sleeps.
//!
//! Two rules drive the code:
//!
//! - CPU time comes from `/proc/<pid>/task/*/schedstat` (nanoseconds), not from
//!   utime+stime at `CLK_TCK` resolution. At 120 Hz a lean compositor burns
//!   ~2 ms of CPU per second; 10 ms tick granularity cannot see that.
//! - The compositor is the process that owns the `WAYLAND_DISPLAY` listening
//!   socket, not "the oldest process with a known name". Nested compositors and
//!   multi-seat setups make the name heuristic pick the wrong process.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub fn read(path: &str) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

pub fn read_u64(path: &str) -> Option<u64> {
    read(path)?.parse().ok()
}

// ---------------------------------------------------------------- cgroup ---

/// The cgroup of this login session (`.../session-N.scope`), or the user slice
/// when the session has none (some greeters, TTY-started compositors).
pub fn session_cgroup() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let raw = read("/proc/self/cgroup").unwrap_or_default();
        let path = raw
            .lines()
            .find_map(|l| l.strip_prefix("0::"))
            .unwrap_or("/")
            .to_string();
        if let Some(idx) = path.find("/session-") {
            let rest = &path[idx..];
            if let Some(end) = rest.find(".scope") {
                let p = PathBuf::from("/sys/fs/cgroup").join(&path[1..idx + end + ".scope".len()]);
                if p.join("memory.current").exists() {
                    return p;
                }
            }
        }
        let uid = unsafe { getuid() };
        PathBuf::from(format!("/sys/fs/cgroup/user.slice/user-{uid}.slice"))
    })
}

/// False when the session has no `session-N.scope` and we fell back to the
/// user slice, which also contains other sessions and unrelated processes.
pub fn cgroup_exact() -> bool {
    session_cgroup()
        .file_name()
        .map(|n| n.to_string_lossy().starts_with("session-"))
        .unwrap_or(false)
}

pub fn cpu_usage_usec(dir: &Path) -> u64 {
    fs::read_to_string(dir.join("cpu.stat"))
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("usage_usec"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

/// One pass over `memory.stat` for every field we report. Reading the file once
/// per field was three reads of the same file per sample.
#[derive(Clone, Copy, Debug, Default)]
pub struct MemStat {
    pub anon: f64,
    pub file: f64,
    pub shmem: f64,
    pub kernel: f64,
    pub slab: f64,
    pub sock: f64,
}

pub fn mem_stat(dir: &Path) -> MemStat {
    let mut out = MemStat::default();
    let Ok(s) = fs::read_to_string(dir.join("memory.stat")) else {
        return out;
    };
    for line in s.lines() {
        let mut it = line.split_whitespace();
        let (Some(k), Some(v)) = (it.next(), it.next()) else {
            continue;
        };
        let Ok(v) = v.parse::<f64>() else { continue };
        let mib = v / 1048576.0;
        match k {
            "anon" => out.anon = mib,
            "file" => out.file = mib,
            "shmem" => out.shmem = mib,
            "kernel" => out.kernel = mib,
            "slab" => out.slab = mib,
            "sock" => out.sock = mib,
            _ => {}
        }
    }
    out
}

pub fn pids_in_cgroup(dir: &Path) -> Vec<u32> {
    fs::read_to_string(dir.join("cgroup.procs"))
        .map(|s| {
            s.split_whitespace()
                .filter_map(|p| p.parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// The kernel's own high-water mark for the session, in MiB. It is only
/// resettable by root, so it is reported as "since login" and never as the peak
/// of a single run.
pub fn memory_peak_mib(dir: &Path) -> f64 {
    read_u64(dir.join("memory.peak").to_string_lossy().as_ref()).unwrap_or(0) as f64 / 1048576.0
}

// ------------------------------------------------------------------ proc ---

#[derive(Clone, Debug, Default)]
pub struct Stat {
    pub ppid: u32,
}

/// `/proc/<pid>/stat`'s parent field. The command name may contain spaces and
/// parentheses, so the split has to go through the last `)`.
pub fn stat(pid: u32) -> Option<Stat> {
    let s = read(&format!("/proc/{pid}/stat"))?;
    let close = s.rfind(')')?;
    let ppid = s[close + 1..].split_whitespace().nth(1)?.parse().ok()?;
    Some(Stat { ppid })
}

pub fn comm(pid: u32) -> Option<String> {
    read(&format!("/proc/{pid}/comm"))
}

#[derive(Clone, Debug, Default)]
pub struct Task {
    pub tid: u32,
    pub comm: String,
    /// CPU time in nanoseconds, from `schedstat` field 1
    pub ns: u64,
}

/// Every thread of a process with its CPU time, busiest first. `schedstat` is
/// per-thread, so the process total is the sum over `/proc/<pid>/task/*`.
pub fn tasks(pid: u32) -> Vec<Task> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(format!("/proc/{pid}/task")) else {
        return out;
    };
    for e in rd.flatten() {
        let Some(tid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let ns = read(&format!("/proc/{pid}/task/{tid}/schedstat"))
            .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse().ok()))
            .unwrap_or(0);
        let comm = read(&format!("/proc/{pid}/task/{tid}/comm")).unwrap_or_default();
        out.push(Task { tid, comm, ns });
    }
    out.sort_by_key(|t| std::cmp::Reverse(t.ns));
    out
}

/// Process CPU time in nanoseconds: the sum over its threads.
pub fn cpu_ns(pid: u32) -> u64 {
    tasks(pid).iter().map(|t| t.ns).sum()
}

/// Proportional set size of one process, in KiB.
pub fn pss_kb(pid: u32) -> u64 {
    read(&format!("/proc/{pid}/smaps_rollup"))
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Pss:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}

/// Resident memory of every process on the machine, grouped by command name and
/// sorted largest first, in MiB.
///
/// `VmRSS` in `/proc/<pid>/status` is readable for other users' processes too,
/// unlike `smaps_rollup`, so the snapshot also covers daemons the session did not
/// start. Threads share their process's resident set, so the sum runs over
/// processes; the sum over *names* is what turns a dozen WebKit children into one
/// bar per application instead of a dozen.
pub fn ram_by_name() -> Vec<(String, f64)> {
    let mut by_name: HashMap<String, f64> = HashMap::new();
    let Ok(rd) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    for entry in rd.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let Some(kb) = read(&format!("/proc/{pid}/status")).and_then(|s| vm_rss_kb(&s)) else {
            continue;
        };
        if kb == 0 {
            continue;
        }
        let name = comm(pid).unwrap_or_else(|| format!("pid {pid}"));
        *by_name.entry(name).or_insert(0.0) += kb as f64 / 1024.0;
    }
    let mut out: Vec<(String, f64)> = by_name.into_iter().collect();
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    out
}

fn vm_rss_kb(status: &str) -> Option<u64> {
    status.lines().find_map(|line| {
        let rest = line.strip_prefix("VmRSS:")?;
        rest.split_whitespace().next()?.parse().ok()
    })
}

/// PSS of a process and all its descendants, in KiB. One pass over `/proc`
/// builds the parent map instead of re-scanning it for every node.
pub fn pss_tree(pid: u32) -> (u64, Vec<u32>) {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    if let Ok(rd) = fs::read_dir("/proc") {
        for e in rd.flatten() {
            let Some(p) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            let ppid = stat(p).map(|s| s.ppid);
            if let Some(ppid) = ppid {
                children.entry(ppid).or_default().push(p);
            }
        }
    }
    let mut total = 0;
    let mut seen = Vec::new();
    let mut visited = HashSet::new();
    let mut queue = vec![pid];
    while let Some(p) = queue.pop() {
        if !visited.insert(p) {
            continue;
        }
        seen.push(p);
        total += pss_kb(p);
        if let Some(kids) = children.get(&p) {
            queue.extend(kids);
        }
    }
    (total, seen)
}

/// Context switches and thread count from one read of `/proc/<pid>/status`.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcStatus {
    pub wakeups: u64,
    pub threads: u32,
}

pub fn status(pid: u32) -> ProcStatus {
    let Some(s) = read(&format!("/proc/{pid}/status")) else {
        return ProcStatus::default();
    };
    let mut out = ProcStatus::default();
    for line in s.lines() {
        if line.starts_with("voluntary_ctxt_switches")
            || line.starts_with("nonvoluntary_ctxt_switches")
        {
            if let Some(v) = line
                .split_whitespace()
                .nth(1)
                .and_then(|v| v.parse::<u64>().ok())
            {
                out.wakeups += v;
            }
        } else if line.starts_with("Threads:") {
            out.threads = line
                .split_whitespace()
                .nth(1)
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
        }
    }
    out
}

/// CPU time of one process in nanoseconds, from `utime + stime` in
/// `/proc/<pid>/stat`. Coarser than `schedstat` (10 ms ticks) but one file per
/// process, which is what a whole-`/proc` attribution pass needs.
pub fn cpu_time_ns(pid: u32) -> Option<u64> {
    let s = read(&format!("/proc/{pid}/stat"))?;
    let close = s.rfind(')')?;
    let mut it = s[close + 1..].split_whitespace();
    let utime: u64 = it.nth(11)?.parse().ok()?;
    let stime: u64 = it.next()?.parse().ok()?;
    Some((utime + stime) * 1_000_000_000 / clock_ticks())
}

fn clock_ticks() -> u64 {
    static TICKS: OnceLock<u64> = OnceLock::new();
    *TICKS.get_or_init(|| {
        extern "C" {
            fn sysconf(name: i32) -> i64;
        }
        const SC_CLK_TCK: i32 = 2;
        let v = unsafe { sysconf(SC_CLK_TCK) };
        if v > 0 {
            v as u64
        } else {
            100
        }
    })
}

/// The session's CPU counters at one instant: the cgroup total plus per-process
/// `utime + stime`, so a run can name what used the CPU besides the compositor.
#[derive(Clone, Debug, Default)]
pub struct CpuSnapshot {
    pub total_usec: u64,
    /// (pid, comm, CPU ns) for every process in the session cgroup
    pub procs: Vec<(u32, String, u64)>,
}

pub fn cpu_snapshot() -> CpuSnapshot {
    let dir = session_cgroup();
    let total_usec = cpu_usage_usec(dir);
    let procs = pids_in_cgroup(dir)
        .into_iter()
        .filter_map(|pid| {
            let ns = cpu_time_ns(pid)?;
            let name = comm(pid).unwrap_or_else(|| format!("pid {pid}"));
            Some((pid, name, ns))
        })
        .collect();
    CpuSnapshot { total_usec, procs }
}

// ------------------------------------------------------------------- gpu ---

/// Per-client GPU counters from `/proc/<pid>/fdinfo`. No root needed: the
/// driver exposes them to the process owner.
#[derive(Clone, Debug, Default)]
pub struct Gpu {
    /// drm-client-id -> (busy cycles, total cycles, absolute busy ns)
    pub clients: HashMap<u64, (u64, u64, u64)>,
    /// bytes held by this process's clients, shared buffers included
    pub resident: u64,
    /// the part of `resident` that is not marked shared, i.e. what this process
    /// alone holds. For a compositor that is its own framebuffers and textures,
    /// without the clients' buffers it imported.
    pub private: u64,
}

impl Gpu {
    pub fn available(&self) -> bool {
        !self.clients.is_empty()
    }
}

/// Leading number of a `key:\t<number> [unit]` value. The kernel prints units
/// on some keys (`126584 KiB`, `1234 ns`) and not on others.
fn parse_num(v: &str) -> Option<u64> {
    v.split_whitespace().next()?.parse().ok()
}

/// Memory counters carry a unit and it is not always the same one:
/// `drm-resident-gtt: 126584 KiB` next to `drm-shared-gtt: 48 MiB`.
fn parse_bytes(v: &str) -> Option<u64> {
    let mut it = v.split_whitespace();
    let n: f64 = it.next()?.parse().ok()?;
    let mult = match it.next().unwrap_or("KiB") {
        "B" => 1.0,
        "MiB" => 1048576.0,
        "GiB" => 1073741824.0,
        _ => 1024.0, // KiB, and the documented default for these keys
    };
    Some((n * mult) as u64)
}

/// Several fds can point at the same DRM client and report identical counters,
/// so values are merged per client id (max), never summed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct FdInfo {
    id: u64,
    cycles: u64,
    total_cycles: u64,
    engine_ns: u64,
    held: u64,
    shared: u64,
}

/// One fdinfo file. Several fds can point at the same DRM client and report
/// identical counters, so callers merge per `id` (max), never sum.
fn parse_fdinfo(text: &str) -> Option<FdInfo> {
    let mut id = None;
    let mut out = FdInfo::default();
    let mut resident_only = 0;
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        if k == "drm-client-id" {
            id = parse_num(v);
            continue;
        }
        // i915/amdgpu: absolute GPU nanoseconds for this client
        if k.starts_with("drm-engine-") {
            if let Some(n) = parse_num(v) {
                out.engine_ns = out.engine_ns.max(n);
            }
            continue;
        }
        // xe/i915: cycle counters, only meaningful as a ratio
        if k.starts_with("drm-total-cycles-") {
            if let Some(n) = parse_num(v) {
                out.total_cycles = out.total_cycles.max(n);
            }
            continue;
        }
        if k.starts_with("drm-cycles-") {
            if let Some(n) = parse_num(v) {
                out.cycles = out.cycles.max(n);
            }
            continue;
        }
        if k.starts_with("drm-shared-") || k.starts_with("drm-memory-shared") {
            if let Some(n) = parse_bytes(v) {
                out.shared += n;
            }
            continue;
        }
        // `drm-total-*` and `drm-resident-*` describe the same buffers; count
        // one family, never both.
        if k.starts_with("drm-total-") || k.starts_with("drm-memory-") {
            if let Some(n) = parse_bytes(v) {
                out.held += n;
            }
        } else if k.starts_with("drm-resident-") {
            if let Some(n) = parse_bytes(v) {
                resident_only += n;
            }
        }
    }
    if out.held == 0 {
        out.held = resident_only;
    }
    out.id = id?;
    Some(out)
}

pub fn gpu(pid: u32) -> Gpu {
    let mut out = Gpu::default();
    let Ok(rd) = fs::read_dir(format!("/proc/{pid}/fdinfo")) else {
        return out;
    };
    let mut per_client: HashMap<u64, FdInfo> = HashMap::new();
    for e in rd.flatten() {
        let Ok(s) = fs::read_to_string(e.path()) else {
            continue;
        };
        let Some(info) = parse_fdinfo(&s) else {
            continue;
        };
        let e = per_client.entry(info.id).or_default();
        e.cycles = e.cycles.max(info.cycles);
        e.total_cycles = e.total_cycles.max(info.total_cycles);
        e.engine_ns = e.engine_ns.max(info.engine_ns);
        e.held = e.held.max(info.held);
        e.shared = e.shared.max(info.shared);
    }
    for (id, c) in per_client {
        out.clients
            .insert(id, (c.cycles, c.total_cycles, c.engine_ns));
        out.resident += c.held;
        out.private += c.held.saturating_sub(c.shared);
    }
    out
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GpuCost {
    /// estimated GPU time attributable to these clients, in seconds
    pub busy_s: f64,
}

/// GPU time between two samples. When the driver only exposes cycle counters,
/// the ratio `busy / total` is unit-free, so it can be scaled by wall time for
/// each client separately and summed.
pub fn gpu_cost(a: &Gpu, b: &Gpu, elapsed_s: f64) -> Option<GpuCost> {
    if !b.available() || elapsed_s <= 0.0 {
        return None;
    }
    let mut busy = 0.0;
    let mut any = false;
    for (id, (cycles, total, engine)) in &b.clients {
        let Some((pc, pt, pe)) = a.clients.get(id) else {
            continue;
        };
        let dc = cycles.saturating_sub(*pc);
        let dt = total.saturating_sub(*pt);
        let de = engine.saturating_sub(*pe);
        if dt > 0 {
            // busy/total is a fraction of this client's lifetime; scaled by the
            // measurement window it becomes wall-clock GPU time.
            busy += dc as f64 / dt as f64 * elapsed_s;
            any = true;
        } else if de > 0 {
            busy += de as f64 / 1e9;
            any = true;
        }
    }
    any.then_some(GpuCost { busy_s: busy })
}

// ----------------------------------------------------------- compositor ----

#[derive(Clone, Debug)]
pub struct Compositor {
    pub pid: u32,
    pub name: String,
    pub source: &'static str,
}

pub const KNOWN: &[&str] = &[
    "cosmic-comp",
    "gnome-shell",
    "labwc",
    "Hyprland",
    "hyprland",
    "niri",
    "sway",
    "kwin_wayland",
    "weston",
    "wayfire",
    "river",
    "dwl",
    "miracle-wm",
    "gamescope",
];

/// The compositor serving `WAYLAND_DISPLAY`: the process holding the *listening*
/// socket. Connected client sockets have different inodes, so this is unique.
pub fn compositor() -> Option<Compositor> {
    socket_compositor().or_else(name_compositor)
}

fn socket_inode() -> Option<u64> {
    let name = std::env::var("WAYLAND_DISPLAY").ok()?;
    let path = if name.starts_with('/') {
        PathBuf::from(&name)
    } else {
        let dir = std::env::var("XDG_RUNTIME_DIR").ok()?;
        PathBuf::from(dir).join(&name)
    };
    // The kernel inode of a unix socket differs from the inode of its path in
    // the filesystem, so look the path up in /proc/net/unix.
    let table = fs::read_to_string("/proc/net/unix").ok()?;
    let want = path.to_string_lossy();
    table.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        let _ = it.next()?;
        let _ = it.next()?;
        let _ = it.next()?;
        let _ = it.next()?;
        let _ = it.next()?;
        let _ = it.next()?;
        let inode: u64 = it.next()?.parse().ok()?;
        let shown = it.next()?;
        (shown == want).then_some(inode)
    })
}

/// The desktop a compositor process belongs to. `comm` is the kernel's
/// 15-character name, and for GNOME it is the shell: mutter is a library inside
/// it, so `gnome-shell` is the process and "GNOME" is the desktop.
pub fn desktop_of(comm: &str) -> Option<&'static str> {
    Some(match comm {
        "gnome-shell" => "GNOME",
        "kwin_wayland" => "KDE Plasma",
        "cosmic-comp" => "COSMIC",
        "Hyprland" | "hyprland" => "Hyprland",
        "miracle-wm" => "Miracle WM",
        "labwc" => "labwc",
        "sway" => "sway",
        "niri" => "niri",
        "weston" => "Weston",
        "wayfire" => "Wayfire",
        "river" => "river",
        "dwl" => "dwl",
        "gamescope" => "gamescope",
        _ => return None,
    })
}

/// Session identity: what the desktop actually is, and what the environment
/// claims it is.
#[derive(Clone, Debug, Default)]
pub struct Desktop {
    /// what this run should be called: "GNOME", "KDE Plasma", "labwc", …
    pub name: String,
    /// the raw `XDG_CURRENT_DESKTOP`, kept for provenance
    pub env: String,
    /// the desktop derived from the process holding the socket, if known
    pub detected: Option<&'static str>,
}

impl Desktop {
    /// The environment names a different desktop than the socket owner. GNOME
    /// Shell runs as a systemd user service, so it inherits the user manager's
    /// environment: a GNOME session entered from COSMIC can still say
    /// `cosmic-comp-traffic:COSMIC` in every one of its processes.
    pub fn mismatch(&self) -> bool {
        match self.detected {
            Some(d) => !self.env.is_empty() && !env_names(&self.env, d),
            None => false,
        }
    }
}

/// `XDG_CURRENT_DESKTOP` names the desktop the run belongs to: "ubuntu:GNOME"
/// and "labwc:wlroots" do, and "cosmic-comp-traffic:COSMIC" does not when the
/// socket belongs to GNOME Shell. The environment wins when it is consistent,
/// because it carries the session flavour; the process wins when it is not.
pub fn desktop(comp: Option<&Compositor>) -> Desktop {
    let env = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let detected = comp.and_then(|c| desktop_of(&c.name));
    let name = match detected {
        Some(d) if env.is_empty() || !env_names(&env, d) => d.to_string(),
        _ if !env.is_empty() => env.clone(),
        _ => "unknown".into(),
    };
    Desktop {
        name,
        env,
        detected,
    }
}

fn env_names(env: &str, desktop: &str) -> bool {
    let want = desktop.to_ascii_lowercase();
    env.split(':').any(|s| {
        let s = s.trim().to_ascii_lowercase();
        !s.is_empty() && (want.contains(&s) || s.contains(&want))
    })
}

fn socket_compositor() -> Option<Compositor> {
    let inode = socket_inode()?;
    let needle = format!("socket:[{inode}]");
    let rd = fs::read_dir("/proc").ok()?;
    for e in rd.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(fds) = fs::read_dir(format!("/proc/{pid}/fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            if fs::read_link(fd.path())
                .ok()
                .map(|p| p.to_string_lossy() == needle.as_str())
                == Some(true)
            {
                return Some(Compositor {
                    pid,
                    name: comm(pid).unwrap_or_else(|| format!("pid {pid}")),
                    source: "socket",
                });
            }
        }
    }
    None
}

/// Fallback for compositors that hand out a display socket we cannot resolve:
/// oldest known name inside the session cgroup.
fn name_compositor() -> Option<Compositor> {
    let mut best: Option<(u32, String)> = None;
    for pid in pids_in_cgroup(session_cgroup()) {
        let Some(name) = comm(pid) else { continue };
        if KNOWN.contains(&name.as_str()) && best.as_ref().map(|(p, _)| pid < *p).unwrap_or(true) {
            best = Some((pid, name));
        }
    }
    best.map(|(pid, name)| Compositor {
        pid,
        name,
        source: "comm",
    })
}

/// `<binary> --version`, first line. Every compositor here exits immediately,
/// but one that ignores the flag would start a nested session and never
/// return, so the child is killed after two seconds instead of blocking the
/// window that is about to open.
///
/// The window and the run both ask for the same line, so the answer is cached:
/// spawning a compositor binary twice per start is unnecessary.
pub fn compositor_version(name: &str) -> Option<String> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(c) = cache.lock() {
        if let Some(v) = c.get(name) {
            return v.clone();
        }
    }
    let version = compositor_version_uncached(name);
    if let Ok(mut c) = cache.lock() {
        c.insert(name.to_string(), version.clone());
    }
    version
}

fn compositor_version_uncached(name: &str) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let mut child = Command::new(name)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut text = String::new();
    child.stdout.take()?.read_to_string(&mut text).ok()?;
    let line = text
        .lines()
        .next()
        .map(str::trim)
        .filter(|l| !l.is_empty())?;
    Some(line.chars().take(80).collect())
}

/// The software that implements the Wayland protocol in a session, as opposed
/// to the process that happens to host it: Mutter is a library inside
/// `gnome-shell`, and KWin is the compositor in a Plasma session. Names that
/// are already the implementation (sway, niri, labwc, …) pass through.
pub fn compositor_impl(comm: &str) -> &str {
    match comm {
        "gnome-shell" => "Mutter",
        "kwin_wayland" => "KWin",
        other => other,
    }
}

/// The release inside a `<binary> --version` line: "sway version 1.11" and
/// "GNOME Shell 51.1" yield "1.11" and "51.1". A line with no dotted number
/// yields `None` rather than a stray integer from a build banner. Mutter is a
/// library inside `gnome-shell` and shares its release, so the shell's line is
/// mutter's version too.
pub fn version_token(line: &str) -> Option<String> {
    line.split_whitespace().find_map(|raw| {
        let t = raw.trim_matches(|c: char| !c.is_ascii_digit() && c != '.');
        let t = t.trim_end_matches('.');
        let dotted = t.contains('.')
            && t.chars().all(|c| c.is_ascii_digit() || c == '.')
            && t.split('.').all(|p| !p.is_empty());
        dotted.then(|| t.to_string())
    })
}

/// Short hash of the compositor's configuration, so "labwc" and "labwc with
/// blur and 1.75x scaling" do not end up in the same median.
pub fn config_hash(name: &str) -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let candidates: &[&str] = match name {
        "labwc" => &["labwc"],
        "Hyprland" | "hyprland" => &["hypr"],
        "niri" => &["niri"],
        "sway" => &["sway"],
        "kwin_wayland" => &["kwinrc", "kwinrulesrc"],
        "wayfire" => &["wayfire.ini"],
        _ => &[],
    };
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for c in candidates {
        let base = PathBuf::from(&home).join(".config").join(c);
        if base.is_dir() {
            collect_files(&base, 0, &mut entries);
        } else if base.is_file() {
            if let Ok(bytes) = fs::read(&base) {
                entries.push((c.to_string(), bytes));
            }
        }
    }
    if entries.is_empty() {
        return None;
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for (name, bytes) in &entries {
        for b in name.as_bytes().iter().chain(bytes.iter()) {
            h ^= *b as u64;
            h = h.wrapping_mul(0x1000_0000_01b3);
        }
    }
    Some(format!("{:08x}", (h >> 32) as u32 ^ h as u32))
}

fn collect_files(dir: &Path, depth: u32, out: &mut Vec<(String, Vec<u8>)>) {
    if depth > 2 || out.len() > 64 {
        return;
    }
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let path = e.path();
        if path.is_dir() {
            collect_files(&path, depth + 1, out);
        } else if let Ok(bytes) = fs::read(&path) {
            if bytes.len() < 512 * 1024 {
                out.push((path.to_string_lossy().into_owned(), bytes));
            }
        }
    }
}

extern "C" {
    fn getuid() -> u32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_of_this_process_parses() {
        let s = stat(std::process::id()).expect("own stat");
        assert!(s.ppid > 0, "ppid");
    }

    #[test]
    fn the_ram_snapshot_is_sorted_and_contains_this_process() {
        let items = ram_by_name();
        assert!(!items.is_empty(), "no process had a resident set");
        assert!(
            items.windows(2).all(|w| w[0].1 >= w[1].1),
            "grouped snapshot is not sorted: {:?}",
            &items[..items.len().min(5)]
        );
        let me = comm(std::process::id()).expect("own comm");
        let mine = items
            .iter()
            .find(|(name, _)| *name == me)
            .unwrap_or_else(|| panic!("{me} missing from the snapshot"));
        assert!(mine.1 > 0.0, "{me} has no resident set");
    }

    #[test]
    fn cpu_ns_is_at_least_the_busiest_thread() {
        let tasks = tasks(std::process::id());
        let all: u64 = tasks.iter().map(|t| t.ns).sum();
        let busiest = tasks.first().map(|t| t.ns).unwrap_or(0);
        assert!(all >= busiest);
        assert!(all > 0, "schedstat should be readable");
    }

    #[test]
    fn session_cgroup_exists_on_linux() {
        let dir = session_cgroup();
        // Containers and CI runners have a cgroup hierarchy but no login
        // session scope: nothing to assert, so skip rather than fail.
        if !dir.join("memory.current").exists() {
            eprintln!("skipping: {} has no memory.current", dir.display());
            return;
        }
        assert!(dir.join("memory.current").exists());
    }

    #[test]
    fn memory_units_are_honoured() {
        assert_eq!(parse_bytes("126584 KiB"), Some(126_584 * 1024));
        assert_eq!(parse_bytes("48 MiB"), Some(48 * 1_048_576));
        assert_eq!(parse_bytes("0 KiB"), Some(0));
        assert_eq!(parse_num("1234 ns"), Some(1234));
        assert_eq!(parse_num("2139146141780"), Some(2_139_146_141_780));
    }

    #[test]
    fn fdinfo_reports_held_and_shared_without_double_counting() {
        // Real layout from an Xe GPU: total and resident describe the same
        // buffers, and the shared part is a subset.
        let text = "\
drm-driver:\txe
drm-client-id:\t5218
drm-pdev:\t0000:00:02.0
drm-total-system:\t0
drm-shared-system:\t0
drm-resident-system:\t0
drm-total-gtt:\t126584 KiB
drm-shared-gtt:\t48 MiB
drm-resident-gtt:\t126584 KiB
drm-cycles-rcs:\t1589032
drm-total-cycles-rcs:\t2139146184137
";
        let info = parse_fdinfo(text).expect("fdinfo");
        assert_eq!(info.id, 5218);
        assert_eq!(info.held, 126_584 * 1024, "total-gtt, counted once");
        assert_eq!(info.shared, 48 * 1_048_576);
        assert_eq!(info.cycles, 1_589_032);
        assert_eq!(info.total_cycles, 2_139_146_184_137);
    }

    #[test]
    fn fdinfo_without_a_client_id_is_ignored() {
        assert!(parse_fdinfo("pos:\t0\nflags:\t02\n").is_none());
    }

    #[test]
    fn gpu_cost_needs_two_samples() {
        let a = Gpu::default();
        assert!(gpu_cost(&a, &a, 1.0).is_none());
        let mut b = Gpu::default();
        b.clients.insert(1, (100, 1000, 0));
        // client missing from the earlier sample: no delta, no estimate
        assert!(gpu_cost(&a, &b, 1.0).is_none());
    }

    #[test]
    fn gpu_cost_uses_the_busy_ratio() {
        let mut a = Gpu::default();
        let mut b = Gpu::default();
        a.clients.insert(7, (100, 1_000, 0));
        b.clients.insert(7, (400, 4_000, 0));
        // busy grew 300 while total grew 3000 -> 10% of a 2 s window
        let c = gpu_cost(&a, &b, 2.0).unwrap();
        assert!((c.busy_s - 0.2).abs() < 1e-9);
    }

    #[test]
    fn desktop_is_derived_from_the_compositor_process() {
        assert_eq!(desktop_of("gnome-shell"), Some("GNOME"));
        assert_eq!(desktop_of("kwin_wayland"), Some("KDE Plasma"));
        assert_eq!(desktop_of("labwc"), Some("labwc"));
        assert_eq!(desktop_of("Alacritty"), None);
    }

    #[test]
    fn compositor_implementation_is_not_the_process_name() {
        assert_eq!(compositor_impl("gnome-shell"), "Mutter");
        assert_eq!(compositor_impl("kwin_wayland"), "KWin");
        assert_eq!(compositor_impl("sway"), "sway");
        assert_eq!(compositor_impl("cosmic-comp"), "cosmic-comp");
    }

    #[test]
    fn version_is_pulled_out_of_a_version_line() {
        assert_eq!(version_token("GNOME Shell 51.1").as_deref(), Some("51.1"));
        assert_eq!(version_token("sway version 1.11").as_deref(), Some("1.11"));
        assert_eq!(
            version_token("kwin 6.4.5 (compiled from ...)").as_deref(),
            Some("6.4.5")
        );
        assert_eq!(
            version_token("Hyprland, built from branch main at commit 1a2b3c (version: v0.51.0)")
                .as_deref(),
            Some("0.51.0")
        );
        // a build number or a word with a dot in it is not a release
        assert_eq!(version_token("niri built 1234"), None);
        assert_eq!(version_token("built from branch main"), None);
    }

    #[test]
    fn environment_names_a_desktop_only_when_it_agrees() {
        assert!(env_names("ubuntu:GNOME", "GNOME"));
        assert!(env_names("labwc:wlroots", "labwc"));
        assert!(env_names("KDE", "KDE Plasma"));
        assert!(!env_names("cosmic-comp-traffic:COSMIC", "GNOME"));
        assert!(!env_names("", "GNOME"));
    }

    #[test]
    fn a_stale_environment_is_flagged_and_not_adopted() {
        // GNOME Shell started from the user manager inheriting COSMIC's env
        let stale = Desktop {
            name: "GNOME".into(),
            env: "cosmic-comp-traffic:COSMIC".into(),
            detected: Some("GNOME"),
        };
        assert!(stale.mismatch());
        let consistent = Desktop {
            name: "ubuntu:GNOME".into(),
            env: "ubuntu:GNOME".into(),
            detected: Some("GNOME"),
        };
        assert!(!consistent.mismatch());
        // an unknown process cannot contradict the environment
        let unknown = Desktop {
            name: "COSMIC".into(),
            env: "COSMIC".into(),
            detected: None,
        };
        assert!(!unknown.mismatch());
    }
}
