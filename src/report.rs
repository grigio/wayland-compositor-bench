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
    pub dur_s: f64,
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
    /// session CPU not attributable to the compositor or to this benchmark
    pub other_cpu_pct: f64,
    pub wakeups_per_s: f64,
    pub comp_pss_mib: f64,
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
    pub baseline_mib: f64,
    pub with_windows_mib: f64,
    /// working set added by the extra toplevels
    pub delta_mib: f64,
    /// GPU-accessible memory the benchmark's clients hold with the toplevels
    /// open, from DRM fdinfo. Buffers are pooled, so there is no useful delta.
    pub client_gpu_mib: f64,
    /// GPU-accessible memory the compositor holds at the same moment
    pub comp_gpu_mib: f64,
    /// working set delta per extra toplevel
    pub per_window_mib: f64,
    /// share of the working set delta charged to the compositor's process tree
    pub comp_delta_mib: f64,
    /// the rest: the clients that were opened
    pub client_delta_mib: f64,
    pub residual_mib: f64,
}

/// One frame-timing scenario: a single animated surface, or that surface plus N
/// extra toplevels drawing every frame.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RenderPhase {
    pub scenario: String,
    pub dur_s: f64,
    /// frames the compositor actually presented
    pub frames: i64,
    pub dropped: i64,
    pub fps: f64,
    pub target_hz: f64,
    /// the rate the compositor actually settled into
    pub cadence_hz: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
    /// "presentation" when wp_presentation feedback arrived, else "frame_clock"
    pub pacing_source: String,
    pub comp_ms_per_frame: f64,
    pub comp_ns_per_frame: f64,
    /// estimated from DRM client counters; null when the driver exposes none
    pub gpu_ms_per_frame: Option<f64>,
    pub app_ms_per_frame: f64,
    pub session_cpu_s: f64,
    /// this benchmark's own CPU time during the same window
    pub self_cpu_s: f64,
    /// `session_cpu_s - self_cpu_s`, the honest session figure
    pub session_cpu_excl_self_s: f64,
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
    /// "on" or "off": whether the window used the compositor's blur
    pub blur: String,
    /// high / medium / low, derived from `checks`
    pub confidence: String,
    pub checks: Vec<Check>,
    pub clean: bool,
    pub contaminants: Vec<Contaminant>,
    pub env: Env,
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
}

impl Stats {
    /// Half the observed range, floored at 2 % of the median. Used as the noise
    /// floor when deciding whether two labels actually differ.
    pub fn noise(&self) -> f64 {
        let spread = if self.n > 1 {
            (self.max - self.min) / 2.0
        } else {
            0.0
        };
        spread.max(self.median.abs() * 0.02)
    }
}

pub fn stats(mut v: Vec<f64>) -> Option<Stats> {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    let median = if n.is_multiple_of(2) {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    } else {
        v[n / 2]
    };
    Some(Stats {
        n,
        median,
        min: v[0],
        max: v[n - 1],
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
        ("procs", "procs", 0),
        ("cpu_pct", "CPU %", 2),
        ("comp_cpu_pct", "comp CPU %", 2),
        ("other_cpu_pct", "other CPU %", 2),
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
        ("residual_mib", "residual", 0),
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
        ("comp_ms_per_frame", "comp ms/frame", 3),
        ("gpu_ms_per_frame", "gpu ms/frame", 3),
        ("psys_w", "psys W", 2),
        ("batt_w", "bat W", 2),
        ("p95_ms", "p95 ms", 2),
        ("dropped", "dropped", 0),
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
        ("comp_ms_per_frame", "comp ms/frame", 3),
        ("gpu_ms_per_frame", "gpu ms/frame", 3),
        ("psys_w", "psys W", 2),
        ("batt_w", "bat W", 2),
        ("p95_ms", "p95 ms", 2),
        ("dropped", "dropped", 0),
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
                "procs" => p.procs as f64,
                "cpu_pct" => p.cpu_pct,
                "comp_cpu_pct" => p.comp_cpu_pct,
                "other_cpu_pct" => p.other_cpu_pct,
                "wakeups_per_s" => p.wakeups_per_s,
                "psys_w" => p.psys_w?,
                "batt_w" => p.batt_w?,
                _ => return None,
            })
        }
        "windows" => {
            let p = run.windows.as_ref()?;
            Some(match field {
                "per_window_mib" => p.per_window_mib,
                "delta_mib" => p.delta_mib,
                "client_gpu_mib" => p.client_gpu_mib,
                "comp_delta_mib" => p.comp_delta_mib,
                "comp_gpu_mib" => p.comp_gpu_mib,
                "residual_mib" => p.residual_mib,
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
                "comp_ms_per_frame" => p.comp_ms_per_frame,
                "gpu_ms_per_frame" => p.gpu_ms_per_frame?,
                "p95_ms" => p.p95_ms,
                "dropped" => p.dropped as f64,
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
         gpu ms/frame = per-client GPU time from DRM fdinfo (estimate, null without counters).\n\
         RAM = anon+shmem working set: page cache follows what the session read, not what it costs.\n\
         dropped = missed frames against the observed cadence, not the panel's refresh rate.\n\
         extra toplevels: ws/window is the working-set delta per window; client buffers are\n\
         dmabufs, pooled by the client, so GPU memory is reported as a total, not per window.\n\
         Medians with [min-max] when runs disagree by more than 2 %."
    );
    Ok(())
}

/// `--compare A B`: medians side by side, with a noise floor so a difference
/// smaller than the run-to-run spread is not reported as a win.
pub fn compare(a: &str, b: &str, targets: &[PathBuf]) -> Result<(), String> {
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
    if let Some(d) = ra[0].env.display_delta(&rb[0].env) {
        println!("\n! displays differ ({d}) — RAM and power are not comparable");
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
            let (Some(sa), Some(sb)) = (sa, sb) else {
                continue;
            };
            let delta = sb.median - sa.median;
            let noise = sa.noise().max(sb.noise());
            let verdict = if delta.abs() <= noise {
                format!("same (noise ±{noise:.nd$})")
            } else {
                let pct = if sa.median != 0.0 {
                    delta / sa.median * 100.0
                } else {
                    0.0
                };
                format!("{pct:+.0}%")
            };
            println!(
                "{:<26} {:>16} {:>16} {:>+14.nd$}  {verdict}",
                format!("{}.{}", phase.key, field),
                fmt_value(&sa, *nd),
                fmt_value(&sb, *nd),
                delta
            );
        }
    }
    println!("\n'verdict' compares the medians against the observed spread; a change\nsmaller than that spread is not a result.");
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
