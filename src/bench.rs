//! The run sequence: settle, idle, frame timing, extra toplevels.
//!
//! What changed from the first version, and why:
//!
//! - Compositor CPU comes from `schedstat` (nanoseconds) instead of
//!   utime+stime (10 ms ticks). At 120 Hz a lean compositor burns ~2 ms of CPU
//!   per second: tick granularity is the whole signal.
//! - Frame intervals, throughput and drops come from `presentation_time` (when
//!   the frame was actually scanned out), not from GDK's frame counter, which
//!   counts the client's own paints.
//! - GPU time comes from the DRM client counters in `/proc/<pid>/fdinfo`.
//! - Two frame scenarios: one animated surface, and that surface plus N extra
//!   toplevels drawing every frame. A static desktop and a 126 px strip do not
//!   exercise a compositor.
//! - The idle phase waits for the session to go quiet instead of sleeping a
//!   fixed number of seconds.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gtk::prelude::*;

use crate::cli::Options;
use crate::power;
use crate::proc;
use crate::report;
use crate::sysinfo::{self, Snapshot};
use crate::wlscan;

const WINDOW_SETTLE_MS: u64 = 3000;
const CLOSE_SETTLE_MS: u64 = 1500;
/// Time given to GTK's transient buffers to drain before the pre-open baseline,
/// and again between the two baselines that detect drift.
const WINDOW_BASELINE_SETTLE_MS: u64 = 1000;
/// A baseline that moves more than this while idle makes the window delta
/// meaningless, however small the delta itself is.
const WINDOW_DRIFT_MIB: f64 = 5.0;
/// Peak sampler period. 25 ms is fine enough to see a window-open spike and
/// cheap enough (two small files per tick) to stay below 0.1 % CPU.
const PEAK_PERIOD_MS: u64 = 25;
/// Quiescence is decided in 2 s windows; the session is "settled" when the last
/// three are within 50 % of each other.
const QUIET_WINDOW_S: f64 = 2.0;

pub struct Summary {
    /// share of frames delivered at the observed cadence, one surface
    pub pacing_single: f64,
    /// the same with the extra toplevels open
    pub pacing_multi: f64,
    pub ram_mib: f64,
    pub ram_peak_mib: f64,
    pub cpu_pct: f64,
    /// compositor CPU at rest, % of one core
    pub comp_cpu_pct: f64,
    pub cpu_peak_pct: f64,
    /// session CPU outside the compositor process tree and this benchmark
    pub other_cpu_pct: f64,
    pub comp_mib: f64,
    pub comp_ms_per_frame: f64,
    /// compositor CPU per frame in the load case (multi, else single)
    pub comp_ms_per_frame_multi: f64,
    /// cadence that test settled into: its period is the frame budget
    pub cadence_hz_multi: f64,
    pub gpu_ms_per_frame: Option<f64>,
    pub feat_pct: f64,
    pub feat_supported: usize,
    pub feat_total: usize,
    pub feat_supported_all: usize,
    pub feat_total_all: usize,
    pub feat_extra: usize,
    pub feat_items: Vec<wlscan::Item>,
    pub confidence: &'static str,
    /// session CPU during the run, one point per ~100 ms
    pub cpu_series: Vec<CpuPoint>,
    /// phase boundaries on the same clock
    pub cpu_marks: Vec<CpuMark>,
}

pub struct BenchUi {
    pub app: gtk::Application,
    pub window: gtk::ApplicationWindow,
    pub area: gtk::DrawingArea,
    pub surface: gtk::Box,
    pub stack: gtk::Stack,
    pub start: gtk::Button,
    pub progress: gtk::ProgressBar,
    pub status: gtk::Label,
    pub grid: gtk::Grid,
    pub results_expander: gtk::Expander,
    pub row: Rc<Cell<i32>>,
    pub comp: Option<proc::Compositor>,
    /// monitor refresh in Hz, used when the compositor reports no interval
    pub refresh_hz: f64,
    /// logical size of the output, for the run's display block
    pub logical: (u32, u32),
    pub scale: f64,
    /// true while a run is in progress: the live labels pause so the benchmark
    /// is not measuring its own updater
    pub busy: Rc<Cell<bool>>,
    /// the blur chip's state, so the run JSON can say whether blur was on
    pub blur_off: Rc<Cell<bool>>,
    /// set when `--run` could not write its JSON; the process must exit non-zero
    pub failed: Rc<Cell<bool>>,
    pub summary: Rc<dyn Fn(Summary)>,
    pub scan: Rc<RefCell<Option<wlscan::Scan>>>,
    pub opts: Options,
}

impl Clone for BenchUi {
    fn clone(&self) -> Self {
        Self {
            app: self.app.clone(),
            window: self.window.clone(),
            area: self.area.clone(),
            surface: self.surface.clone(),
            stack: self.stack.clone(),
            start: self.start.clone(),
            progress: self.progress.clone(),
            status: self.status.clone(),
            grid: self.grid.clone(),
            results_expander: self.results_expander.clone(),
            row: self.row.clone(),
            comp: self.comp.clone(),
            refresh_hz: self.refresh_hz,
            logical: self.logical,
            scale: self.scale,
            busy: self.busy.clone(),
            blur_off: self.blur_off.clone(),
            failed: self.failed.clone(),
            summary: self.summary.clone(),
            scan: self.scan.clone(),
            opts: self.opts.clone(),
        }
    }
}

impl BenchUi {
    fn clear(&self) {
        while let Some(child) = self.grid.first_child() {
            self.grid.remove(&child);
        }
        self.row.set(0);
    }

    fn add(&self, key: &str, value: &str) {
        if std::env::var("WG_BENCH_PRINT").is_ok() && !key.is_empty() {
            println!("{key}\t{value}");
        }
        let r = self.row.get();
        let k = gtk::Label::new(Some(key));
        k.set_xalign(0.0);
        k.set_size_request(160, -1);
        k.add_css_class("key");
        let v = gtk::Label::new(Some(value));
        v.set_xalign(0.0);
        v.set_hexpand(true);
        v.set_wrap(true);
        v.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        v.set_width_chars(18);
        v.set_max_width_chars(26);
        v.add_css_class("value");
        v.set_selectable(true);
        self.grid.attach(&k, 0, r, 1, 1);
        self.grid.attach(&v, 1, r, 1, 1);
        self.row.set(r + 1);
    }

    fn section(&self, title: &str) {
        let l = gtk::Label::new(Some(title));
        l.set_xalign(0.0);
        l.add_css_class("section");
        self.grid.attach(&l, 0, self.row.get(), 2, 1);
        self.row.set(self.row.get() + 1);
    }

    fn set_status(&self, s: &str) {
        self.status.set_text(s);
    }
}

// --------------------------------------------------------------- sampling ---

/// Ceiling on the timeline: a 100 ms cadence means ~20 min of run. Past that the
/// chart stops rather than growing a vector without bound.
const MAX_SERIES_POINTS: usize = 12000;

/// (seconds since the run started, CPU % of one core)
type CpuPoint = (f32, f32);
/// (seconds since the run started, phase name)
type CpuMark = (f32, &'static str);

/// The session CPU timeline, kept alongside the peaks. Peaks answer "how bad did
/// it get"; this answers "when", which is what the strip under the radar shows.
#[derive(Default)]
struct Series {
    /// set by the first sample after a reset, so the run starts at t = 0
    t0: Option<Instant>,
    points: Vec<CpuPoint>,
    /// phase boundaries on the same clock
    marks: Vec<CpuMark>,
    /// Baseline for the next sample, in the shared state so whoever resets the
    /// timeline (main thread or sampler) re-baselines it as well. Keeping these
    /// in the thread-local loop made the first post-settle sample span the
    /// whole settle period.
    last_t: Option<Instant>,
    last_cpu: u64,
}

impl Series {
    /// Start a new measurable run at `now`, with the cgroup counter `cpu` as
    /// the baseline for the next sample.
    fn reset_at(&mut self, now: Instant, cpu: u64) {
        self.t0 = None;
        self.points.clear();
        self.marks.clear();
        self.last_t = Some(now);
        self.last_cpu = cpu;
    }

    fn at(&mut self, now: Instant) -> f32 {
        let t0 = *self.t0.get_or_insert(now);
        now.duration_since(t0).as_secs_f32()
    }

    fn push(&mut self, now: Instant, pct: f64) {
        if self.points.len() >= MAX_SERIES_POINTS {
            return;
        }
        let t = self.at(now);
        self.points.push((t, pct as f32));
    }

    fn mark(&mut self, now: Instant, name: &'static str) {
        let t = self.at(now);
        self.marks.push((t, name));
    }
}

/// Average CPU load across the run, in % of one core.
///
/// Every sample is the load measured between the previous one and its own
/// timestamp, so it describes the interval *ending* at its timestamp. The
/// trapezoid rule is the matching estimator: each point is the right edge of
/// the interval before it and the left edge of the interval after it. Weighting
/// `points[i]` by the following interval (the old code) attributed every sample
/// to the wrong side and dropped the last real one.
fn mean_cpu(points: &[CpuPoint]) -> f64 {
    let mut weighted = 0.0;
    let mut total = 0.0;
    for pair in points.windows(2) {
        let dt = f64::from(pair[1].0 - pair[0].0);
        if dt > 0.0 {
            weighted += (f64::from(pair[0].1) + f64::from(pair[1].1)) * 0.5 * dt;
            total += dt;
        }
    }
    if total > 0.0 {
        weighted / total
    } else {
        // Fewer than two samples, or a run short enough to share one timestamp:
        // the first reading is all we have.
        points.first().map_or(0.0, |p| f64::from(p.1))
    }
}

/// Polled peaks, on their own thread so GTK's main loop cannot smooth them out
/// and so `memory.peak` (root-only to reset here) is not needed.
struct Sampler {
    stop: Arc<AtomicBool>,
    peak_mem: Arc<AtomicU64>,
    peak_cpu: Arc<AtomicU64>,
    series: Arc<Mutex<Series>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Sampler {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let peak_mem = Arc::new(AtomicU64::new(0));
        let peak_cpu = Arc::new(AtomicU64::new(0));
        let series = Arc::new(Mutex::new(Series::default()));
        let (s, m, c, se) = (
            stop.clone(),
            peak_mem.clone(),
            peak_cpu.clone(),
            series.clone(),
        );
        let join = std::thread::spawn(move || {
            let dir = proc::session_cgroup();
            let stat_path = dir.join("memory.stat");
            if let Ok(mut se) = se.lock() {
                se.reset_at(Instant::now(), proc::cpu_usage_usec(dir));
            }
            while !s.load(Ordering::Relaxed) {
                // Peak *working set*, not `memory.current`: the latter is mostly
                // page cache, which moves with whatever else the session reads.
                if let Ok(text) = std::fs::read_to_string(&stat_path) {
                    let mut ws = 0u64;
                    for line in text.lines() {
                        let mut it = line.split_whitespace();
                        let (Some(k), Some(v)) = (it.next(), it.next()) else {
                            continue;
                        };
                        if k == "anon" || k == "shmem" {
                            ws += v.parse::<u64>().unwrap_or(0);
                        }
                    }
                    if ws > 0 {
                        m.fetch_max(ws, Ordering::Relaxed);
                    }
                }
                let cpu = proc::cpu_usage_usec(dir);
                let now = Instant::now();
                let Ok(mut se) = se.lock() else {
                    break;
                };
                let Some(last_t) = se.last_t else {
                    se.last_t = Some(now);
                    se.last_cpu = cpu;
                    drop(se);
                    std::thread::sleep(Duration::from_millis(PEAK_PERIOD_MS));
                    continue;
                };
                let dt = now.duration_since(last_t).as_secs_f64();
                if dt >= 0.10 && cpu >= se.last_cpu {
                    let pct = (cpu - se.last_cpu) as f64 / 1e6 / dt * 100.0;
                    // positive floats order the same as their bit patterns, so
                    // fetch_max on the bits is a numeric max
                    c.fetch_max(pct.to_bits(), Ordering::Relaxed);
                    se.push(now, pct);
                    se.last_t = Some(now);
                    se.last_cpu = cpu;
                }
                drop(se);
                std::thread::sleep(Duration::from_millis(PEAK_PERIOD_MS));
            }
        });
        Self {
            stop,
            peak_mem,
            peak_cpu,
            series,
            join: Some(join),
        }
    }

    /// Starts the measurable run: peaks back to zero, timeline back to t = 0.
    /// The baseline is re-read here, not left for the sampling thread, so the
    /// first sample cannot span the settle period.
    fn reset(&self) {
        self.peak_mem.store(0, Ordering::Relaxed);
        self.peak_cpu.store(0, Ordering::Relaxed);
        let now = Instant::now();
        let cpu = proc::cpu_usage_usec(proc::session_cgroup());
        if let Ok(mut se) = self.series.lock() {
            se.reset_at(now, cpu);
        }
    }

    /// Names the phase that starts now, for the verticals on the timeline.
    /// It does not touch the sampling baseline: `reset` already did.
    fn mark(&self, name: &'static str) {
        let Ok(mut se) = self.series.lock() else {
            return;
        };
        se.mark(Instant::now(), name);
    }

    fn peaks(&self) -> (f64, f64) {
        (
            self.peak_mem.load(Ordering::Relaxed) as f64 / 1048576.0,
            f64::from_bits(self.peak_cpu.load(Ordering::Relaxed)),
        )
    }

    fn series(&self) -> (Vec<CpuPoint>, Vec<CpuMark>) {
        let Ok(se) = self.series.lock() else {
            return (Vec::new(), Vec::new());
        };
        (se.points.clone(), se.marks.clone())
    }

    fn finish(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// One instant of everything a frame test needs before and after.
#[derive(Clone)]
struct Probe {
    comp_ns: u64,
    tasks: Vec<proc::Task>,
    gpu: proc::Gpu,
    /// this benchmark's own DRM client counters, so the frame test can report
    /// both sides of the GPU work
    app_gpu: proc::Gpu,
    session_usec: u64,
    app_ns: u64,
    at: Instant,
}

impl Probe {
    /// Before the measured window: scans first, then the session/app counters
    /// and the timestamp, so the probe's own `/proc` walk is outside the window.
    fn before(pid: Option<u32>) -> Self {
        let tasks = pid.map(proc::tasks).unwrap_or_default();
        let comp_ns = tasks.iter().map(|t| t.ns).sum();
        let gpu = pid.map(proc::gpu).unwrap_or_default();
        let app_gpu = proc::gpu(std::process::id());
        let session_usec = proc::cpu_usage_usec(proc::session_cgroup());
        let app_ns = proc::cpu_ns(std::process::id());
        Self {
            comp_ns,
            tasks,
            gpu,
            app_gpu,
            session_usec,
            app_ns,
            at: Instant::now(),
        }
    }

    /// After the measured window: counters and timestamp first, then the
    /// scans, so the probe does not charge its own work to the session.
    fn after(pid: Option<u32>) -> Self {
        let session_usec = proc::cpu_usage_usec(proc::session_cgroup());
        let app_ns = proc::cpu_ns(std::process::id());
        let at = Instant::now();
        let tasks = pid.map(proc::tasks).unwrap_or_default();
        let comp_ns = tasks.iter().map(|t| t.ns).sum();
        let gpu = pid.map(proc::gpu).unwrap_or_default();
        let app_gpu = proc::gpu(std::process::id());
        Self {
            comp_ns,
            tasks,
            gpu,
            app_gpu,
            session_usec,
            app_ns,
            at,
        }
    }
}

fn thread_shares(
    before: &[proc::Task],
    after: &[proc::Task],
    total_ns: u64,
) -> Vec<report::ThreadShare> {
    if total_ns == 0 {
        return Vec::new();
    }
    let before_ns: std::collections::HashMap<u32, u64> =
        before.iter().map(|t| (t.tid, t.ns)).collect();
    let mut out: Vec<report::ThreadShare> = after
        .iter()
        .filter_map(|t| {
            let prev = *before_ns.get(&t.tid)?;
            let delta = t.ns.saturating_sub(prev);
            (delta > 0).then(|| report::ThreadShare {
                name: t.comm.clone(),
                tid: t.tid,
                share_pct: delta as f64 / total_ns as f64 * 100.0,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.share_pct
            .partial_cmp(&a.share_pct)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(4);
    out
}

// ---------------------------------------------------------------- frames ----

#[derive(Default)]
struct FrameStats {
    /// keyed by frame counter: frame_time, presentation_time, refresh_interval
    frames: BTreeMap<i64, (i64, i64, i64)>,
}

fn collect(clock: &gtk::gdk::FrameClock, st: &mut FrameStats, min_frame_time: i64) {
    let start = clock.history_start();
    let cur = clock
        .current_timings()
        .map(|t| t.frame_counter())
        .unwrap_or(start);
    for i in start..=cur {
        let Some(t) = clock.timings(i) else { continue };
        let ft = t.frame_time();
        if ft < min_frame_time {
            continue;
        }
        st.frames.insert(
            t.frame_counter(),
            (ft, t.presentation_time(), t.refresh_interval()),
        );
    }
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((v.len() - 1) as f64 * p).round() as usize;
    v[idx]
}

struct Pacing {
    frames: usize,
    dropped: i64,
    fps: f64,
    p50: f64,
    p95: f64,
    max: f64,
    /// what the display advertises
    target_hz: f64,
    /// the rate the compositor actually settled into (120 Hz panel at 60 fps -> 60)
    cadence_hz: f64,
    source: &'static str,
}

/// Frame pacing from presentation times when the compositor provides them:
/// those are scanout timestamps, so a late composite or a missed vblank shows
/// up here, while GDK's own counter would blame the client.
///
/// Drops are counted against the *observed* cadence, not the advertised refresh:
/// a heavy scene that settles into a clean 60 fps on a 120 Hz panel is not
/// dropping every other frame, it is running at half rate. The cadence is
/// reported so that distinction is visible.
fn pacing(v: &[(i64, i64, i64)], fallback_hz: f64) -> Option<Pacing> {
    // refresh: median of the nonzero intervals the compositor reported
    let mut intervals: Vec<f64> = v
        .iter()
        .filter(|(_, _, ri)| *ri > 0)
        .map(|(_, _, ri)| *ri as f64 / 1000.0)
        .collect();
    let advertised = if intervals.is_empty() {
        fallback_hz
    } else {
        1000.0 / pct(&mut intervals, 0.5)
    };
    let advertised = if advertised > 1.0 { advertised } else { 60.0 };

    let presented: Vec<i64> = v
        .iter()
        .map(|(_, pt, _)| *pt)
        .filter(|pt| *pt > 0)
        .collect();
    let (times, source): (Vec<i64>, &'static str) = if presented.len() >= 3 {
        (presented, "presentation")
    } else {
        (v.iter().map(|(ft, _, _)| *ft).collect(), "frame_clock")
    };
    if times.len() < 3 {
        return None;
    }

    let mut deltas = Vec::with_capacity(times.len() - 1);
    for w in times.windows(2) {
        deltas.push((w[1] - w[0]) as f64 / 1000.0); // ms
    }
    let span = (times[times.len() - 1] - times[0]) as f64 / 1e6;
    if span <= 0.0 {
        return None;
    }
    let fps = (times.len() - 1) as f64 / span;

    let mut sorted = deltas.clone();
    let p50 = pct(&mut sorted, 0.50);
    let p95 = pct(&mut sorted, 0.95);
    let max = pct(&mut sorted, 1.0);

    let advertised_period = 1000.0 / advertised;
    let cadence = if p50 > advertised_period * 1.3 {
        p50
    } else {
        advertised_period
    };
    let cadence_hz = 1000.0 / cadence;
    let dropped = deltas
        .iter()
        .map(|d| ((d / cadence).round() as i64 - 1).max(0))
        .sum();

    Some(Pacing {
        frames: times.len(),
        dropped,
        fps,
        p50,
        p95,
        max,
        target_hz: advertised,
        cadence_hz,
        source,
    })
}

/// Extra toplevel that repaints every frame, so compositing costs more than one
/// static surface plus a strip.
struct TestWin {
    win: gtk::Window,
    area: gtk::DrawingArea,
    phase: Rc<Cell<f64>>,
}

fn test_window(app: &gtk::Application, i: u32) -> TestWin {
    let (w, h) = match i % 3 {
        0 => (900, 620),
        1 => (640, 430),
        _ => (460, 320),
    };
    let win = gtk::Window::builder()
        .application(app)
        .title(format!("bench window {i}"))
        .default_width(w)
        .default_height(h)
        .build();
    let area = gtk::DrawingArea::new();
    let phase = Rc::new(Cell::new(i as f64 * 0.37));
    let ph = phase.clone();
    let tint = (i % 5) as f64;
    area.set_draw_func(move |_, cr, w, h| {
        let p = ph.get();
        let (w, h) = (w as f64, h as f64);
        cr.set_source_rgb(0.12 + 0.10 * tint, 0.22, 0.40);
        cr.paint().expect("paint");
        // A handful of moving blocks: real damage every frame, cheap on the CPU
        // so the client does not become the bottleneck.
        for k in 0..10 {
            let f = k as f64;
            let v = (p + f * 0.31).sin() * 0.5 + 0.5;
            let bw = w / 10.0;
            let bh = 20.0 + v * (h - 60.0);
            cr.set_source_rgba(0.35 + 0.5 * v, 0.55, 1.0, 0.7);
            cr.rectangle(f * bw + 3.0, h - bh - 10.0, bw - 6.0, bh);
            cr.fill().ok();
        }
        cr.set_source_rgb(0.95, 0.95, 1.0);
        cr.select_font_face(
            "Sans",
            gtk::cairo::FontSlant::Normal,
            gtk::cairo::FontWeight::Normal,
        );
        cr.set_font_size(22.0);
        cr.move_to(16.0, 32.0);
        cr.show_text(&format!("#{i}")).ok();
    });
    win.set_child(Some(&area));
    win.present();
    TestWin { win, area, phase }
}

// ------------------------------------------------------------------- run ----

pub fn start(ui: BenchUi) {
    ui.start.set_sensitive(false);
    ui.progress.set_fraction(0.02);
    ui.progress.set_visible(true);
    ui.surface.set_visible(true);
    ui.stack.set_visible_child(&ui.surface);
    ui.status.set_visible(true);
    ui.results_expander.set_expanded(false);
    ui.clear();
    ui.set_status("starting…");
    glib::spawn_future_local(async move {
        run(ui).await;
    });
}

struct Settle {
    seconds: f64,
}

/// Session CPU split into "the thing being measured", "this tool" and
/// "everything else". The last number is what turns an unexplained 46 % idle
/// figure into a diagnosis.
///
/// A process that did not exist at the start of the window is skipped: its
/// `/proc` CPU time is its whole lifetime, and charging that to this window is
/// how a short-lived helper turns into a fake 30 % offender.
fn cpu_attribution(
    before: &proc::CpuSnapshot,
    after: &proc::CpuSnapshot,
    seconds: f64,
    exclude: &[u32],
) -> (f64, Vec<(String, f64)>) {
    if seconds <= 0.0 {
        return (0.0, Vec::new());
    }
    let total_pct =
        after.total_usec.saturating_sub(before.total_usec) as f64 / 1e6 / seconds * 100.0;
    let before_ns: std::collections::HashMap<u32, u64> = before
        .procs
        .iter()
        .map(|(pid, _, ns)| (*pid, *ns))
        .collect();
    let mut excluded_pct = 0.0;
    let mut top: Vec<(String, f64)> = Vec::new();
    for (pid, name, ns) in &after.procs {
        let Some(previous) = before_ns.get(pid) else {
            continue;
        };
        let delta = ns.saturating_sub(*previous);
        let pct = delta as f64 / 1e9 / seconds * 100.0;
        if exclude.contains(pid) {
            excluded_pct += pct;
        } else if pct > 0.05 {
            top.push((name.clone(), pct));
        }
    }
    top.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    top.truncate(3);
    ((total_pct - excluded_pct).max(0.0), top)
}

/// Wait until session CPU stops moving. Lazy portals and login animations land
/// seconds after the window appears, so a fixed sleep is either too short or
/// wasteful.
async fn settle(ui: &BenchUi, max_s: f64) -> Settle {
    let start = Instant::now();
    let mut windows: Vec<f64> = Vec::new();
    loop {
        let t0 = Instant::now();
        let c0 = proc::cpu_usage_usec(proc::session_cgroup());
        ui.set_status(&format!(
            "waiting for the session to settle ({:.0} s, max {:.0} s)…",
            start.elapsed().as_secs_f64(),
            max_s
        ));
        glib::timeout_future(Duration::from_secs_f64(QUIET_WINDOW_S)).await;
        let dt = t0.elapsed().as_secs_f64();
        let c1 = proc::cpu_usage_usec(proc::session_cgroup());
        windows.push((c1.saturating_sub(c0)) as f64 / 1e6 / dt * 100.0);

        let elapsed = start.elapsed().as_secs_f64();
        if elapsed >= max_s {
            break;
        }
        if windows.len() >= 3 {
            let last = &windows[windows.len() - 3..];
            let hi = last.iter().cloned().fold(0.0, f64::max);
            let lo = last.iter().cloned().fold(f64::MAX, f64::min);
            if hi <= (lo * 1.5).max(lo + 0.15) && lo < 5.0 {
                break;
            }
        }
    }
    let seconds = start.elapsed().as_secs_f64();
    Settle { seconds }
}

async fn run(ui: BenchUi) {
    let opts = ui.opts.clone();
    let comp = ui.comp.clone();
    let pid = comp.as_ref().map(|c| c.pid);
    let comp_name = comp
        .as_ref()
        .map(|c| c.name.clone())
        .unwrap_or_else(|| "?".into());
    // The socket owner decides what this session is; `XDG_CURRENT_DESKTOP` is
    // checked against it, not trusted on its own.
    let desktop = proc::desktop(comp.as_ref());
    let run_t0 = Instant::now();
    ui.busy.set(true);

    // Anything big already running makes every number incomparable. The
    // compositor's own process tree is part of the session on purpose.
    let mut exclude: Vec<u32> = vec![std::process::id()];
    if let Some(p) = pid {
        exclude.extend(proc::pss_tree(p).1);
    }
    let contaminants = sysinfo::contaminants(&exclude);

    // ---------------------------------------------------------- context ---
    let hint = sysinfo::DisplayHint {
        logical_w: ui.logical.0,
        logical_h: ui.logical.1,
        scale: ui.scale,
        compositor_hz: Some(ui.refresh_hz),
    };
    let env = sysinfo::Env::collect(hint, comp.as_ref());
    // Probe both once: `readable()` may fork `sudo` per domain, and the check
    // needs the same answer for `ok` and for `detail`.
    let rapl = power::readable();
    let batt = power::battery();

    let mut checks = vec![
        report::Check {
            name: "session cgroup".into(),
            ok: proc::cgroup_exact(),
            detail: proc::session_cgroup().display().to_string(),
        },
        report::Check {
            name: "compositor process".into(),
            ok: comp.is_some(),
            detail: match &comp {
                Some(c) => format!("{} (pid {}, via {})", c.name, c.pid, c.source),
                None => "not found".into(),
            },
        },
        report::Check {
            name: "desktop identity".into(),
            ok: !desktop.mismatch(),
            detail: if desktop.mismatch() {
                format!(
                    "{} (XDG_CURRENT_DESKTOP says {}, which is stale)",
                    desktop.name, desktop.env
                )
            } else {
                desktop.name.clone()
            },
        },
        report::Check {
            name: "display refresh".into(),
            ok: env.display.refresh_hz.is_some() || env.display.compositor_hz.is_some(),
            detail: match (env.display.refresh_hz, env.display.compositor_hz) {
                (Some(e), Some(c)) => format!("{e:.1} Hz EDID, {c:.1} Hz compositor"),
                (Some(e), None) => format!("{e:.1} Hz EDID"),
                (None, Some(c)) => format!("{c:.1} Hz compositor"),
                (None, None) => "unknown".into(),
            },
        },
        report::Check {
            name: "RAPL power".into(),
            ok: rapl.0 || rapl.1,
            detail: match rapl {
                (true, true) => "psys + package".into(),
                (true, false) => "psys only".into(),
                (false, true) => "package only".into(),
                (false, false) => "unreadable (root-only)".into(),
            },
        },
        report::Check {
            name: "battery discharging".into(),
            ok: batt
                .as_ref()
                .map(|(_, s)| s == "Discharging")
                .unwrap_or(false),
            detail: batt
                .as_ref()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| "no battery".into()),
        },
        report::Check {
            name: "clean session".into(),
            ok: contaminants.is_empty(),
            detail: if contaminants.is_empty() {
                format!("nothing over {} MiB", sysinfo::CONTAMINANT_MIB as u32)
            } else {
                contaminants
                    .iter()
                    .take(3)
                    .map(|(n, m)| format!("{n} {m:.0} MiB"))
                    .collect::<Vec<_>>()
                    .join(", ")
            },
        },
    ];

    let sampler = Sampler::start();

    // ------------------------------------------------------------- idle ---
    let settle = settle(&ui, opts.settle_s).await;
    let settle_s = settle.seconds;
    sampler.reset();
    sampler.mark("idle");
    ui.set_status(&format!(
        "measuring the idle session ({:.0} s)…",
        opts.idle_s
    ));
    ui.progress.set_fraction(0.25);
    // `begin` scans first and reads the counters last; `end` reads the counters
    // first and scans last. The scans are therefore outside the interval, and
    // the power samples bracket only the sleep.
    let i0 = Snapshot::begin(pid);
    let pw0 = power::sample();
    glib::timeout_future(Duration::from_secs_f64(opts.idle_s)).await;
    let pw1 = power::sample();
    let i1 = Snapshot::end(pid);
    let idle_s = i1
        .at
        .saturating_duration_since(i0.at)
        .as_secs_f64()
        .max(1e-6);
    let (cpu_pct, comp_pct) = i0.delta_cpu(&i1, idle_s);
    // The offenders and the headline number come from the same idle window and
    // the same exclusion set: compositor process tree plus this benchmark.
    let (other_cpu_pct, top_cpu) =
        cpu_attribution(&i0.cpu_snapshot(), &i1.cpu_snapshot(), idle_s, &exclude);
    let wakeups = i1.comp_wakeups.saturating_sub(i0.comp_wakeups) as f64 / idle_s;
    let (psys_w, pkg_w) = power::delta(pw0, pw1);
    let batt_status = batt
        .as_ref()
        .map(|(_, s)| s.clone())
        .unwrap_or_else(|| "unknown".into());
    let batt_w = power::battery_watts();
    let idle_peak_mib = sampler.peaks().0.max(i1.working_set_mib());
    let idle_threads = thread_shares(
        &i0.comp_tasks,
        &i1.comp_tasks,
        i1.comp_ns.saturating_sub(i0.comp_ns),
    );

    let top_cpu = top_cpu
        .iter()
        .map(|(name, pct)| format!("{name} {pct:.1}%"))
        .collect::<Vec<_>>()
        .join(", ");
    checks.push(report::Check {
        name: "quiet cpu".into(),
        ok: other_cpu_pct <= 5.0,
        detail: if top_cpu.is_empty() {
            format!("{other_cpu_pct:.1}% outside the compositor tree and this tool")
        } else {
            format!("{other_cpu_pct:.1}% outside the compositor tree and this tool: {top_cpu}")
        },
    });

    let comp_mib = i1.comp_pss_mib;
    ui.section("idle session");
    ui.add(
        "compositor",
        &format!(
            "{} (pid {}) · settled in {:.0} s",
            comp.as_ref().map(|c| c.name.as_str()).unwrap_or("?"),
            pid.unwrap_or(0),
            settle_s
        ),
    );
    ui.add("session memory", &sysinfo::fmt_mib(i1.working_set_mib()));
    ui.add(
        "  anon / shm / file",
        &format!(
            "{:.0} / {:.0} / {:.0} MiB",
            i1.anon_mib, i1.shmem_mib, i1.file_mib
        ),
    );
    ui.add(
        "  kernel / swap",
        &format!("{:.0} / {:.1} MiB", i1.kernel_mib, i1.swap_mib),
    );
    ui.add("  processes", &i1.procs.to_string());
    ui.add("session CPU", &format!("{cpu_pct:.2} % of one core"));
    ui.add("compositor CPU", &format!("{comp_pct:.2} % of one core"));
    ui.add(
        "other CPU",
        &format!("{other_cpu_pct:.2} % of one core (neither compositor nor this tool)"),
    );
    if !top_cpu.is_empty() {
        ui.add("  top CPU users", &top_cpu);
    }
    ui.add("compositor wakeups", &format!("{wakeups:.0} /s"));
    ui.add("compositor PSS", &format!("{:.0} MiB", comp_mib));
    ui.add(
        "platform / package power",
        &match (psys_w, pkg_w) {
            (Some(ps), Some(pk)) => format!("{ps:.1} W psys · {pk:.1} W pkg"),
            (Some(ps), None) => format!("{ps:.1} W psys"),
            _ => "not readable (root-only, see README)".to_string(),
        },
    );
    ui.add(
        "battery draw",
        &match batt_w {
            Some(w) => format!("{w:.1} W"),
            None => format!("n/a ({batt_status})"),
        },
    );
    if let Some(top) = idle_threads.first() {
        ui.add(
            "busiest thread",
            &format!("{} ({:.0}% of compositor CPU)", top.name, top.share_pct),
        );
    }

    // ------------------------------------------------------- frame tests ---
    sampler.mark("single");
    let single = frame_test(&ui, "single", opts.frames_s, &[], pid).await;
    ui.progress.set_fraction(0.55);

    // ----------------------------------------------------------- windows ---
    let n = opts.windows;
    ui.set_status(&format!("opening {n} toplevels…"));
    sampler.mark("+windows");
    // The single-surface frame test leaves transient GTK buffers behind. Take
    // two baselines a second apart: the second is the one used, and the
    // difference says whether the idle session was still moving.
    glib::timeout_future(Duration::from_millis(WINDOW_BASELINE_SETTLE_MS)).await;
    let base0 = Snapshot::take(pid);
    glib::timeout_future(Duration::from_millis(WINDOW_BASELINE_SETTLE_MS)).await;
    let base = Snapshot::take(pid);
    let baseline_drift = base.working_set_mib() - base0.working_set_mib();
    let wins: Vec<TestWin> = (0..n).map(|i| test_window(&ui.app, i)).collect();
    glib::timeout_future(Duration::from_millis(WINDOW_SETTLE_MS)).await;
    let with_wins = Snapshot::take(pid);
    let with_gpu_self = proc::gpu(std::process::id());
    let with_gpu_comp = pid.map(proc::gpu).unwrap_or_default();
    // Keep the signed delta: a negative value means the session shrank, which
    // is a measurement, not zero growth. The per-window figure is only written
    // when the delta is non-negative and the baseline was stable.
    let delta = with_wins.working_set_mib() - base.working_set_mib();
    let comp_delta = with_wins.comp_pss_mib - base.comp_pss_mib;
    let client_delta = delta - comp_delta;
    let delta_valid = n > 0 && delta >= 0.0 && baseline_drift.abs() <= WINDOW_DRIFT_MIB;
    let per_window = delta_valid.then(|| delta / f64::from(n));
    // Window buffers are dmabufs here: they never appear in the session cgroup,
    // and GTK keeps a large buffer pool, so the delta is ~0. Report what is
    // actually held instead of pretending a per-window GPU cost exists.
    // The compositor's number excludes the clients' buffers it imported.
    let client_gpu = with_gpu_self.resident as f64 / 1048576.0;
    let comp_gpu = with_gpu_comp.private as f64 / 1048576.0;

    let extras: Vec<(gtk::DrawingArea, Rc<Cell<f64>>)> = wins
        .iter()
        .map(|w| (w.area.clone(), w.phase.clone()))
        .collect();
    sampler.mark("multi");
    let multi = frame_test(&ui, "multi", opts.frames_s, &extras, pid).await;
    ui.progress.set_fraction(0.85);

    ui.set_status("closing the toplevels…");
    for w in &wins {
        w.win.close();
    }
    drop(wins);
    // Two post-close snapshots: the drift between them is the leak indicator,
    // while the difference against the pre-open baseline is how much was never
    // returned. They answer different questions and are reported separately.
    glib::timeout_future(Duration::from_millis(CLOSE_SETTLE_MS)).await;
    let post_close = Snapshot::take(pid);
    glib::timeout_future(Duration::from_millis(CLOSE_SETTLE_MS)).await;
    let after_close = Snapshot::take(pid);
    let residual = after_close.working_set_mib() - base.working_set_mib();
    let post_close_drift = after_close.working_set_mib() - post_close.working_set_mib();

    ui.section("memory");
    ui.add(
        "baseline (working set)",
        &sysinfo::fmt_mib(base.working_set_mib()),
    );
    ui.add(
        "  baseline drift",
        &format!(
            "{baseline_drift:+.1} MiB over {:.1} s",
            WINDOW_BASELINE_SETTLE_MS as f64 / 1000.0
        ),
    );
    ui.add(
        &format!("with {n} extra toplevels"),
        &format!(
            "{}  ({delta:+.0} MiB)",
            sysinfo::fmt_mib(with_wins.working_set_mib())
        ),
    );
    ui.add("  compositor share", &format!("{comp_delta:+.1} MiB"));
    ui.add(
        "  client share (same process)",
        &format!("{client_delta:+.1} MiB"),
    );
    ui.add(
        "GPU memory held",
        &format!(
            "client {client_gpu:.0} MiB · compositor {comp_gpu:.0} MiB (pooled, not per window)"
        ),
    );
    ui.add(
        "per extra toplevel",
        &match per_window {
            Some(v) => format!("{v:.1} MiB"),
            None if n == 0 => "not measured (no extra toplevels)".to_string(),
            None => format!(
                "not measured (delta {delta:+.1} MiB, baseline drift {baseline_drift:+.1} MiB)"
            ),
        },
    );
    ui.add(
        "residual after close",
        &format!("{residual:+.0} MiB vs. pre-open baseline"),
    );
    ui.add(
        "post-close drift",
        &format!("{post_close_drift:+.0} MiB between two post-close samples"),
    );
    ui.add(
        "what is counted",
        "anon + shmem. dmabuf/GPU buffers live in the driver, not in the session cgroup",
    );

    // ------------------------------------------------------------- peaks ---
    let (peak_mem, peak_cpu) = sampler.peaks();
    let (cpu_series, cpu_marks) = sampler.series();
    sampler.finish();
    // Same vector the timeline draws, so the radar's vertex and the strip below
    // it cannot disagree.
    let cpu_avg = mean_cpu(&cpu_series);
    let ram_peak = peak_mem.max(i1.working_set_mib());

    ui.section("during the run");
    ui.add("RAM before test", &sysinfo::fmt_mib(i1.working_set_mib()));
    ui.add(
        "RAM peak during test",
        &format!(
            "{}  (+{:.0} MiB)",
            sysinfo::fmt_mib(ram_peak),
            ram_peak - i1.working_set_mib()
        ),
    );
    ui.add(
        "CPU idle before test",
        &format!("{cpu_pct:.2} % of one core"),
    );
    ui.add(
        "CPU average during test",
        &format!("{cpu_avg:.1} % of one core"),
    );
    ui.add(
        "CPU peak during test",
        &format!("{peak_cpu:.1} % of one core"),
    );
    ui.add("sampling", &format!("every {PEAK_PERIOD_MS} ms"));

    // --------------------------------------------------------- confidence --
    if let Some(r) = &single {
        checks.push(report::Check {
            name: "presentation feedback".into(),
            ok: r.pacing_source == "presentation",
            detail: r.pacing_source.to_string(),
        });
        checks.push(report::Check {
            name: "GPU counters".into(),
            ok: r.gpu_ms_per_frame.is_some() || r.app_gpu_ms_per_frame.is_some(),
            detail: if r.gpu_ms_per_frame.is_some() || r.app_gpu_ms_per_frame.is_some() {
                "DRM fdinfo".into()
            } else {
                "not exposed by this driver".into()
            },
        });
    }
    let confidence = confidence(&checks);
    ui.add("confidence", confidence);

    // ------------------------------------------------------------ summary ---
    let scan = ui.scan.borrow();
    let scan_ok = scan.as_ref().is_some_and(|s| s.connected);
    let globals = scan.as_ref().map(|s| s.globals.clone()).unwrap_or_default();
    let feats = wlscan::evaluate(&globals);
    drop(scan);
    // A failed scan writes null, not 0 %: "not measured" and "advertised
    // nothing" are different claims.
    let features = scan_ok.then(|| report::Features::from_scan(&feats));

    // Delivered frames are counted against the cadence the compositor actually
    // settled into, so a clean 60 fps on a 120 Hz panel is 100 %, not 50 %.
    let pacing_share = |r: &Option<report::RenderPhase>| {
        r.as_ref()
            .filter(|p| p.frames > 0)
            .map(|p| ((p.frames - p.dropped) as f64 / p.frames as f64).clamp(0.0, 1.0))
            .unwrap_or(0.0)
    };
    let pacing_single = pacing_share(&single);
    let pacing_multi = pacing_share(&multi);
    // The multi-surface test is the load case; fall back to the single one
    // when it produced too few frames to pace.
    let load = multi.as_ref().or(single.as_ref());

    (ui.summary)(Summary {
        pacing_single,
        pacing_multi,
        ram_mib: i1.working_set_mib(),
        ram_peak_mib: ram_peak,
        cpu_pct,
        comp_cpu_pct: comp_pct,
        cpu_peak_pct: peak_cpu,
        other_cpu_pct,
        comp_mib,
        comp_ms_per_frame: single.as_ref().map(|r| r.comp_ms_per_frame).unwrap_or(0.0),
        comp_ms_per_frame_multi: load.map(|r| r.comp_ms_per_frame).unwrap_or(0.0),
        cadence_hz_multi: load.map(|r| r.cadence_hz).unwrap_or(0.0),
        gpu_ms_per_frame: single.as_ref().and_then(|r| r.gpu_ms_per_frame),
        feat_pct: feats.percent(),
        feat_supported: feats.supported,
        feat_total: feats.total,
        feat_supported_all: feats.supported_all,
        feat_total_all: feats.total_all,
        feat_extra: feats.extra,
        feat_items: feats.items.clone(),
        confidence,
        cpu_series: cpu_series.clone(),
        cpu_marks: cpu_marks.clone(),
    });

    // --------------------------------------------------------------- json ---
    if let Some(target) = &opts.json {
        let run = report::Run {
            schema_version: report::SCHEMA_VERSION,
            label: opts.label(&desktop.name),
            compositor: comp_name,
            compositor_version: comp
                .as_ref()
                .and_then(|c| proc::compositor_version(&c.name)),
            compositor_pid: pid.unwrap_or(0),
            compositor_source: comp
                .as_ref()
                .map(|c| c.source.to_string())
                .unwrap_or_default(),
            when: report::now_iso(),
            desktop: desktop.name.clone(),
            desktop_env: desktop.env.clone(),
            cgroup: proc::session_cgroup().display().to_string(),
            cgroup_exact: proc::cgroup_exact(),
            blur: if ui.blur_off.get() { "off" } else { "on" }.into(),
            confidence: confidence.to_string(),
            checks,
            clean: contaminants.is_empty(),
            contaminants: contaminants
                .iter()
                .map(|(name, pss_mib)| report::Contaminant {
                    name: name.clone(),
                    pss_mib: (pss_mib * 10.0).round() / 10.0,
                })
                .collect(),
            env,
            features,
            idle: Some(report::IdlePhase {
                dur_s: opts.idle_s,
                measured_s: Some(idle_s),
                settle_s,
                mem_mib: i1.working_set_mib(),
                total_mib: i1.total_mib,
                peak_mib: idle_peak_mib,
                peak_method: format!("polled every {PEAK_PERIOD_MS} ms"),
                anon_mib: i1.anon_mib,
                shmem_mib: i1.shmem_mib,
                file_mib: i1.file_mib,
                kernel_mib: i1.kernel_mib,
                swap_mib: i1.swap_mib,
                procs: i1.procs,
                cpu_pct,
                comp_cpu_pct: comp_pct,
                other_cpu_pct: Some(other_cpu_pct),
                wakeups_per_s: wakeups,
                comp_pss_mib: comp_mib,
                psys_w,
                pkg_w,
                batt_w,
                batt_status,
                threads: idle_threads,
            }),
            windows: Some(report::WindowsPhase {
                extra: n,
                settle_s: WINDOW_SETTLE_MS as f64 / 1000.0,
                method: "same-process GTK toplevels".into(),
                baseline_mib: base.working_set_mib(),
                baseline_drift_mib: Some(baseline_drift),
                with_windows_mib: with_wins.working_set_mib(),
                delta_mib: delta,
                delta_valid,
                client_gpu_mib: client_gpu,
                comp_gpu_mib: comp_gpu,
                per_window_mib: per_window,
                comp_delta_mib: comp_delta,
                client_delta_mib: client_delta,
                residual_mib: residual,
                post_close_drift_mib: Some(post_close_drift),
            }),
            render: single,
            multi,
            peaks: Some(report::PeakPhase {
                dur_s: run_t0.elapsed().as_secs_f64(),
                ram_before_mib: i1.working_set_mib(),
                ram_peak_mib: ram_peak,
                ram_peak_method: format!("polled every {PEAK_PERIOD_MS} ms"),
                cpu_idle_pct: cpu_pct,
                cpu_avg_pct: Some(cpu_avg),
                cpu_peak_pct: peak_cpu,
                cpu_series: Some(
                    cpu_series
                        .iter()
                        .map(|(t_s, pct)| report::CpuPoint {
                            t_s: *t_s,
                            pct: *pct,
                        })
                        .collect(),
                ),
                cpu_marks: Some(
                    cpu_marks
                        .iter()
                        .map(|(t_s, phase)| report::CpuMark {
                            t_s: *t_s,
                            phase: (*phase).to_string(),
                        })
                        .collect(),
                ),
            }),
        };
        let target_name = match target {
            crate::cli::JsonTarget::Path(p) => p.display().to_string(),
            crate::cli::JsonTarget::Auto => report::results_dir().display().to_string(),
        };
        match run.write(target) {
            Ok(path) => {
                if std::env::var("WG_BENCH_PRINT").is_ok() {
                    println!("{}", run.summary_line());
                }
                ui.add("written to", &path.display().to_string());
            }
            Err(e) => {
                let msg = format!("cannot write {target_name}: {e}");
                eprintln!("wayland-compositor-bench: {msg}");
                ui.add("write failed", &msg);
                ui.failed.set(true);
            }
        }
    }

    // --------------------------------------------------------------- done ---
    ui.busy.set(false);
    ui.progress.set_fraction(1.0);
    ui.status.set_visible(false);
    ui.surface.set_visible(false);
    ui.start.set_label("Restart");
    ui.start.set_sensitive(true);
    // A list that filled up while it was folded should open at its first row,
    // not wherever the adjustment clamped on the way.
    if let Some(sw) = ui
        .results_expander
        .child()
        .and_downcast::<gtk::ScrolledWindow>()
    {
        sw.vadjustment().set_value(0.0);
    }
    let pb = ui.progress.clone();
    glib::timeout_add_local_once(Duration::from_millis(900), move || pb.set_visible(false));

    // `--run` is a measurement invocation, not an interactive one: exit so it
    // can sit in a script. Pressing Start in the window keeps it open.
    if ui.opts.run {
        ui.app.quit();
    }
}

/// One frame-timing scenario. `extras` are already-mapped windows whose areas
/// are repainted by the same tick callback, so the compositor sees N+1
/// surfaces with fresh damage every frame.
async fn frame_test(
    ui: &BenchUi,
    scenario: &str,
    dur_s: f64,
    extras: &[(gtk::DrawingArea, Rc<Cell<f64>>)],
    pid: Option<u32>,
) -> Option<report::RenderPhase> {
    ui.set_status(&format!(
        "frame timing ({scenario}): {} surface(s) for {dur_s:.0} s…",
        extras.len() + 1
    ));
    let stats = Rc::new(RefCell::new(FrameStats::default()));
    let s = stats.clone();
    let t_start = ui
        .area
        .frame_clock()
        .map(|c| c.frame_time())
        .unwrap_or(i64::MAX);
    let ex: Vec<(gtk::DrawingArea, Rc<Cell<f64>>)> = extras.to_vec();
    let tick = ui.area.add_tick_callback(move |w, clock| {
        collect(clock, &mut s.borrow_mut(), t_start);
        for (area, phase) in &ex {
            phase.set(phase.get() + 0.03);
            area.queue_draw();
        }
        w.queue_draw();
        glib::ControlFlow::Continue
    });

    // The probes bracket the CPU window; the power samples bracket only the
    // sleep, so a slow `/proc` walk is not charged to the frame test and the
    // RAPL reading itself is not mistaken for the test's cost.
    let before = Probe::before(pid);
    let pw0 = power::sample();
    glib::timeout_future(Duration::from_secs_f64(dur_s)).await;
    let pw1 = power::sample();
    let after = Probe::after(pid);
    let batt_w = power::battery_watts();
    let wall = after.at.duration_since(before.at).as_secs_f64();
    tick.remove();

    let vals: Vec<(i64, i64, i64)> = stats.borrow().frames.values().cloned().collect();
    let Some(p) = pacing(&vals, ui.refresh_hz) else {
        ui.section(&format!("frame timing · {scenario}"));
        ui.add("result", "not enough frames collected");
        return None;
    };

    let frames = p.frames.max(1) as f64;
    let comp_ns = after.comp_ns.saturating_sub(before.comp_ns);
    let comp_ms = comp_ns as f64 / 1e9 * 1000.0 / frames;
    let app_ms = after.app_ns.saturating_sub(before.app_ns) as f64 / 1e9 * 1000.0 / frames;
    let comp_gpu =
        proc::gpu_cost(&before.gpu, &after.gpu, wall).map(|c| c.busy_s * 1000.0 / frames);
    let app_gpu =
        proc::gpu_cost(&before.app_gpu, &after.app_gpu, wall).map(|c| c.busy_s * 1000.0 / frames);
    // The compositor's counters and this client's counters are separate DRM
    // clients, so they add rather than replace each other. A missing side is not
    // zero, but the other side is still a floor. `gpu_ms_per_frame` stays
    // compositor-only for v3 compatibility; the client component is separate.
    let session_cpu = after.session_usec.saturating_sub(before.session_usec) as f64 / 1e6;
    let self_cpu = after.app_ns.saturating_sub(before.app_ns) as f64 / 1e9;
    let session_cpu_excl_self = (session_cpu - self_cpu).max(0.0);
    let (psys_w, pkg_w) = power::delta(pw0, pw1);
    let threads = thread_shares(&before.tasks, &after.tasks, comp_ns);

    ui.section(&format!("frame timing · {scenario}"));
    ui.add(
        "surfaces / frames",
        &format!(
            "{} / {}  ({wall:.1} s measured)",
            extras.len() + 1,
            p.frames
        ),
    );
    ui.add(
        "target / achieved",
        &format!(
            "{:.0} Hz panel · {:.1} fps ({:.0} Hz cadence)",
            p.target_hz, p.fps, p.cadence_hz
        ),
    );
    ui.add(
        "interval p50 / p95 / max",
        &format!("{:.2} / {:.2} / {:.2} ms", p.p50, p.p95, p.max),
    );
    ui.add(
        "dropped frames",
        &format!("{} (vs the observed cadence)", p.dropped),
    );
    ui.add("compositor CPU per frame", &format!("{comp_ms:.3} ms"));
    ui.add(
        "GPU time per frame",
        &match (comp_gpu, app_gpu) {
            (Some(c), Some(a)) => format!("{:.3} ms ({c:.3} comp + {a:.3} client)", c + a),
            (Some(c), None) => format!("{c:.3} ms (compositor only)"),
            (None, Some(a)) => format!("{a:.3} ms (client only)"),
            (None, None) => "no DRM counters".into(),
        },
    );
    ui.add("app CPU per frame", &format!("{app_ms:.3} ms"));
    ui.add(
        "session CPU during test",
        &format!(
            "{session_cpu:.2} s over {wall:.1} s ({session_cpu_excl_self:.2} s excluding this tool)"
        ),
    );
    ui.add(
        "power during test",
        &match (psys_w, pkg_w) {
            (Some(ps), Some(pk)) => format!("{ps:.1} W psys · {pk:.1} W pkg"),
            (Some(ps), None) => format!("{ps:.1} W psys"),
            _ => "not readable (root-only, see README)".to_string(),
        },
    );
    if let Some(w) = batt_w {
        ui.add("  battery draw", &format!("{w:.1} W"));
    }
    ui.add("pacing source", p.source);
    if let Some(top) = threads.first() {
        ui.add(
            "busiest compositor thread",
            &format!("{} ({:.0}%)", top.name, top.share_pct),
        );
    }

    Some(report::RenderPhase {
        scenario: scenario.to_string(),
        dur_s,
        measured_s: Some(wall),
        frames: p.frames as i64,
        dropped: p.dropped,
        fps: p.fps,
        target_hz: p.target_hz,
        cadence_hz: p.cadence_hz,
        p50_ms: p.p50,
        p95_ms: p.p95,
        max_ms: p.max,
        pacing_source: p.source.to_string(),
        comp_ms_per_frame: comp_ms,
        comp_ns_per_frame: comp_ns as f64 / frames,
        gpu_ms_per_frame: comp_gpu,
        comp_gpu_ms_per_frame: comp_gpu,
        app_gpu_ms_per_frame: app_gpu,
        app_ms_per_frame: app_ms,
        session_cpu_s: session_cpu,
        self_cpu_s: Some(self_cpu),
        session_cpu_excl_self_s: Some(session_cpu_excl_self),
        psys_w,
        pkg_w,
        batt_w,
        threads,
    })
}

/// No single score: a scalar over arbitrary weights invites quoting a number
/// that hides its own trade-offs. A run is high confidence only when every
/// input the report depends on was actually available.
fn confidence(checks: &[report::Check]) -> &'static str {
    let bad = |name: &str| {
        checks
            .iter()
            .find(|c| c.name == name)
            .map(|c| !c.ok)
            .unwrap_or(true)
    };
    if bad("session cgroup") || bad("compositor process") {
        "low"
    } else if bad("presentation feedback")
        || bad("display refresh")
        || bad("clean session")
        || bad("quiet cpu")
    {
        "medium"
    } else {
        "high"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_average_weights_each_sample_by_its_span() {
        // 10 % of a core for the first half of the run, 100 % for the second
        let pts: Vec<CpuPoint> = vec![(0.0, 10.0), (0.5, 100.0), (1.0, 100.0)];
        // trapezoid: (10+100)/2 over 0.5 s + (100+100)/2 over 0.5 s
        assert!((mean_cpu(&pts) - 77.5).abs() < 1e-6);
        // a sample that never got an interval of its own is not invented
        assert_eq!(mean_cpu(&[]), 0.0);
        assert!((mean_cpu(&[(0.0, 42.0)]) - 42.0).abs() < 1e-6);
    }

    #[test]
    fn a_process_that_appeared_mid_window_is_not_charged_its_lifetime() {
        let before = proc::CpuSnapshot {
            total_usec: 0,
            procs: vec![(1, "old".into(), 0)],
        };
        let after = proc::CpuSnapshot {
            total_usec: 1_000_000,
            procs: vec![
                (1, "old".into(), 0),
                // 10 s of lifetime CPU, none of it known to be in this window
                (2, "new".into(), 10_000_000_000),
            ],
        };
        let (other, top) = cpu_attribution(&before, &after, 1.0, &[]);
        assert!(top.is_empty(), "a new process has no in-window baseline");
        assert!((other - 100.0).abs() < 1e-9);
    }

    #[test]
    fn attribution_uses_the_same_window_for_the_total_and_the_offenders() {
        let before = proc::CpuSnapshot {
            total_usec: 0,
            procs: vec![(1, "old".into(), 0), (2, "busy".into(), 0)],
        };
        let after = proc::CpuSnapshot {
            total_usec: 2_000_000,
            procs: vec![(1, "old".into(), 0), (2, "busy".into(), 2_000_000_000)],
        };
        let (other, top) = cpu_attribution(&before, &after, 2.0, &[]);
        assert!((other - 100.0).abs() < 1e-9);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].0, "busy");
        assert!((top[0].1 - 100.0).abs() < 1e-9);
    }
}
