//! Hardware, display and session identity: the context that decides whether two
//! runs may be compared at all. Measured quantities live in `proc` and `power`.

use std::collections::HashMap;
use std::fs;
use std::sync::OnceLock;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::proc;

/// A process holding this much PSS makes a run incomparable: it competes for
/// RAM and, usually, CPU. One constant so `--check`, the run and `--report`
/// cannot disagree about what "clean" means.
pub const CONTAMINANT_MIB: f64 = 150.0;

pub fn read(path: &str) -> Option<String> {
    proc::read(path)
}

// ------------------------------------------------------------- snapshots ---

#[derive(Clone, Debug)]
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
    /// (pid, comm, CPU ns) for the session at the same instant as `cpu_usec`
    pub cpu_procs: Vec<(u32, String, u64)>,
    /// when the counters above were read; the wall-clock window is derived from
    /// this, never from the requested CLI duration
    pub at: Instant,
}

impl Snapshot {
    /// What the session actually holds in memory: heap and shared buffers.
    /// Page cache is excluded because it follows whatever the session read,
    /// not what the desktop costs.
    pub fn working_set_mib(&self) -> f64 {
        self.anon_mib + self.shmem_mib
    }

    /// One-shot snapshot for callers that only need a value at one instant.
    /// It uses the start ordering, so the expensive scans are outside the
    /// interval a subsequent snapshot will measure.
    pub fn take(comp_pid: Option<u32>) -> Self {
        Self::begin(comp_pid)
    }

    /// Start of a measured window: scan first, then read the counters, then
    /// timestamp. The scan is therefore *before* the window, not inside it.
    pub fn begin(comp_pid: Option<u32>) -> Self {
        let dir = proc::session_cgroup();
        let mut snap = Self::take_light();
        snap.procs = proc::pids_in_cgroup(dir).len();
        snap.comp_pss_mib = comp_pid
            .map(|p| proc::pss_tree(p).0 as f64 / 1024.0)
            .unwrap_or(0.0);
        snap.cpu_procs = proc::cpu_procs();
        snap.comp_tasks = comp_pid.map(proc::tasks).unwrap_or_default();
        snap.comp_ns = snap.comp_tasks.iter().map(|t| t.ns).sum();
        snap.comp_wakeups = comp_pid.map(proc::process_wakeups).unwrap_or(0);
        // Counters last: everything above is the benchmark's own work and is
        // deliberately excluded from the interval.
        snap.cpu_usec = proc::cpu_usage_usec(dir);
        snap.at = Instant::now();
        snap
    }

    /// End of a measured window: read the counters first, then scan, then keep
    /// the timestamp taken before the scan. The scan is *after* the window.
    pub fn end(comp_pid: Option<u32>) -> Self {
        let dir = proc::session_cgroup();
        let cpu_usec = proc::cpu_usage_usec(dir);
        let at = Instant::now();
        let mut snap = Self::take_light();
        snap.cpu_usec = cpu_usec;
        snap.at = at;
        snap.cpu_procs = proc::cpu_procs();
        snap.comp_tasks = comp_pid.map(proc::tasks).unwrap_or_default();
        snap.comp_ns = snap.comp_tasks.iter().map(|t| t.ns).sum();
        snap.comp_wakeups = comp_pid.map(proc::process_wakeups).unwrap_or(0);
        snap.procs = proc::pids_in_cgroup(dir).len();
        snap.comp_pss_mib = comp_pid
            .map(|p| proc::pss_tree(p).0 as f64 / 1024.0)
            .unwrap_or(0.0);
        snap
    }

    /// The cgroup counters the 1 s live labels need, without the `/proc` walk
    /// and `smaps_rollup` that make `take` expensive.
    pub fn take_light() -> Self {
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
            comp_ns: 0,
            comp_tasks: Vec::new(),
            comp_wakeups: 0,
            procs: 0,
            comp_pss_mib: 0.0,
            cpu_procs: Vec::new(),
            at: Instant::now(),
        }
    }

    /// Same cgroup total and per-process baseline as this snapshot, for
    /// attribution over exactly this window rather than a nearby settle phase.
    pub fn cpu_snapshot(&self) -> proc::CpuSnapshot {
        proc::CpuSnapshot {
            total_usec: self.cpu_usec,
            procs: self.cpu_procs.clone(),
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
        if mib >= CONTAMINANT_MIB {
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
    /// The rest of the machine's GPUs, as "name · driver".
    pub others: Vec<String>,
}

/// Human name for a PCI id from the pci.ids database. Distros disagree on
/// where it lives: hwdata on Arch/Fedora, misc on Debian, pciutils elsewhere.
/// Falls back to the bare id: a wrong name is worse than none.
///
/// The file is ~1.6 MB and was re-scanned per card, twice per start. Parse it
/// once into a map of the ids it actually contains.
fn pci_name(id: &str) -> Option<String> {
    static IDS: OnceLock<Option<HashMap<(String, String), String>>> = OnceLock::new();
    let ids = IDS.get_or_init(|| {
        for path in [
            "/usr/share/hwdata/pci.ids",
            "/usr/share/misc/pci.ids",
            "/usr/share/pci.ids",
            "/var/lib/pciutils/pci.ids",
        ] {
            let Ok(text) = fs::read_to_string(path) else {
                continue;
            };
            let mut map = HashMap::new();
            let mut vendor = String::new();
            for line in text.lines() {
                if line.starts_with('#') || line.trim().is_empty() {
                    continue;
                }
                if !line.starts_with('\t') {
                    vendor = line
                        .split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    continue;
                }
                if line.starts_with("\t\t") || vendor.is_empty() {
                    continue;
                }
                let mut it = line.trim_start().splitn(2, char::is_whitespace);
                let Some(dev) = it.next() else { continue };
                let name = it.next().unwrap_or("").trim();
                if !name.is_empty() {
                    map.insert((vendor.clone(), dev.to_ascii_lowercase()), name.to_string());
                }
            }
            return Some(map);
        }
        None
    });
    let (vendor, device) = id.split_once(':')?;
    ids.as_ref()?
        .get(&(vendor.to_ascii_lowercase(), device.to_ascii_lowercase()))
        .cloned()
}

fn pci_vendor_name(id: &str) -> Option<&'static str> {
    Some(match id.split(':').next()?.to_ascii_lowercase().as_str() {
        "1002" => "AMD",
        "8086" => "Intel",
        "10de" => "NVIDIA",
        "13b5" => "ARM",
        "5143" => "Qualcomm",
        "1af4" => "virtio",
        "1414" => "Microsoft",
        "1ae0" => "Google",
        _ => return None,
    })
}

/// One `/sys/class/drm/cardN`, with what the kernel exposes about it.
#[derive(Clone, Debug, Default)]
struct Card {
    node: String,
    driver: String,
    pci_id: String,
    slot: String,
    drm_name: String,
    boot_vga: bool,
    connected: usize,
    /// The compositor has this device open (card or render node).
    used: bool,
}

/// DRM devices the given process holds open. A compositor usually opens the
/// render node, not the card node, so both map back to the same `device`.
fn drm_devices_used(pid: u32) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(format!("/proc/{pid}/fd")) else {
        return out;
    };
    for e in rd.flatten() {
        let Ok(target) = fs::read_link(e.path()) else {
            continue;
        };
        let Some(name) = target.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !(name.starts_with("card") || name.starts_with("renderD")) {
            continue;
        }
        if let Ok(dev) = fs::canonicalize(format!("/sys/class/drm/{name}/device")) {
            if !out.contains(&dev) {
                out.push(dev);
            }
        }
    }
    out
}

fn drm_cards(compositor: Option<u32>) -> Vec<Card> {
    let used = compositor.map(drm_devices_used).unwrap_or_default();
    let Ok(rd) = fs::read_dir("/sys/class/drm") else {
        return Vec::new();
    };
    let mut cards = Vec::new();
    for e in rd.flatten() {
        let node = e.file_name().to_string_lossy().to_string();
        let Some(num) = node.strip_prefix("card") else {
            continue;
        };
        if num.is_empty() || !num.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let dev = e.path().join("device");
        let uevent = read(&dev.join("uevent").to_string_lossy()).unwrap_or_default();
        let mut map = HashMap::new();
        for line in uevent.lines() {
            if let Some((k, v)) = line.split_once('=') {
                map.insert(k.to_string(), v.to_string());
            }
        }
        // The uevent is the fastest source, but platform GPUs often omit the
        // driver there: the symlink the driver core creates is authoritative.
        let driver = map
            .get("DRIVER")
            .cloned()
            .filter(|d| !d.is_empty())
            .or_else(|| {
                fs::read_link(dev.join("driver"))
                    .ok()
                    .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            })
            .unwrap_or_default();
        let pci_id = map
            .get("PCI_ID")
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty())
            .or_else(|| {
                let vendor = read(&dev.join("vendor").to_string_lossy())?;
                let device = read(&dev.join("device").to_string_lossy())?;
                Some(format!(
                    "{}:{}",
                    vendor.trim().trim_start_matches("0x").to_ascii_lowercase(),
                    device.trim().trim_start_matches("0x").to_ascii_lowercase()
                ))
            })
            .unwrap_or_default();
        let slot = map
            .get("PCI_SLOT_NAME")
            .cloned()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                fs::canonicalize(&dev)
                    .ok()
                    .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            })
            .unwrap_or_default();
        let connected = fs::read_dir("/sys/class/drm")
            .map(|rd| {
                rd.flatten()
                    .filter(|o| {
                        let name = o.file_name().to_string_lossy().to_string();
                        name.starts_with(&format!("{node}-"))
                            && read(&o.path().join("status").to_string_lossy()).as_deref()
                                == Some("connected")
                    })
                    .count()
            })
            .unwrap_or(0);
        cards.push(Card {
            node,
            driver,
            pci_id,
            slot,
            drm_name: map.get("DRM_NAME").cloned().unwrap_or_default(),
            boot_vga: proc::read_u64(&dev.join("boot_vga").to_string_lossy()) == Some(1),
            connected,
            used: fs::canonicalize(&dev)
                .map(|p| used.contains(&p))
                .unwrap_or(false),
        });
    }
    cards
}

impl Card {
    /// Which card a benchmark run is about: the one the compositor renders
    /// with, then the boot display, then whichever has a monitor attached. A
    /// virtual or unbound device only wins when it is the only one there.
    fn rank(&self) -> i32 {
        let virtual_driver = matches!(
            self.driver.as_str(),
            "" | "vgem" | "vkms" | "simpledrm" | "simpledrmfb"
        );
        let mut score = 0;
        if self.used {
            score += 100;
        }
        if !virtual_driver {
            score += 20;
        }
        if self.boot_vga {
            score += 10;
        }
        if self.connected > 0 {
            score += 5;
        }
        if !self.pci_id.is_empty() {
            score += 2;
        }
        score
    }

    fn name(&self) -> String {
        if let Some(n) = pci_name(&self.pci_id) {
            return n;
        }
        if !self.drm_name.is_empty() && self.drm_name != "unknown" {
            return self.drm_name.clone();
        }
        if self.pci_id.is_empty() {
            if !self.driver.is_empty() {
                return format!("{} GPU", self.driver);
            }
            return "unknown GPU".into();
        }
        match pci_vendor_name(&self.pci_id) {
            Some(vendor) => format!("{vendor} GPU {}", self.pci_id),
            None => format!("GPU {}", self.pci_id),
        }
    }

    fn describe(&self) -> String {
        if self.driver.is_empty() {
            self.name()
        } else {
            format!("{} · {}", self.name(), self.driver)
        }
    }
}

pub fn gpu_info(compositor: Option<u32>) -> GpuInfo {
    let mut cards = drm_cards(compositor);
    // Stable order for equal ranks: card0 first, then card1, …
    cards.sort_by(|a, b| b.rank().cmp(&a.rank()).then_with(|| a.node.cmp(&b.node)));
    let Some(primary) = cards.first() else {
        return GpuInfo {
            name: "unknown GPU".into(),
            ..Default::default()
        };
    };
    GpuInfo {
        name: primary.name(),
        driver: primary.driver.clone(),
        pci_id: primary.pci_id.clone(),
        pci_slot: primary.slot.clone(),
        others: cards[1..].iter().map(Card::describe).collect(),
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
    pub fn collect(hint: DisplayHint, compositor: Option<&proc::Compositor>) -> Self {
        let (cores, threads) = cpu_topology();
        let gpu = gpu_info(compositor.map(|c| c.pid));
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
            config_hash: compositor.and_then(|c| proc::config_hash(&c.name)),
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
    fn begin_and_end_measure_the_actual_wall_clock() {
        let a = Snapshot::begin(None);
        std::thread::sleep(std::time::Duration::from_millis(30));
        let b = Snapshot::end(None);
        let elapsed = b.at.saturating_duration_since(a.at).as_secs_f64();
        assert!(elapsed >= 0.02, "elapsed was {elapsed:.3} s");
        let (cpu, comp) = a.delta_cpu(&b, elapsed);
        assert!(cpu.is_finite() && cpu >= 0.0);
        assert_eq!(comp, 0.0, "no compositor pid was given");
        assert!(b.cpu_usec >= a.cpu_usec);
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

    #[test]
    fn the_card_the_compositor_uses_wins() {
        let fallback = Card {
            node: "card0".into(),
            driver: "simpledrm".into(),
            ..Default::default()
        };
        let amd = Card {
            node: "card1".into(),
            driver: "amdgpu".into(),
            pci_id: "1002:15d8".into(),
            used: true,
            ..Default::default()
        };
        assert!(amd.rank() > fallback.rank());
        // a real driver beats a virtual one even without an attached monitor
        let real = Card {
            node: "card2".into(),
            driver: "i915".into(),
            ..Default::default()
        };
        assert!(real.rank() > fallback.rank());
    }

    #[test]
    fn gpu_names_fall_back_to_the_vendor() {
        let amd = Card {
            pci_id: "1002:ffff".into(),
            ..Default::default()
        };
        assert_eq!(amd.name(), "AMD GPU 1002:ffff");
        let unknown = Card {
            pci_id: "ffff:ffff".into(),
            ..Default::default()
        };
        assert_eq!(unknown.name(), "GPU ffff:ffff");
        let platform = Card {
            driver: "msm".into(),
            ..Default::default()
        };
        assert_eq!(platform.name(), "msm GPU");
    }
}
