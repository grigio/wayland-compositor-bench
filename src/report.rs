//! The result schema, the median table, and `--compare`.
//!
//! One writer, one reader. Every phase is a flat object so a new column is one
//! tuple instead of a new code path.
//!
//! Two things the table refuses to hide:
//!
//! - dispersion: a cell printed without a range is a single run, and a cell
//!   whose range is wide is marked, because "median of 3" is not a confidence
//!   interval.
//! - context: runs whose display, compositor version or config differ are
//!   flagged, since no amount of statistics fixes comparing 60 Hz to 120 Hz.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cli::JsonTarget;
use crate::sysinfo::{self, Env};

pub const SCHEMA_VERSION: u32 = 3;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Contaminant {
    pub name: String,
    pub pss_mib: f64,
}

/// Share of the compositor's own CPU time taken by one thread in a phase.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ThreadShare {
    pub name: String,
    pub tid: u32,
    pub share_pct: f64,
}

/// One separate shell/panel/notification process included in the desktop
/// stack. A compositor whose shell lives inside its own process reports none.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StackProcess {
    pub pid: u32,
    pub name: String,
    pub pss_mib: f64,
    pub cpu_pct: f64,
}

/// One point of the session CPU timeline: seconds since the run started, CPU %
/// of one core. Only written with `--json`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CpuPoint {
    pub t_s: f32,
    pub pct: f32,
}

/// A phase boundary on the same clock as `CpuPoint`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CpuMark {
    pub t_s: f32,
    pub phase: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct IdlePhase {
    /// requested `--idle` seconds
    pub dur_s: f64,
    /// actual wall-clock length of the measured window; `None` in files written
    /// before it was recorded
    pub measured_s: Option<f64>,
    /// how long it took the session to go quiet before sampling started
    pub settle_s: f64,
    pub mem_mib: f64,
    /// `memory.current`: working set plus page cache
    pub total_mib: f64,
    pub peak_mib: f64,
    pub peak_method: String,
    pub anon_mib: f64,
    pub shmem_mib: f64,
    pub file_mib: f64,
    pub kernel_mib: f64,
    pub swap_mib: f64,
    pub procs: usize,
    pub cpu_pct: f64,
    pub comp_cpu_pct: f64,
    /// compositor CPU plus the separately declared shell/panel processes.
    /// `None` in older files and when no stack was declared.
    pub stack_cpu_pct: Option<f64>,
    /// session CPU not attributable to the stack or to this benchmark. This is
    /// the number `quiet cpu` checks; a shell is part of the desktop, not noise.
    pub unrelated_cpu_pct: Option<f64>,
    /// session CPU not attributable to the compositor process tree or to this
    /// benchmark. Kept with its v3 meaning (it includes a separate shell).
    pub other_cpu_pct: Option<f64>,
    pub wakeups_per_s: f64,
    pub comp_pss_mib: f64,
    /// compositor PSS plus the separately declared shell/panel processes.
    pub stack_pss_mib: Option<f64>,
    /// `integrated` when the compositor process already contains the shell
    /// (GNOME), `modular` when the stack adds separate processes.
    pub stack_style: String,
    /// `auto`, `none`, or the user's comma-separated `--stack` selector.
    pub stack_declared: String,
    pub stack_processes: Vec<StackProcess>,
    pub stack_wakeups_per_s: Option<f64>,
    /// PSS of this benchmark process, so session memory can be corrected for it.
    pub self_pss_mib: Option<f64>,
    /// session working set without this benchmark client.
    pub mem_excl_self_mib: Option<f64>,
    /// null when RAPL is unreadable or the battery is not discharging
    pub psys_w: Option<f64>,
    pub pkg_w: Option<f64>,
    pub batt_w: Option<f64>,
    pub batt_status: String,
    pub threads: Vec<ThreadShare>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowsPhase {
    pub extra: u32,
    pub settle_s: f64,
    /// "same-process": the extra toplevels belong to this benchmark, so GTK's
    /// buffer pool is shared and the client share is not a separate client's
    /// footprint.
    pub method: String,
    /// second of two pre-open baselines
    pub baseline_mib: f64,
    /// second baseline minus the first: how much the idle session moved while
    /// the baseline was taken. `None` in files written before it was recorded.
    pub baseline_drift_mib: Option<f64>,
    pub with_windows_mib: f64,
    /// raw signed delta; negative means the session shrank, not that it grew
    /// by zero
    pub delta_mib: f64,
    /// false when the delta is negative or the baseline drifted too much
    pub delta_valid: bool,
    /// GPU-accessible memory the benchmark's clients hold with the toplevels
    /// open, from DRM fdinfo. Buffers are pooled, so there is no useful delta.
    pub client_gpu_mib: f64,
    /// GPU-accessible memory the compositor holds at the same moment
    pub comp_gpu_mib: f64,
    /// working set delta per extra toplevel; `None` when `delta_valid` is false
    pub per_window_mib: Option<f64>,
    /// signed share of the working set delta charged to the compositor's tree
    pub comp_delta_mib: f64,
    /// the signed rest: the same-process clients that were opened
    pub client_delta_mib: f64,
    /// memory still held after close, against the pre-open baseline (signed).
    /// This keeps the v3 meaning of the field.
    pub residual_mib: f64,
    /// drift between two post-close snapshots: the leak indicator, not the
    /// buffer pool that the frame test grew. `None` in files written before it
    /// was recorded.
    pub post_close_drift_mib: Option<f64>,
}

/// One frame-timing scenario: a single animated surface, or that surface plus N
/// extra toplevels drawing every frame.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RenderPhase {
    pub scenario: String,
    /// requested `--frames` seconds
    pub dur_s: f64,
    /// actual wall-clock length of the measured window; `None` in files written
    /// before it was recorded
    pub measured_s: Option<f64>,
    /// frames the compositor actually presented
    pub frames: i64,
    pub dropped: i64,
    pub fps: f64,
    pub target_hz: f64,
    /// the cadence the client was asked to target; `None` means native refresh
    pub workload_hz: Option<f64>,
    /// the rate the compositor actually settled into
    pub cadence_hz: f64,
    /// delivered frames as a share of the panel refresh: 60 fps on a 120 Hz
    /// panel is 50 %, not 100 %. This is the cross-compositor pacing number.
    pub pacing_vs_panel: Option<f64>,
    /// frames missed against the panel period, not the observed cadence
    pub dropped_vs_panel: Option<i64>,
    /// p95 frame interval as a multiple of the panel period
    pub p95_over_panel: Option<f64>,
    /// compositor CPU time over the measured wall clock, % of one core
    pub comp_cpu_pct_load: Option<f64>,
    /// compositor CPU over the test window, scaled from the panel refresh to a
    /// common 60 Hz panel. `None` in files written before it was recorded.
    pub comp_cpu_pct_at_60hz: Option<f64>,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
    /// "presentation" when wp_presentation feedback arrived, else "frame_clock"
    pub pacing_source: String,
    pub comp_ms_per_frame: f64,
    pub comp_ns_per_frame: f64,
    /// compositor GPU time per frame from DRM client counters. This keeps the
    /// v3 meaning of the field; the client component is stored separately.
    pub gpu_ms_per_frame: Option<f64>,
    /// compositor GPU time per frame, under an explicit name
    pub comp_gpu_ms_per_frame: Option<f64>,
    /// this benchmark's GPU time per frame from its own DRM client counters
    pub app_gpu_ms_per_frame: Option<f64>,
    pub app_ms_per_frame: f64,
    pub session_cpu_s: f64,
    /// this benchmark's own CPU time during the same window. `None` in files
    /// written before the field was optional: a missing measurement is not a
    /// free run.
    pub self_cpu_s: Option<f64>,
    /// `session_cpu_s - self_cpu_s`, the honest session figure. `None` when
    /// `self_cpu_s` is missing.
    pub session_cpu_excl_self_s: Option<f64>,
    /// RAPL/battery around the frame test; null when unreadable
    pub psys_w: Option<f64>,
    pub pkg_w: Option<f64>,
    pub batt_w: Option<f64>,
    pub threads: Vec<ThreadShare>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PeakPhase {
    pub dur_s: f64,
    pub ram_before_mib: f64,
    pub ram_peak_mib: f64,
    pub ram_peak_method: String,
    pub cpu_idle_pct: f64,
    /// Mean load across the run, the number the radar plots. `None` in files
    /// written before the radar showed an average: those runs only have the
    /// peak, and a missing average is not a zero.
    pub cpu_avg_pct: Option<f64>,
    pub cpu_peak_pct: f64,
    /// 100 ms session CPU timeline, written only with `--json`
    pub cpu_series: Option<Vec<CpuPoint>>,
    /// phase boundaries for the timeline
    pub cpu_marks: Option<Vec<CpuMark>>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

/// One protocol in the run's coverage snapshot. `version` is what the
/// compositor advertised, `since` is the lowest version that is actually
/// useful, and `stale` means it advertised an older one.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FeatureItem {
    pub name: String,
    pub label: String,
    pub tier: String,
    pub version: Option<u32>,
    pub since: u32,
    pub stale: bool,
}

/// Protocol coverage at run time. `None` on the run means the scan failed;
/// an empty block is not the same as "advertised nothing".
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Features {
    /// advertised, including stale versions
    pub supported: usize,
    pub total: usize,
    pub supported_all: usize,
    pub total_all: usize,
    /// globals the reference list does not know about
    pub extra: usize,
    /// advertised but below `since`
    pub stale: usize,
    pub percent: f64,
    pub items: Vec<FeatureItem>,
}

impl Features {
    pub fn from_scan(scan: &crate::wlscan::Features) -> Self {
        let items: Vec<FeatureItem> = scan
            .items
            .iter()
            .map(|item| FeatureItem {
                name: item.feature.name.to_string(),
                label: item.feature.label.to_string(),
                tier: item.feature.tier.key().to_string(),
                version: item.version,
                since: item.feature.since,
                stale: item.stale(),
            })
            .collect();
        Self {
            supported: scan.supported,
            total: scan.total,
            supported_all: scan.supported_all,
            total_all: scan.total_all,
            extra: scan.extra,
            stale: scan.stale,
            percent: scan.percent(),
            items,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Run {
    pub schema_version: u32,
    pub label: String,
    pub compositor: String,
    pub compositor_version: Option<String>,
    pub compositor_pid: u32,
    /// "socket" when resolved through WAYLAND_DISPLAY, "comm" for the fallback
    pub compositor_source: String,
    pub when: String,
    /// the desktop this run belongs to, resolved from the process that owns
    /// the Wayland socket — not from the environment, which can be stale
    pub desktop: String,
    /// the raw `XDG_CURRENT_DESKTOP`; differs from `desktop` when the session
    /// manager handed the compositor another desktop's environment
    pub desktop_env: String,
    pub cgroup: String,
    pub cgroup_exact: bool,
    /// `login-session`, `user-manager`, `login-session+user-manager` or
    /// `user-slice`; empty in older files
    pub cgroup_kind: String,
    /// `integrated` or `modular`; empty in older files
    pub stack_style: String,
    /// `native` or `reference`; empty in older files
    pub profile: String,
    /// "on" or "off": whether the window used the compositor's blur
    pub blur: String,
    /// high / medium / low, derived from `checks`
    pub confidence: String,
    pub checks: Vec<Check>,
    pub clean: bool,
    pub contaminants: Vec<Contaminant>,
    pub env: Env,
    /// protocol coverage at run time; `None` when the scan failed
    pub features: Option<Features>,
    pub idle: Option<IdlePhase>,
    pub windows: Option<WindowsPhase>,
    pub render: Option<RenderPhase>,
    pub multi: Option<RenderPhase>,
    pub peaks: Option<PeakPhase>,
}

/// Where automatic runs land: `$XDG_STATE_HOME/wayland-compositor-bench/results`
/// (usually `~/.local/state/...`), not the working directory. A menu launch has
/// no useful CWD; `--report` searches `./results` as well, so runs made from the
/// repository keep working.
pub fn results_dir() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state")
        });
    base.join("wayland-compositor-bench").join("results")
}

impl Run {
    /// Writes the run and returns the path it landed on.
    pub fn write(&self, target: &JsonTarget) -> std::io::Result<PathBuf> {
        let path = match target {
            JsonTarget::Path(p) => p.clone(),
            JsonTarget::Auto => results_dir().join(format!(
                "{}-{}.json",
                crate::cli::slug(&self.label),
                stamp()
            )),
        };
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        // A serialization failure must not leave an empty file that looks like
        // a successful run: propagate it so `--run` can exit non-zero.
        let mut text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        text.push('\n');
        fs::write(&path, text)?;
        Ok(path)
    }

    /// The one line worth pasting into a terminal.
    pub fn summary_line(&self) -> String {
        let i = self.idle.clone().unwrap_or_default();
        let watts = match (i.psys_w, i.batt_w) {
            (Some(w), _) => format!("{w:.1} W"),
            (None, Some(w)) => format!("{w:.1} W bat"),
            _ => "- W".to_string(),
        };
        let frame = self
            .render
            .as_ref()
            .map(|r| format!("{:.3} ms/frame", r.comp_ms_per_frame))
            .unwrap_or_else(|| "- ms/frame".into());
        format!(
            "{}\t{:.0} MiB\t{:.0} MiB comp\t{:.2} %cpu\t{}\t{}",
            self.label, i.mem_mib, i.comp_pss_mib, i.cpu_pct, watts, frame
        )
    }
}

pub fn now_iso() -> String {
    glib::DateTime::now_local()
        .and_then(|d| d.format("%Y-%m-%dT%H:%M:%S%:z"))
        .map(|s| s.to_string())
        .unwrap_or_else(|_| String::from("unknown"))
}

pub fn stamp() -> String {
    glib::DateTime::now_local()
        .and_then(|d| d.format("%Y%m%d-%H%M%S"))
        .map(|s| s.to_string())
        .unwrap_or_else(|_| String::from("0"))
}

// ------------------------------------------------------------------ stats ---

#[derive(Clone, Copy, Debug)]
pub struct Stats {
    pub n: usize,
    pub median: f64,
    pub min: f64,
    pub max: f64,
    /// median absolute deviation: a spread estimate that one outlier cannot
    /// inflate the way the range can
    pub mad: f64,
}

impl Stats {
    /// Half the observed range, floored at the median absolute deviation and
    /// at 2 % of the median. Used as the noise floor when deciding whether two
    /// labels actually differ.
    pub fn noise(&self) -> f64 {
        let spread = if self.n > 1 {
            (self.max - self.min) / 2.0
        } else {
            0.0
        };
        spread
            .max(1.57 * self.mad / (self.n as f64).sqrt())
            .max(self.median.abs() * 0.02)
    }

    /// True when there are enough runs for a median and a spread to mean
    /// anything. One run is an anecdote, not a comparison.
    pub fn enough_runs(&self) -> bool {
        self.n >= 3
    }
}

fn median_of_sorted(v: &[f64]) -> f64 {
    let n = v.len();
    if n.is_multiple_of(2) {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    } else {
        v[n / 2]
    }
}

pub fn stats(mut v: Vec<f64>) -> Option<Stats> {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    let median = median_of_sorted(&v);
    let mut deviations: Vec<f64> = v.iter().map(|x| (x - median).abs()).collect();
    deviations.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(Stats {
        n,
        median,
        min: v[0],
        max: v[n - 1],
        mad: median_of_sorted(&deviations),
    })
}

fn fmt_value(s: &Stats, nd: usize) -> String {
    if s.n == 1 || (s.max - s.min) <= s.median.abs() * 0.01 {
        format!("{:.nd$}", s.median)
    } else {
        format!("{:.nd$} [{:.nd$}-{:.nd$}]", s.median, s.min, s.max)
    }
}

// ------------------------------------------------------------------ table ---

struct Phase {
    key: &'static str,
    title: &'static str,
    columns: &'static [(&'static str, &'static str, usize)],
    sort_by: &'static str,
}

struct TableRow {
    label: String,
    cells: Vec<String>,
    runs: usize,
    sort_key: f64,
}

const IDLE: Phase = Phase {
    key: "idle",
    title: "idle session",
    columns: &[
        ("mem_mib", "RAM MiB", 0),
        ("peak_mib", "peak MiB", 0),
        ("anon_mib", "anon MiB", 0),
        ("comp_pss_mib", "comp MiB", 0),
        ("stack_pss_mib", "stack MiB", 0),
        ("procs", "procs", 0),
        ("cpu_pct", "CPU %", 2),
        ("comp_cpu_pct", "comp CPU %", 2),
        ("stack_cpu_pct", "stack CPU %", 2),
        ("unrelated_cpu_pct", "unrel CPU %", 2),
        ("wakeups_per_s", "wakeups/s", 0),
        ("psys_w", "psys W", 2),
        ("batt_w", "bat W", 2),
    ],
    sort_by: "mem_mib",
};

const WINDOWS: Phase = Phase {
    key: "windows",
    title: "extra toplevels",
    columns: &[
        ("per_window_mib", "ws/window", 1),
        ("delta_mib", "ws delta", 0),
        ("client_gpu_mib", "client GPU", 0),
        ("comp_delta_mib", "comp delta", 1),
        ("comp_gpu_mib", "comp GPU", 0),
        ("residual_mib", "residual", 1),
        ("post_close_drift_mib", "post-close", 1),
        ("extra", "windows", 0),
    ],
    sort_by: "per_window_mib",
};

const RENDER: Phase = Phase {
    key: "render",
    title: "frame timing — one animated surface",
    columns: &[
        ("fps", "fps", 1),
        ("cadence_hz", "cadence Hz", 1),
        ("pacing_vs_panel", "panel %", 1),
        ("comp_ms_per_frame", "comp ms/frame", 3),
        ("comp_cpu_pct_at_60hz", "CPU@60Hz %", 2),
        ("gpu_ms_per_frame", "comp gpu", 3),
        ("app_gpu_ms_per_frame", "client gpu", 3),
        ("psys_w", "psys W", 2),
        ("batt_w", "bat W", 2),
        ("p95_ms", "p95 ms", 2),
        ("dropped", "dropped", 0),
        ("dropped_vs_panel", "drop/panel", 0),
        ("frames", "frames", 0),
    ],
    sort_by: "comp_ms_per_frame",
};

const MULTI: Phase = Phase {
    key: "multi",
    title: "frame timing — animated surface + extra windows",
    columns: &[
        ("fps", "fps", 1),
        ("cadence_hz", "cadence Hz", 1),
        ("pacing_vs_panel", "panel %", 1),
        ("comp_ms_per_frame", "comp ms/frame", 3),
        ("comp_cpu_pct_at_60hz", "CPU@60Hz %", 2),
        ("gpu_ms_per_frame", "comp gpu", 3),
        ("app_gpu_ms_per_frame", "client gpu", 3),
        ("psys_w", "psys W", 2),
        ("batt_w", "bat W", 2),
        ("p95_ms", "p95 ms", 2),
        ("dropped", "dropped", 0),
        ("dropped_vs_panel", "drop/panel", 0),
        ("frames", "frames", 0),
    ],
    sort_by: "comp_ms_per_frame",
};

const PEAKS: Phase = Phase {
    key: "peaks",
    title: "during the run",
    columns: &[
        ("ram_peak_mib", "RAM peak", 0),
        ("cpu_avg_pct", "CPU avg %", 1),
        ("cpu_peak_pct", "CPU peak %", 1),
    ],
    sort_by: "ram_peak_mib",
};

fn has_phase(run: &Run, key: &str) -> bool {
    match key {
        "idle" => run.idle.is_some(),
        "windows" => run.windows.is_some(),
        "render" => run.render.is_some(),
        "multi" => run.multi.is_some(),
        "peaks" => run.peaks.is_some(),
        _ => false,
    }
}

/// Typed field access. The old version serialized the whole phase to a
/// `serde_json::Value` for every column of every run, then parsed it back.
fn cell(run: &Run, phase: &Phase, field: &str) -> Option<f64> {
    match phase.key {
        "idle" => {
            let p = run.idle.as_ref()?;
            Some(match field {
                "mem_mib" => p.mem_mib,
                "peak_mib" => p.peak_mib,
                "anon_mib" => p.anon_mib,
                "comp_pss_mib" => p.comp_pss_mib,
                "stack_pss_mib" => p.stack_pss_mib?,
                "procs" => p.procs as f64,
                "cpu_pct" => p.cpu_pct,
                "comp_cpu_pct" => p.comp_cpu_pct,
                "stack_cpu_pct" => p.stack_cpu_pct?,
                // Old files have no stack split: their `other_cpu_pct` is the
                // closest available "outside the compositor and this tool".
                "unrelated_cpu_pct" => p.unrelated_cpu_pct.or(p.other_cpu_pct)?,
                "other_cpu_pct" => p.other_cpu_pct?,
                "wakeups_per_s" => p.wakeups_per_s,
                "psys_w" => p.psys_w?,
                "batt_w" => p.batt_w?,
                _ => return None,
            })
        }
        "windows" => {
            let p = run.windows.as_ref()?;
            Some(match field {
                // A per-window number is only meaningful when the writer said
                // the delta was valid. Old v3 files have no `delta_valid`, so
                // their clamped 0.0 reads as not measured.
                "per_window_mib" => {
                    if !p.delta_valid {
                        return None;
                    }
                    p.per_window_mib?
                }
                "delta_mib" => p.delta_mib,
                "client_gpu_mib" => p.client_gpu_mib,
                "comp_delta_mib" => p.comp_delta_mib,
                "comp_gpu_mib" => p.comp_gpu_mib,
                "residual_mib" => p.residual_mib,
                "post_close_drift_mib" => p.post_close_drift_mib?,
                "extra" => f64::from(p.extra),
                _ => return None,
            })
        }
        "render" | "multi" => {
            let p = if phase.key == "render" {
                run.render.as_ref()?
            } else {
                run.multi.as_ref()?
            };
            Some(match field {
                "fps" => p.fps,
                "cadence_hz" => p.cadence_hz,
                "pacing_vs_panel" => p.pacing_vs_panel? * 100.0,
                "pacing_vs_panel_share" => p.pacing_vs_panel?,
                "comp_ms_per_frame" => p.comp_ms_per_frame,
                "comp_cpu_pct_load" => p.comp_cpu_pct_load?,
                "comp_cpu_pct_at_60hz" => p.comp_cpu_pct_at_60hz?,
                "gpu_ms_per_frame" => p.gpu_ms_per_frame?,
                "comp_gpu_ms_per_frame" => p.comp_gpu_ms_per_frame?,
                "app_gpu_ms_per_frame" => p.app_gpu_ms_per_frame?,
                "p95_ms" => p.p95_ms,
                "p95_over_panel" => p.p95_over_panel?,
                "dropped" => p.dropped as f64,
                "dropped_vs_panel" => p.dropped_vs_panel? as f64,
                "frames" => p.frames as f64,
                "psys_w" => p.psys_w?,
                "batt_w" => p.batt_w?,
                _ => return None,
            })
        }
        "peaks" => {
            let p = run.peaks.as_ref()?;
            Some(match field {
                "ram_peak_mib" => p.ram_peak_mib,
                "cpu_avg_pct" => p.cpu_avg_pct?,
                "cpu_peak_pct" => p.cpu_peak_pct,
                _ => return None,
            })
        }
        _ => None,
    }
}

fn field_stats(runs: &[&Run], phase: &Phase, field: &str) -> Option<Stats> {
    stats(runs.iter().filter_map(|r| cell(r, phase, field)).collect())
}

/// Collects result files from directories and explicit files, plus the label
/// filters that need a parsed run to match. With no targets, both the working
/// directory's `results/` and the XDG state directory are searched.
fn collect(targets: &[PathBuf]) -> (Vec<PathBuf>, Vec<String>) {
    let mut files = Vec::new();
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut filters: Vec<String> = Vec::new();

    if targets.is_empty() {
        dirs.push(PathBuf::from("results"));
        dirs.push(results_dir());
    }
    for t in targets {
        if t.is_dir() {
            dirs.push(t.clone());
        } else if t.is_file() {
            files.push(t.clone());
        } else {
            filters.push(t.to_string_lossy().into_owned());
        }
    }
    if !filters.is_empty() && dirs.is_empty() {
        dirs.push(PathBuf::from("results"));
        dirs.push(results_dir());
    }

    for dir in &dirs {
        let Ok(rd) = fs::read_dir(dir) else { continue };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                files.push(path);
            }
        }
    }

    files.sort();
    files.dedup();
    (files, filters)
}

type Labeled = BTreeMap<String, Vec<Run>>;

fn label_of(run: &Run, file: &Path) -> String {
    if run.label.is_empty() {
        file.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        run.label.clone()
    }
}

fn load(targets: &[PathBuf]) -> Result<(Labeled, usize), String> {
    let (files, filters) = collect(targets);
    if files.is_empty() {
        return Err(
            "no saved runs yet — run `wayland-compositor-bench --run --json` inside each session first"
                .into(),
        );
    }
    // `--report`/`--compare` promise label substrings. A path filter still
    // works as a fallback, which is what the old implementation did for both.
    let selected = |label: &str, file: &Path| -> bool {
        filters.is_empty()
            || filters.iter().any(|f| {
                let f = f.to_lowercase();
                label.to_lowercase().contains(&f)
                    || file.to_string_lossy().to_lowercase().contains(&f)
            })
    };
    let path_selected = |file: &Path| -> bool {
        filters.is_empty()
            || filters.iter().any(|f| {
                file.to_string_lossy()
                    .to_lowercase()
                    .contains(&f.to_lowercase())
            })
    };

    let mut runs: Labeled = BTreeMap::new();
    let mut skipped = 0;
    for file in &files {
        let text = fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
        match serde_json::from_str::<Run>(&text) {
            // Older schemas measured different things under the same names
            // (RAM used to include page cache), so they are not comparable.
            Ok(run) if run.schema_version != SCHEMA_VERSION => {
                if selected(&label_of(&run, file), file) {
                    eprintln!(
                        "ignoring {}: schema v{} (this build reads v{SCHEMA_VERSION})",
                        file.display(),
                        run.schema_version
                    );
                    skipped += 1;
                }
            }
            Ok(run) => {
                let label = label_of(&run, file);
                if !selected(&label, file) {
                    continue;
                }
                runs.entry(label).or_default().push(run);
            }
            Err(e) => {
                if path_selected(file) {
                    eprintln!("skipping {}: {e}", file.display());
                    skipped += 1;
                }
            }
        }
    }
    Ok((runs, skipped))
}

/// Scope kind for old files too. A v3 file written before the field existed
/// still carries the cgroup path, which is enough to tell a login scope from
/// the user manager.
fn cgroup_kind_of(run: &Run) -> &str {
    if !run.cgroup_kind.is_empty() {
        return &run.cgroup_kind;
    }
    if run.cgroup.contains("user@") {
        "user-manager"
    } else if run.cgroup.contains("session-") {
        "login-session"
    } else if run.cgroup.contains("user.slice") {
        "user-slice"
    } else {
        ""
    }
}

/// `--report`: medians per label, one table per phase.
pub fn print(targets: &[PathBuf]) -> Result<(), String> {
    let (runs, skipped) = load(targets)?;

    let has = |p: &Phase, set: &Labeled| set.values().flatten().any(|r| has_phase(r, p.key));
    let mut phases = vec![IDLE, WINDOWS];
    if has(&RENDER, &runs) {
        phases.push(RENDER);
    }
    if has(&MULTI, &runs) {
        phases.push(MULTI);
    }
    if has(&PEAKS, &runs) {
        phases.push(PEAKS);
    }

    for phase in &phases {
        let mut rows: Vec<TableRow> = Vec::new();
        for (label, list) in &runs {
            let list: Vec<&Run> = list.iter().filter(|r| has_phase(r, phase.key)).collect();
            if list.is_empty() {
                continue;
            }
            let mut sort_key = f64::MAX;
            let cells = phase
                .columns
                .iter()
                .map(|(field, _, nd)| match field_stats(&list, phase, field) {
                    Some(s) => {
                        if *field == phase.sort_by {
                            sort_key = s.median;
                        }
                        fmt_value(&s, *nd)
                    }
                    None => "-".into(),
                })
                .collect();
            rows.push(TableRow {
                label: label.clone(),
                cells,
                runs: list.len(),
                sort_key,
            });
        }
        if rows.is_empty() {
            continue;
        }
        // Numeric key kept while the row is built: parsing the formatted cell
        // back into a float breaks the moment a format gains a suffix.
        rows.sort_by(|a, b| {
            a.sort_key
                .partial_cmp(&b.sort_key)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut header: Vec<String> = vec!["compositor".into()];
        header.extend(phase.columns.iter().map(|(_, t, _)| t.to_string()));
        header.push("runs".into());

        println!("\n### {}\n", phase.title);
        print_table(&header, &rows);
    }

    // --------------------------------------------------------- context -----
    println!("\nsessions:");
    for (label, list) in &runs {
        let r = &list[0];
        let version = r
            .compositor_version
            .clone()
            .unwrap_or_else(|| r.compositor.clone());
        let blur = match r.blur.as_str() {
            "on" => " · blur on",
            "off" => " · blur off",
            _ => "",
        };
        println!(
            "  {label:<16} {version} · {} · {} run(s) · confidence {}{blur}",
            if r.desktop.is_empty() {
                "?"
            } else {
                &r.desktop
            },
            list.len(),
            if r.confidence.is_empty() {
                "unknown"
            } else {
                &r.confidence
            }
        );
        // The README promises `--report` prints what downgraded a run. The
        // confidence string alone does not say which check failed.
        let mut failures: Vec<String> = Vec::new();
        for run in list {
            for c in run.checks.iter().filter(|c| !c.ok) {
                let text = if c.detail.is_empty() {
                    c.name.clone()
                } else {
                    format!("{}: {}", c.name, c.detail)
                };
                if !failures.contains(&text) {
                    failures.push(text);
                }
            }
        }
        if !failures.is_empty() {
            println!("    checks: {}", failures.join("; "));
        }
    }

    let mut warnings = Vec::new();
    for (label, list) in &runs {
        // Same label, different display or config: the median mixes them.
        let first = &list[0].env;
        for r in list.iter().skip(1) {
            if let Some(d) = first.display_delta(&r.env) {
                warnings.push(format!("{label}: runs differ ({d})"));
                break;
            }
            if r.env.config_hash != first.config_hash && first.config_hash.is_some() {
                warnings.push(format!("{label}: config changed between runs"));
                break;
            }
        }
        // Only known values can disagree; an old file that predates the field
        // is not a third kind, it is an unknown.
        let kinds: Vec<&str> = list
            .iter()
            .map(cgroup_kind_of)
            .filter(|k| !k.is_empty())
            .collect();
        if kinds.len() > 1 && kinds.iter().any(|k| *k != kinds[0]) {
            warnings.push(format!(
                "{label}: runs used different cgroup kinds ({}), so session totals are not the \
                 same scope",
                kinds.join(", ")
            ));
        }
        let styles: Vec<&str> = list
            .iter()
            .map(|r| r.stack_style.as_str())
            .filter(|s| !s.is_empty())
            .collect();
        if styles.len() > 1 && styles.iter().any(|s| *s != styles[0]) {
            warnings.push(format!(
                "{label}: runs mixed stack styles ({}); core and stack numbers are not \
                 interchangeable",
                styles.join(", ")
            ));
        }
        let profiles: Vec<&str> = list
            .iter()
            .map(|r| r.profile.as_str())
            .filter(|p| !p.is_empty())
            .collect();
        if profiles.len() > 1 && profiles.iter().any(|p| *p != profiles[0]) {
            warnings.push(format!(
                "{label}: runs used different workload profiles ({}), so render numbers are not \
                 comparable",
                profiles.join(", ")
            ));
        }
        let workload = |r: &Run| {
            r.multi
                .as_ref()
                .or(r.render.as_ref())
                .and_then(|p| p.workload_hz)
        };
        let mut workloads: Vec<String> = list
            .iter()
            .map(|r| {
                workload(r)
                    .map(|h| format!("{h:.0} Hz"))
                    .unwrap_or_else(|| "native".into())
            })
            .collect();
        workloads.sort();
        workloads.dedup();
        if workloads.len() > 1 {
            warnings.push(format!(
                "{label}: runs targeted different render workloads ({}), so CPU@60Hz mixes \
                 different amounts of work",
                workloads.join(", ")
            ));
        }
        let selectors: Vec<&str> = list
            .iter()
            .filter_map(|r| r.idle.as_ref())
            .map(|p| p.stack_declared.as_str())
            .filter(|s| !s.is_empty())
            .collect();
        if selectors.len() > 1 && selectors.iter().any(|s| *s != selectors[0]) {
            warnings.push(format!(
                "{label}: runs used different stack selectors ({}), so the idle scopes are \
                 not the same",
                selectors.join(", ")
            ));
        }
        let blur = |r: &Run| {
            if r.blur.is_empty() {
                "unrecorded".to_string()
            } else {
                r.blur.clone()
            }
        };
        let first_blur = blur(&list[0]);
        if let Some(other) = list.iter().skip(1).find(|r| blur(r) != first_blur) {
            warnings.push(format!(
                "{label}: blur was {first_blur} in one run and {} in another — the chip state \
                 changes what the compositor is asked to do",
                blur(other)
            ));
        }
        if list.iter().any(|r| !r.cgroup_exact && !r.cgroup.is_empty()) {
            warnings.push(format!(
                "{label}: measured a user slice, not a session scope (other sessions included)"
            ));
        }
        if list.iter().any(|r| !r.clean) {
            warnings.push(format!(
                "{label}: a run had >{} MiB of unrelated processes",
                sysinfo::CONTAMINANT_MIB as u32
            ));
        }
        if list
            .iter()
            .any(|r| r.compositor_source == "comm" || r.compositor_pid == 0)
        {
            warnings.push(format!(
                "{label}: compositor process was not resolved via its socket"
            ));
        }
        if let Some(r) = list
            .iter()
            .find(|r| !r.desktop_env.is_empty() && r.desktop_env != r.desktop)
        {
            warnings.push(format!(
                "{label}: XDG_CURRENT_DESKTOP said {} but the socket belongs to {} — the run is \
                 named after the compositor",
                r.desktop_env, r.desktop
            ));
        }
    }
    warnings.dedup();
    if !warnings.is_empty() {
        println!("\n! comparability:");
        for w in warnings {
            println!("  - {w}");
        }
    }

    if skipped > 0 {
        println!(
            "\n! {skipped} file(s) were ignored: unreadable, or written by an older schema. \
             Re-run them with this build."
        );
    }

    println!(
        "\ncomp ms/frame = compositor CPU time per presented frame (lower = leaner).\n\
         stack MiB/CPU = compositor plus the declared shell/panel processes; unrel CPU = the\n\
         rest of the session, excluding the stack and this benchmark.\n\
         comp gpu = compositor GPU time per frame; client gpu = this benchmark's, both from DRM fdinfo (estimate, null without counters).\n\
         RAM = anon+shmem working set: page cache follows what the session read, not what it costs.\n\
         panel % = delivered frames as a share of the panel refresh; drop/panel = frames missed\n\
         against that panel period. CPU@60Hz % = compositor CPU over the test, scaled from the\n\
         panel refresh to 60 Hz.\n\
         dropped = missed frames against the observed cadence, which is useful for native-rate\n\
         runs but flatters a deliberately throttled one.\n\
         extra toplevels: ws/window is the signed working-set delta per window, omitted when the\n\
         delta is negative or the baseline drifted; residual is what was not returned to the\n\
         pre-open baseline; post-close is the drift between two post-close samples; client\n\
         buffers are dmabufs, pooled by the client, so GPU memory is a total.\n\
         Medians with [min-max] when runs disagree by more than 1 %."
    );
    Ok(())
}

/// `--compare A B`: medians side by side, with a noise floor so a difference
/// smaller than the run-to-run spread is not reported as a win. With `strict`,
/// an incomparable pair is an error instead of a warning.
pub fn compare(a: &str, b: &str, targets: &[PathBuf], strict: bool) -> Result<(), String> {
    let (runs, _) = load(targets)?;
    let grab = |needle: &str| -> Vec<Run> {
        let mut out = Vec::new();
        for (label, list) in &runs {
            if label.to_lowercase().contains(&needle.to_lowercase()) {
                out.extend(list.iter().cloned());
            }
        }
        out
    };
    let (ra, rb) = (grab(a), grab(b));
    if ra.is_empty() || rb.is_empty() {
        return Err(format!(
            "need saved runs for both labels (found {} for '{a}', {} for '{b}')",
            ra.len(),
            rb.len()
        ));
    }

    let ctx = |name: &str, list: &[Run]| {
        let r = &list[0];
        let hz = r
            .env
            .display
            .refresh_hz
            .or(r.env.display.compositor_hz)
            .unwrap_or(0.0);
        println!(
            "  {name}: {} · {}x{} @ {hz:.0} Hz · scale {:.2} · {} run(s)",
            r.compositor_version
                .clone()
                .unwrap_or_else(|| r.compositor.clone()),
            r.env.display.logical_w,
            r.env.display.logical_h,
            r.env.display.scale,
            list.len()
        );
    };
    println!("\ncomparing '{a}' vs '{b}'\n");
    ctx(a, &ra);
    ctx(b, &rb);
    let display_differ = ra[0].env.display_delta(&rb[0].env);
    if let Some(d) = &display_differ {
        println!("\n! displays differ ({d}) — RAM and power are not comparable");
    }

    let mut warnings = Vec::new();
    let ka = cgroup_kind_of(&ra[0]);
    let kb = cgroup_kind_of(&rb[0]);
    if !ka.is_empty() && !kb.is_empty() && ka != kb {
        warnings.push(format!(
            "cgroup kinds differ ({ka} vs {kb}): the two runs do not cover the same session \
             processes"
        ));
    }
    let (sa, sb) = (ra[0].stack_style.as_str(), rb[0].stack_style.as_str());
    if !sa.is_empty() && !sb.is_empty() && sa != sb {
        warnings.push(format!(
            "stack styles differ ({sa} vs {sb}): compare core with core or stack with stack"
        ));
    }
    // The style alone is not the scope: `--stack none` and `--stack auto` are
    // both "modular" on labwc but count different processes.
    let selector = |r: &Run| {
        r.idle
            .as_ref()
            .map(|p| p.stack_declared.clone())
            .unwrap_or_default()
    };
    let (xa, xb) = (selector(&ra[0]), selector(&rb[0]));
    if !xa.is_empty() && !xb.is_empty() && xa != xb {
        warnings.push(format!(
            "stack selectors differ ({xa} vs {xb}): the two runs count different shell/panel \
             processes"
        ));
    }
    let (pa, pb) = (ra[0].profile.as_str(), rb[0].profile.as_str());
    if !pa.is_empty() && !pb.is_empty() && pa != pb {
        warnings.push(format!(
            "workload profiles differ ({pa} vs {pb}): use the same --profile on both sides"
        ));
    }
    if ra[0].blur != rb[0].blur {
        warnings.push(format!(
            "blur differs ({} vs {}): the compositor is doing different work",
            ra[0].blur, rb[0].blur
        ));
    }
    // `--target-hz` can differ while the profile string stays `native`, and a
    // damage-driven compositor uses less CPU when the client asks for fewer
    // frames. CPU@60Hz would then measure two workloads, not two compositors.
    let workload = |r: &Run| {
        r.multi
            .as_ref()
            .or(r.render.as_ref())
            .and_then(|p| p.workload_hz)
    };
    match (workload(&ra[0]), workload(&rb[0])) {
        (Some(a), Some(b)) if (a - b).abs() >= 0.5 => warnings.push(format!(
            "render workloads differ ({a:.0} vs {b:.0} Hz target): CPU@60Hz measures different \
             amounts of work"
        )),
        (Some(a), None) | (None, Some(a)) => warnings.push(format!(
            "render workloads differ (native refresh vs {a:.0} Hz target): CPU@60Hz measures \
             different amounts of work"
        )),
        _ => {}
    }
    let power = |r: &Run| {
        (
            r.env.profile.clone(),
            r.env.governor.clone(),
            r.env.epp.clone(),
        )
    };
    if power(&ra[0]) != power(&rb[0]) {
        warnings.push(format!(
            "power settings differ ({:?}/{:?}/{:?} vs {:?}/{:?}/{:?})",
            power(&ra[0]).0,
            power(&ra[0]).1,
            power(&ra[0]).2,
            power(&rb[0]).0,
            power(&rb[0]).1,
            power(&rb[0]).2
        ));
    }
    let pacing_source = |r: &Run| {
        r.render
            .as_ref()
            .map(|p| p.pacing_source.clone())
            .unwrap_or_default()
    };
    let (psa, psb) = (pacing_source(&ra[0]), pacing_source(&rb[0]));
    if !psa.is_empty() && !psb.is_empty() && psa != psb {
        warnings.push(format!(
            "pacing source differs ({psa} vs {psb}): one side lacks presentation feedback"
        ));
    }
    if !warnings.is_empty() {
        println!("\n! comparability:");
        for w in &warnings {
            println!("  - {w}");
        }
    }

    let mut phases = vec![IDLE, WINDOWS];
    let has = |p: &Phase, list: &[Run]| list.iter().any(|r| has_phase(r, p.key));
    if has(&RENDER, &ra) || has(&RENDER, &rb) {
        phases.push(RENDER);
    }
    if has(&MULTI, &ra) || has(&MULTI, &rb) {
        phases.push(MULTI);
    }
    if has(&PEAKS, &ra) || has(&PEAKS, &rb) {
        phases.push(PEAKS);
    }

    let mut anecdotal = false;
    for phase in &phases {
        let (la, lb): (Vec<&Run>, Vec<&Run>) = (
            ra.iter().filter(|r| has_phase(r, phase.key)).collect(),
            rb.iter().filter(|r| has_phase(r, phase.key)).collect(),
        );
        if la.is_empty() && lb.is_empty() {
            continue;
        }
        println!("\n### {}\n", phase.title);
        println!(
            "{:<26} {:>16} {:>16} {:>14}  verdict",
            "field", a, b, "delta"
        );
        for (field, _title, nd) in phase.columns {
            let sa = field_stats(&la, phase, field);
            let sb = field_stats(&lb, phase, field);
            let (fa, fb) = (
                sa.as_ref()
                    .map(|s| fmt_value(s, *nd))
                    .unwrap_or_else(|| "-".into()),
                sb.as_ref()
                    .map(|s| fmt_value(s, *nd))
                    .unwrap_or_else(|| "-".into()),
            );
            let (Some(sa), Some(sb)) = (sa, sb) else {
                println!(
                    "{:<26} {:>16} {:>16} {:>14}  not measured",
                    format!("{}.{}", phase.key, field),
                    fa,
                    fb,
                    "-"
                );
                continue;
            };
            let delta = sb.median - sa.median;
            let noise = sa.noise().max(sb.noise());
            let enough = sa.enough_runs() && sb.enough_runs();
            let verdict = if !enough {
                // A single run cannot establish a spread, let alone a win.
                // Keep the numbers visible, but say what they are.
                anecdotal = true;
                format!("anecdotal (n {} vs {})", sa.n, sb.n)
            } else if delta.abs() <= noise {
                format!("same (noise ±{noise:.nd$})")
            } else if sa.median == 0.0 {
                // A percentage against zero is not a percentage; report the
                // absolute change instead of a fabricated +0 %.
                format!("n/a (baseline 0, {:+.nd$})", delta)
            } else {
                let pct = delta / sa.median * 100.0;
                format!("{pct:+.0}%")
            };
            println!(
                "{:<26} {:>16} {:>16} {:>+14.nd$}  {verdict}",
                format!("{}.{}", phase.key, field),
                fa,
                fb,
                delta
            );
        }
    }
    println!(
        "\n'verdict' compares the medians against the observed spread; a change\n\
         smaller than that spread is not a result. With fewer than 3 runs per\n\
         label it says 'anecdotal' instead of inventing a winner."
    );
    if strict && (anecdotal || display_differ.is_some() || !warnings.is_empty()) {
        return Err(
            "--strict: the two sides are not comparable (fewer than three runs per label, or \
             the warnings above)"
                .to_string(),
        );
    }
    Ok(())
}

fn print_table(header: &[String], rows: &[TableRow]) {
    let mut table: Vec<Vec<String>> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut cells = vec![row.label.clone()];
        cells.extend(row.cells.iter().cloned());
        cells.push(row.runs.to_string());
        table.push(cells);
    }

    let mut width: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for row in &table {
        for (i, c) in row.iter().enumerate() {
            width[i] = width[i].max(c.chars().count());
        }
    }

    let line = |cells: &[String]| {
        let padded: Vec<String> = cells
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c:<width$}", width = width[i]))
            .collect();
        format!("| {} |", padded.join(" | "))
    };

    println!("{}", line(header));
    let sep: Vec<String> = width.iter().map(|w| "-".repeat(w + 2)).collect();
    println!("|{}|", sep.join("|"));
    for row in &table {
        println!("{}", line(row));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(label: &str, mem: f64, ms: f64) -> Run {
        Run {
            schema_version: SCHEMA_VERSION,
            label: label.into(),
            clean: true,
            idle: Some(IdlePhase {
                mem_mib: mem,
                peak_mib: mem + 10.0,
                ..Default::default()
            }),
            render: Some(RenderPhase {
                comp_ms_per_frame: ms,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn median_of_odd_and_even_counts() {
        assert_eq!(stats(vec![3.0, 1.0, 2.0]).unwrap().median, 2.0);
        assert_eq!(stats(vec![4.0, 1.0]).unwrap().median, 2.5);
        assert!(stats(vec![]).is_none());
    }

    #[test]
    fn noise_floor_grows_with_spread() {
        let tight = stats(vec![100.0, 100.0, 100.0]).unwrap();
        let loose = stats(vec![80.0, 100.0, 120.0]).unwrap();
        assert!(loose.noise() > tight.noise());
        assert!((tight.noise() - 2.0).abs() < 1e-9, "2 % floor");
    }

    #[test]
    fn json_round_trips_through_the_schema() {
        let r = run("labwc", 120.0, 0.05);
        let text = serde_json::to_string(&r).unwrap();
        let back: Run = serde_json::from_str(&text).unwrap();
        assert_eq!(back.label, "labwc");
        assert_eq!(back.idle.unwrap().mem_mib, 120.0);
        assert_eq!(back.render.unwrap().comp_ms_per_frame, 0.05);
    }

    #[test]
    fn missing_phase_is_null_and_old_files_still_load() {
        let legacy = r#"{"label":"gnome","idle":{"mem_mib":700}}"#;
        let r: Run = serde_json::from_str(legacy).unwrap();
        assert_eq!(r.idle.unwrap().mem_mib, 700.0);
        assert!(r.render.is_none());
        assert_eq!(r.schema_version, 0);
    }

    #[test]
    fn v3_fixture_without_new_fields_reads_as_missing_not_zero() {
        let text = include_str!("../tests/fixtures/v3-missing-other-cpu.json");
        let r: Run = serde_json::from_str(text).unwrap();
        assert_eq!(r.schema_version, 3);

        let idle = r.idle.clone().unwrap();
        assert_eq!(idle.other_cpu_pct, None);
        assert_eq!(idle.measured_s, None);
        assert!(
            cell(&r, &IDLE, "other_cpu_pct").is_none(),
            "a v3 file without other_cpu_pct must print -, not 0.00"
        );

        let windows = r.windows.clone().unwrap();
        assert!(!windows.delta_valid, "the old file never recorded validity");
        assert_eq!(windows.baseline_drift_mib, None);
        assert_eq!(windows.post_close_drift_mib, None);
        assert!(
            cell(&r, &WINDOWS, "per_window_mib").is_none(),
            "a legacy clamped 0.0 must not read as a measured per-window cost"
        );
        assert!(cell(&r, &WINDOWS, "post_close_drift_mib").is_none());
        assert_eq!(cell(&r, &WINDOWS, "delta_mib"), Some(0.0));
        assert_eq!(
            cell(&r, &WINDOWS, "residual_mib"),
            Some(-2.0),
            "residual_mib keeps its v3 meaning"
        );

        let render = r.render.clone().unwrap();
        assert_eq!(render.self_cpu_s, None);
        assert_eq!(render.session_cpu_excl_self_s, None);
        assert!(r.features.is_none());
    }

    #[test]
    fn features_block_keeps_stale_versions_separate() {
        let globals = vec![
            crate::wlscan::Global {
                name: "wl_seat".into(),
                version: 2,
            },
            crate::wlscan::Global {
                name: "wl_compositor".into(),
                version: 4,
            },
        ];
        let block = Features::from_scan(&crate::wlscan::evaluate(&globals));
        assert_eq!(block.stale, 1);
        let seat = block.items.iter().find(|i| i.name == "wl_seat").unwrap();
        assert_eq!(seat.version, Some(2));
        assert!(seat.stale, "wl_seat 2 is below the useful version");
        assert_eq!(seat.tier, "core");
        assert_eq!(block.supported, 2, "advertised coverage includes stale");
    }

    #[test]
    fn an_invalid_window_delta_has_no_per_window_number() {
        let r = Run {
            windows: Some(WindowsPhase {
                extra: 2,
                delta_mib: -28.2,
                delta_valid: false,
                per_window_mib: None,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(cell(&r, &WINDOWS, "per_window_mib").is_none());
        assert_eq!(cell(&r, &WINDOWS, "delta_mib"), Some(-28.2));
    }

    #[test]
    fn a_peak_block_without_an_average_reads_as_missing_not_zero() {
        let v3 = r#"{"peaks":{"ram_peak_mib":852.0,"cpu_peak_pct":182.1}}"#;
        let r: Run = serde_json::from_str(v3).unwrap();
        let peaks = r.peaks.clone().unwrap();
        assert_eq!(peaks.cpu_peak_pct, 182.1);
        assert_eq!(peaks.cpu_avg_pct, None);
        assert!(cell(&r, &PEAKS, "cpu_avg_pct").is_none());
        // and a run that has it still sorts and prints
        let r = Run {
            peaks: Some(PeakPhase {
                cpu_avg_pct: Some(41.5),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(cell(&r, &PEAKS, "cpu_avg_pct"), Some(41.5));
    }

    #[test]
    fn nan_and_inf_never_reach_the_median() {
        assert_eq!(
            stats(vec![f64::NAN, 2.0, f64::INFINITY]).unwrap().median,
            2.0
        );
    }
}
