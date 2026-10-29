//! Hardware, display and session identity: the context that decides whether two
//! runs may be compared at all. Measured quantities live in `proc` and `power`.

use std::collections::HashMap;
use std::fs;

use serde::{Deserialize, Serialize};

use crate::proc;

pub fn read(path: &str) -> Option<String> {
    proc::read(path)
}

// ------------------------------------------------------------- snapshots ---

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    /// `memory.current`: the whole cgroup, page cache included
    pub total_mib: f64,
    pub anon_mib: f64,
    pub shmem_mib: f64,
    pub file_mib: f64,
    pub kernel_mib: f64,
    pub swap_mib: f64,
    pub cpu_usec: u64,
    pub comp_ns: u64,
    /// compositor threads with their nanosecond CPU time, at sample time
    pub comp_tasks: Vec<proc::Task>,
    pub comp_wakeups: u64,
    pub procs: usize,
    pub comp_pss_mib: f64,
}

impl Snapshot {
    /// What the session actually holds in memory: heap and shared buffers.
    /// Page cache is excluded because it follows whatever the session read,
    /// not what the desktop costs.
    pub fn working_set_mib(&self) -> f64 {
        self.anon_mib + self.shmem_mib
    }

    pub fn take(comp_pid: Option<u32>) -> Self {
        let dir = proc::session_cgroup();
        let m = proc::mem_stat(dir);
        Self {
            total_mib: proc::read_u64(dir.join("memory.current").to_string_lossy().as_ref())
                .unwrap_or(0) as f64
                / 1048576.0,
            anon_mib: m.anon,
            shmem_mib: m.shmem,
            file_mib: m.file,
            kernel_mib: m.kernel,
            swap_mib: proc::read_u64(dir.join("memory.swap.current").to_string_lossy().as_ref())
                .unwrap_or(0) as f64
                / 1048576.0,
            cpu_usec: proc::cpu_usage_usec(dir),
            comp_ns: comp_pid.map(proc::cpu_ns).unwrap_or(0),
            comp_tasks: comp_pid.map(proc::tasks).unwrap_or_default(),
            comp_wakeups: comp_pid.map(proc::wakeups).unwrap_or(0),
            procs: proc::pids_in_cgroup(dir).len(),
            comp_pss_mib: comp_pid
                .map(|p| proc::pss_tree(p).0 as f64 / 1024.0)
                .unwrap_or(0.0),
        }
    }

    /// (session CPU % of one core, compositor CPU % of one core)
    pub fn delta_cpu(&self, after: &Snapshot, seconds: f64) -> (f64, f64) {
        if seconds <= 0.0 {
            return (0.0, 0.0);
        }
        let cpu = after.cpu_usec.saturating_sub(self.cpu_usec) as f64 / 1e6 / seconds * 100.0;
        let comp = after.comp_ns.saturating_sub(self.comp_ns) as f64 / 1e9 / seconds * 100.0;
        (cpu, comp)
    }
}

/// Processes big enough to make a run incomparable, excluding the compositor and
/// the benchmark itself.
pub fn contaminants(exclude: &[u32]) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    for pid in proc::pids_in_cgroup(proc::session_cgroup()) {
        if exclude.contains(&pid) {
            continue;
        }
        let mib = proc::pss_kb(pid) as f64 / 1024.0;
        if mib >= 150.0 {
            out.push((proc::comm(pid).unwrap_or_else(|| format!("pid {pid}")), mib));
        }
    }
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    out
}

// --------------------------------------------------------------- display ---

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Display {
    pub connector: String,
    /// preferred (physical) mode from DRM
    pub mode: String,
    /// refresh derived from the EDID detailed timing, independent of the compositor
    pub refresh_hz: Option<f64>,
    /// what the compositor says it is presenting at
    pub compositor_hz: Option<f64>,
    pub logical_w: u32,
    pub logical_h: u32,
    pub scale: f64,
    pub outputs: usize,
    pub brightness: Option<u64>,
    pub brightness_max: Option<u64>,
}

pub struct DisplayHint {
    pub logical_w: u32,
    pub logical_h: u32,
    pub scale: f64,
    pub compositor_hz: Option<f64>,
}

impl Default for DisplayHint {
    fn default() -> Self {
        Self {
            logical_w: 0,
            logical_h: 0,
            scale: 0.0,
            compositor_hz: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Output {
    pub connector: String,
    pub connected: bool,
    pub mode: Option<String>,
}

pub fn outputs() -> Vec<Output> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir("/sys/class/drm") else {
        return out;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.contains('-') {
            continue;
        }
        let base = e.path();
        let connected =
            proc::read(&base.join("status").to_string_lossy()) == Some("connected".into());
        let mode = proc::read(&base.join("modes").to_string_lossy())
            .and_then(|m| m.lines().next().map(str::to_string));
        out.push(Output {
            connector: name,
            connected,
            mode,
        });
    }
    out.sort_by(|a, b| a.connector.cmp(&b.connector));
    out
}

/// Refresh of a connector's preferred mode, computed from the first detailed
/// timing descriptor in the EDID. Independent of what the compositor claims.
fn edid_refresh(connector: &str) -> Option<f64> {
    let b = fs::read(format!("/sys/class/drm/{connector}/edid")).ok()?;
    if b.len() < 72 {
        return None;
    }
    let d = &b[54..72];
    let pixclk = u16::from_le_bytes([d[0], d[1]]) as f64 * 10_000.0;
    if pixclk <= 0.0 {
        return None;
    }
    let hact = d[2] as f64 + (d[4] & 0xf0) as f64 * 16.0;
    let hblank = d[3] as f64 + (d[4] & 0x0f) as f64 * 256.0;
    let vact = d[5] as f64 + (d[7] & 0xf0) as f64 * 16.0;
    let vblank = d[6] as f64 + (d[7] & 0x0f) as f64 * 256.0;
    if hact + hblank <= 0.0 || vact + vblank <= 0.0 {
        return None;
    }
    Some(pixclk / (hact + hblank) / (vact + vblank))
}

pub fn display(hint: DisplayHint) -> Display {
    let outs = outputs();
    let connected: Vec<&Output> = outs.iter().filter(|o| o.connected).collect();
    let first = connected.first();
    let backlight = backlight();
    Display {
        connector: first.map(|o| o.connector.clone()).unwrap_or_default(),
        mode: first.and_then(|o| o.mode.clone()).unwrap_or_default(),
        refresh_hz: first.and_then(|o| edid_refresh(&o.connector)),
        compositor_hz: hint.compositor_hz,
        logical_w: hint.logical_w,
        logical_h: hint.logical_h,
        scale: hint.scale,
        outputs: connected.len(),
        brightness: backlight.as_ref().map(|b| b.value),
        brightness_max: backlight.as_ref().map(|b| b.max),
    }
}

pub struct Backlight {
    pub name: String,
    pub value: u64,
    pub max: u64,
}

pub fn backlight() -> Option<Backlight> {
    let rd = fs::read_dir("/sys/class/backlight").ok()?;
    // Only the first entry is ever inspected: the panel backlight.
    let e = rd.flatten().next()?;
    let dir = e.path();
    let value = proc::read_u64(&dir.join("brightness").to_string_lossy())?;
    let max = proc::read_u64(&dir.join("max_brightness").to_string_lossy()).unwrap_or(0);
    Some(Backlight {
        name: e.file_name().to_string_lossy().to_string(),
        value,
        max,
    })
}

/// Current physical mode of the first connected output.
pub fn drm_mode() -> Option<(String, i64)> {
    let outs = outputs();
    let o = outs.iter().find(|o| o.connected)?;
    let mode = o.mode.clone()?;
    let w = mode.split('x').next()?.parse().ok()?;
    Some((mode, w))
}

// ------------------------------------------------------------- system -----

pub fn performance_mode() -> String {
    match read("/sys/firmware/acpi/platform_profile").as_deref() {
        Some("low-power") => "Low power".into(),
        Some("balanced") => "Balanced".into(),
        Some("performance") => "Performance".into(),
        Some(other) if !other.is_empty() => other.into(),
        _ => cpu_governor().unwrap_or_else(|| "unknown".into()),
    }
}

pub fn performance_mode_choices() -> String {
    read("/sys/firmware/acpi/platform_profile_choices").unwrap_or_default()
}

pub fn cpu_governor() -> Option<String> {
    read("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
}

pub fn energy_preference() -> Option<String> {
    read("/sys/devices/system/cpu/cpu0/cpufreq/energy_performance_preference")
}

/// Mean current CPU frequency across cores, in GHz.
pub fn cpu_current_ghz() -> Option<f64> {
    let rd = fs::read_dir("/sys/devices/system/cpu").ok()?;
    let mut sum = 0u64;
    let mut n = 0u64;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(num) = name.strip_prefix("cpu") else {
            continue;
        };
        if num.is_empty() || !num.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if let Some(v) = proc::read_u64(&format!(
            "/sys/devices/system/cpu/{name}/cpufreq/scaling_cur_freq"
        )) {
            sum += v;
            n += 1;
        }
    }
    (n > 0).then(|| sum as f64 / n as f64 / 1_000_000.0)
}

/// Package temperature, if a zone names itself one.
pub fn package_temp_c() -> Option<f64> {
    let rd = fs::read_dir("/sys/class/thermal").ok()?;
    for e in rd.flatten() {
        let dir = e.path();
        let Ok(kind) = fs::read_to_string(dir.join("type")) else {
            continue;
        };
        let kind = kind.trim();
        if !(kind.contains("pkg") || kind.contains("x86_pkg") || kind == "TCPU") {
            continue;
        }
        if let Some(raw) = proc::read_u64(&dir.join("temp").to_string_lossy()) {
            return Some(raw as f64 / 1000.0);
        }
    }
    None
}

/// True when the kernel exposes RAPL throttle counters and any of them is set.
pub fn throttled() -> bool {
    let mut any = false;
    for f in ["core_throttle", "package_throttle"] {
        if let Some(v) = proc::read_u64(&format!("/sys/class/powercap/intel-rapl:0/{f}")) {
            any |= v > 0;
        }
    }
    any
}

pub fn psi(c: &std::path::Path, file: &str) -> Option<(f64, f64)> {
    let s = fs::read_to_string(c.join(file)).ok()?;
    let mut some = 0.0;
    let mut full = 0.0;
    for line in s.lines() {
        let mut it = line.split_whitespace();
        let kind = it.next()?;
        let avg10 = it
            .find_map(|kv| kv.strip_prefix("avg10="))
            .and_then(|v| v.parse::<f64>().ok())?;
        if kind == "some" {
            some = avg10;
        } else if kind == "full" {
            full = avg10;
        }
    }
    Some((some, full))
}

pub fn total_ram_mib() -> (f64, f64) {
    let s = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let get = |k: &str| {
        s.lines()
            .find(|l| l.starts_with(k))
            .and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok())
            .unwrap_or(0.0)
            / 1024.0
    };
    (get("MemTotal:"), get("MemAvailable:"))
}

pub fn cpu_model() -> String {
    read("/proc/cpuinfo")
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("model name"))
                .map(|l| l.split(':').nth(1).unwrap_or("").trim().to_string())
        })
        .unwrap_or_else(|| "unknown".into())
}

pub fn cpu_topology() -> (usize, usize) {
    let s = read("/proc/cpuinfo").unwrap_or_default();
    let mut threads = 0usize;
    let mut physical = String::new();
    let mut cores = std::collections::HashSet::new();
    for line in s.lines() {
        if line.starts_with("processor") {
            threads += 1;
        } else if let Some(v) = line.strip_prefix("physical id") {
            physical = v.trim_start_matches(':').trim().to_string();
        } else if let Some(v) = line.strip_prefix("core id") {
            cores.insert((
                physical.clone(),
                v.trim_start_matches(':').trim().to_string(),
            ));
        }
    }
    let cores = if cores.is_empty() {
        threads
    } else {
        cores.len()
    };
    (cores, threads)
}

pub fn cpu_max_ghz() -> Option<f64> {
    let mut best = 0u64;
    let rd = fs::read_dir("/sys/devices/system/cpu").ok()?;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(num) = name.strip_prefix("cpu") else {
            continue;
        };
        if num.is_empty() || !num.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if let Some(v) = proc::read_u64(&format!(
            "/sys/devices/system/cpu/{name}/cpufreq/cpuinfo_max_freq"
        )) {
            best = best.max(v);
        }
    }
    (best > 0).then(|| best as f64 / 1_000_000.0)
}

pub fn os_name() -> String {
    let s = read("/etc/os-release").unwrap_or_default();
    for line in s.lines() {
        if let Some(v) = line.strip_prefix("PRETTY_NAME=") {
            let v = v.trim().trim_matches('"');
            if !v.is_empty() {
                return v.to_string();
            }
        }
    }
    "unknown".into()
}

#[derive(Clone, Debug, Default)]
pub struct GpuInfo {
    pub name: String,
    pub driver: String,
    pub pci_id: String,
    pub pci_slot: String,
}

/// Human name for a PCI id from `/usr/share/hwdata/pci.ids`, when present.
/// Falls back to the bare id: a wrong name is worse than none.
fn pci_name(id: &str) -> Option<String> {
    let (vendor, device) = id.split_once(':')?;
    let text = std::fs::read_to_string("/usr/share/hwdata/pci.ids").ok()?;
    let mut in_vendor = false;
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if !line.starts_with('\t') {
            let Some(v) = line.split_whitespace().next() else {
                continue;
            };
            in_vendor = v.eq_ignore_ascii_case(vendor);
            continue;
        }
        if in_vendor && !line.starts_with("\t\t") {
            let mut it = line.trim_start().splitn(2, char::is_whitespace);
            let Some(dev) = it.next() else { continue };
            if dev.eq_ignore_ascii_case(device) {
                let name = it.next().unwrap_or("").trim();
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
}

pub fn gpu_info() -> GpuInfo {
    let uevent = read("/sys/class/drm/card0/device/uevent").unwrap_or_default();
    let mut map = HashMap::new();
    for line in uevent.lines() {
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.to_string(), v.to_string());
        }
    }
    let driver = map.get("DRIVER").cloned().unwrap_or_default();
    let id = map.get("PCI_ID").cloned().unwrap_or_default();
    let slot = map.get("PCI_SLOT_NAME").cloned().unwrap_or_default();
    // Prefer the name the kernel gives us; the PCI id is what identifies the
    // part across machines.
    let name = map
        .get("DRM_NAME")
        .cloned()
        .filter(|n| !n.is_empty() && n != "unknown")
        .unwrap_or_else(|| {
            if id.is_empty() {
                "unknown GPU".into()
            } else {
                pci_name(&id).unwrap_or_else(|| format!("GPU {id}"))
            }
        });
    GpuInfo {
        name,
        driver,
        pci_id: id,
        pci_slot: slot,
    }
}

pub fn mesa_version() -> String {
    for dir in [
        "/usr/lib",
        "/usr/lib64",
        "/usr/lib/x86_64-linux-gnu",
        "/usr/local/lib",
    ] {
        let Ok(rd) = fs::read_dir(dir) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(rest) = name.strip_prefix("libgallium-") {
                let v = rest.split(".so").next().unwrap_or(rest);
                return v.split('-').next().unwrap_or(v).to_string();
            }
        }
    }
    "unknown".into()
}

pub fn fmt_mib(v: f64) -> String {
    if v >= 1024.0 {
        format!("{:.2} GiB", v / 1024.0)
    } else {
        format!("{v:.0} MiB")
    }
}

/// Working set first, the whole cgroup in brackets: the gap is page cache, and
/// confusing the two is what makes naive "memory usage" comparisons jump.
pub fn fmt_mem(s: &Snapshot) -> String {
    format!(
        "{} (cgroup {})",
        fmt_mib(s.working_set_mib()),
        fmt_mib(s.total_mib)
    )
}

pub fn fmt_pct(v: f64) -> String {
    if v < 1.0 {
        format!("{v:.2}%")
    } else {
        format!("{v:.1}%")
    }
}

// ------------------------------------------------------------------ env ----

/// Everything that must match for two runs to be comparable.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Env {
    pub cpu: String,
    pub cpu_cores: usize,
    pub cpu_threads: usize,
    pub cpu_max_ghz: Option<f64>,
    pub gpu: String,
    pub gpu_driver: String,
    pub gpu_pci: String,
    pub mesa: String,
    pub os: String,
    pub kernel: String,
    pub display: Display,
    pub profile: String,
    pub governor: String,
    pub epp: String,
    pub temp_c: Option<f64>,
    pub cpu_ghz: Option<f64>,
    pub throttled: bool,
    /// short hash of the compositor's own configuration
    pub config_hash: Option<String>,
}

impl Env {
    pub fn collect(hint: DisplayHint, compositor: Option<&str>) -> Self {
        let (cores, threads) = cpu_topology();
        let gpu = gpu_info();
        Self {
            cpu: cpu_model(),
            cpu_cores: cores,
            cpu_threads: threads,
            cpu_max_ghz: cpu_max_ghz(),
            gpu: gpu.name,
            gpu_driver: gpu.driver,
            gpu_pci: gpu.pci_id,
            mesa: mesa_version(),
            os: os_name(),
            kernel: read("/proc/sys/kernel/osrelease").unwrap_or_default(),
            display: display(hint),
            profile: performance_mode(),
            governor: cpu_governor().unwrap_or_default(),
            epp: energy_preference().unwrap_or_default(),
            temp_c: package_temp_c(),
            cpu_ghz: cpu_current_ghz(),
            throttled: throttled(),
            config_hash: compositor.and_then(proc::config_hash),
        }
    }

    /// Percent difference between the display side of two runs, for the report.
    pub fn display_delta(&self, other: &Env) -> Option<String> {
        let (a, b) = (&self.display, &other.display);
        let mut diffs = Vec::new();
        if a.mode != b.mode {
            diffs.push(format!("mode {} vs {}", a.mode, b.mode));
        }
        let hz = |d: &Display| d.refresh_hz.or(d.compositor_hz).unwrap_or(0.0);
        if (hz(a) - hz(b)).abs() > 0.5 {
            diffs.push(format!("refresh {:.0} vs {:.0} Hz", hz(a), hz(b)));
        }
        if (a.scale - b.scale).abs() > 0.01 && a.scale > 0.0 && b.scale > 0.0 {
            diffs.push(format!("scale {:.2} vs {:.2}", a.scale, b.scale));
        }
        if let (Some(x), Some(y)) = (a.brightness, b.brightness) {
            if x.abs_diff(y) > 3 {
                diffs.push(format!("brightness {x} vs {y}"));
            }
        }
        (!diffs.is_empty()).then(|| diffs.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_has_a_session_and_no_nan() {
        let s = Snapshot::take(None);
        // A container or a CI runner has no login session scope, so there is
        // no working set to assert on: skip instead of failing a build for a
        // property of the machine, not of the code.
        if !proc::session_cgroup().join("memory.current").exists() {
            eprintln!("skipping: no session cgroup on this machine");
            return;
        }
        assert!(s.working_set_mib() > 0.0);
        assert!(s.cpu_usec > 0);
        assert!(!s.working_set_mib().is_nan());
    }

    #[test]
    fn display_delta_notices_a_refresh_change() {
        let mut a = Env::default();
        let mut b = Env::default();
        a.display.mode = "3120x2080".into();
        b.display.mode = "3120x2080".into();
        a.display.refresh_hz = Some(120.0);
        b.display.refresh_hz = Some(60.0);
        assert!(a.display_delta(&b).is_some());
        b.display.refresh_hz = Some(120.0);
        assert!(a.display_delta(&b).is_none());
    }
}
