mod access;
mod bench;
mod cli;
mod cpuchart;
mod power;
mod proc;
mod radar;
mod ramchart;
mod report;
mod sysinfo;
mod wlscan;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;

const CSS: &str = r#"
window.wbg, window.wbg.background { background-color: transparent; }
.card {
    background-color: rgba(14,16,24,0.55);
    backdrop-filter: blur(28px);
    border-radius: 18px;
    border: 1px solid rgba(255,255,255,0.09);
    padding: 18px;
}
.title { font-size: 15px; font-weight: 700; color: #ffffff; }
.subtitle { font-size: 11px; color: rgba(255,255,255,0.45); }
.key { font-size: 13px; color: rgba(255,255,255,0.50); }
.value { font-size: 13.5px; color: rgba(255,255,255,0.95); }
.section {
    font-size: 11.5px; font-weight: 700; letter-spacing: 0.09em;
    color: rgba(255,255,255,0.42); margin-top: 10px; margin-bottom: 2px;
}
.group {
    font-size: 10px; font-weight: 700; letter-spacing: 0.11em;
    color: rgba(255,255,255,0.30); margin-top: 8px; margin-bottom: 1px;
}
.chart {
    background-color: rgba(255,255,255,0.045);
    border-radius: 12px;
    border: 1px solid rgba(255,255,255,0.07);
    padding: 2px;
}
.summary {
    background-color: rgba(255,255,255,0.042);
    border-radius: 14px;
    border: 1px solid rgba(255,255,255,0.08);
    padding: 12px 14px;
}
.panel {
    background-color: rgba(255,255,255,0.042);
    border-radius: 14px;
    border: 1px solid rgba(255,255,255,0.08);
    padding: 12px 14px;
}
.panel .section { margin-top: 0; margin-bottom: 4px; }
.ram-item {
    font-size: 11px; color: rgba(255,255,255,0.86);
    background-color: rgba(255,255,255,0.05);
    border-radius: 6px; padding: 1px 7px;
}
.ram-item.on {
    background-color: rgba(255,255,255,0.17);
    box-shadow: inset 0 0 0 1px rgba(255,255,255,0.38);
}
.stat-title { font-size: 11px; font-weight: 700; letter-spacing: 0.10em; color: rgba(255,255,255,0.45); }
.stat-big { font-size: 31px; font-weight: 800; color: #ffffff; }
.stat-sub { font-size: 12px; color: rgba(255,255,255,0.45); }
.access-check {
    font-size: 11.5px;
    border-radius: 8px; padding: 8px 10px;
}
.access-ok {
    color: #86efac; background-color: rgba(74,222,128,0.08);
}
.access-warn {
    color: #fcd34d; background-color: rgba(251,191,36,0.08);
}
.chip {
    background-color: rgba(255,255,255,0.09);
    border-radius: 999px; padding: 3px 10px;
    font-size: 11px; color: rgba(255,255,255,0.75);
}
.chip.ok   { background-color: rgba(74,222,128,0.16); color: #86efac; }
.chip.warn { background-color: rgba(251,191,36,0.16); color: #fcd34d; }
button.chip {
    background-image: none; border: none; box-shadow: none;
    min-height: 0; min-width: 0;
}
button.chip:hover { background-color: rgba(255,255,255,0.18); }
button.chip.ok:hover { background-color: rgba(74,222,128,0.26); }
button.chip.warn:hover { background-color: rgba(251,191,36,0.26); }
/* Blur off: the window paints its own opaque background, so nothing of the
   desktop shows through and the compositor has nothing to blur. */
window.wbg.solid, window.wbg.solid.background {
    background-color: #12141c; border-radius: 18px;
}
window.wbg.solid .card {
    background-color: #12141c; backdrop-filter: none;
}
button.start {
    background-image: none; background-color: #4f8cff; color: #ffffff;
    border-radius: 10px; padding: 7px 20px; font-weight: 600;
    box-shadow: none; border: none;
}
button.start:hover { background-color: #6a9dff; }
button.start:disabled { background-color: rgba(255,255,255,0.10); color: rgba(255,255,255,0.4); }
button.ghost {
    background-image: none; background-color: rgba(255,255,255,0.08);
    color: rgba(255,255,255,0.85); border-radius: 9px; padding: 8px 12px;
    box-shadow: none; border: none; font-size: 16px; min-width: 22px; min-height: 22px;
}
button.ghost:hover { background-color: rgba(255,255,255,0.16); }
.tick { color: #86efac; font-weight: 700; font-size: 13px; }
.cross { color: #f87171; font-weight: 700; font-size: 13px; }
.proto { font-size: 12.5px; color: rgba(255,255,255,0.82); }
.version { font-size: 13px; font-weight: 600; color: rgba(255,255,255,0.42); }
progressbar trough { background-color: rgba(255,255,255,0.10); border-radius: 99px; min-height: 6px; }
progressbar progress { background-color: #4f8cff; border-radius: 99px; min-height: 6px; }
separator { background-color: rgba(255,255,255,0.08); min-height: 1px; }
"#;

type Labels = HashMap<&'static str, gtk::Label>;

fn add_row(grid: &gtk::Grid, labels: &mut Labels, r: i32, key: &'static str, text: &str) {
    let k = gtk::Label::new(Some(key));
    k.set_xalign(0.0);
    // fixed key column: opening a collapsible must not resize the panel
    k.set_size_request(172, -1);
    k.add_css_class("key");
    let v = gtk::Label::new(Some(text));
    v.set_xalign(0.0);
    v.set_hexpand(true);
    // single line, ellipsized: a wrapped value makes the grid's height depend
    // on its width, which GTK reports as min > natural and draws as a layout
    // warning. Full text stays available as a tooltip.
    v.set_ellipsize(gtk::pango::EllipsizeMode::End);
    v.set_width_chars(20);
    v.set_max_width_chars(28);
    v.set_tooltip_text(Some(text));
    v.add_css_class("value");
    v.set_selectable(true);
    grid.attach(&k, 0, r, 1, 1);
    grid.attach(&v, 1, r, 1, 1);
    labels.insert(key, v);
}

fn add_section(grid: &gtk::Grid, r: i32, title: &str) {
    let l = gtk::Label::new(Some(title));
    l.set_xalign(0.0);
    l.add_css_class("section");
    grid.attach(&l, 0, r, 2, 1);
}

fn add_group(grid: &gtk::Grid, r: i32, title: &str) {
    let l = gtk::Label::new(Some(title));
    l.set_xalign(0.0);
    l.add_css_class("group");
    grid.attach(&l, 0, r, 2, 1);
}

/// Top-left column: the handful of facts that identify the session and the
/// machine it runs on, grouped so the eye can jump straight to what it needs.
fn build_basic_grid(labels: &mut Labels) -> gtk::Grid {
    let grid = gtk::Grid::builder()
        .row_spacing(6)
        .column_spacing(22)
        .build();
    let mut r = 0;

    add_group(&grid, r, "SESSION");
    r += 1;
    add_row(&grid, labels, r, "Wayland compositor", "…");
    r += 1;
    add_row(&grid, labels, r, "Wayland display", "…");
    r += 1;
    add_row(&grid, labels, r, "Resolution", "…");
    r += 1;
    add_row(&grid, labels, r, "Performance mode", "…");
    r += 1;

    add_group(&grid, r, "HARDWARE");
    r += 1;
    add_row(&grid, labels, r, "CPU", "…");
    r += 1;
    add_row(&grid, labels, r, "CPU cores", "…");
    r += 1;
    add_row(&grid, labels, r, "GPU", "…");
    r += 1;
    add_row(&grid, labels, r, "GPU driver", "…");
    r += 1;
    add_row(&grid, labels, r, "RAM", "…");
    r += 1;

    add_group(&grid, r, "SOFTWARE");
    r += 1;
    add_row(&grid, labels, r, "OS", "…");
    r += 1;
    add_row(&grid, labels, r, "Mesa", "…");
    r += 1;
    add_row(&grid, labels, r, "Kernel", "…");
    grid
}

/// Everything else: output geometry and live session counters.
fn build_other_grid(labels: &mut Labels) -> gtk::Grid {
    let grid = gtk::Grid::builder()
        .row_spacing(4)
        .column_spacing(22)
        .build();
    let mut r = 0;
    add_section(&grid, r, "OUTPUT");
    r += 1;
    add_row(&grid, labels, r, "Physical mode", "…");
    r += 1;
    add_row(&grid, labels, r, "Logical size", "…");
    r += 1;
    add_row(&grid, labels, r, "Scale", "…");
    r += 1;
    add_row(&grid, labels, r, "Refresh", "…");
    r += 1;
    add_section(&grid, r, "SESSION");
    r += 1;
    add_row(&grid, labels, r, "Compositor process", "…");
    r += 1;
    add_row(&grid, labels, r, "Session memory", "…");
    r += 1;
    add_row(&grid, labels, r, "Peak memory (since login)", "…");
    r += 1;
    add_row(&grid, labels, r, "Compositor memory (PSS)", "…");
    r += 1;
    add_row(&grid, labels, r, "GPU memory (compositor)", "…");
    r += 1;
    add_row(&grid, labels, r, "Session CPU", "…");
    r += 1;
    add_row(&grid, labels, r, "Pressure (cpu/mem)", "…");
    r += 1;
    add_row(&grid, labels, r, "Brightness", "…");
    r += 1;
    add_row(&grid, labels, r, "Package temp", "…");
    r += 1;
    add_row(&grid, labels, r, "Blur protocol", "…");
    grid
}

fn set(labels: &Labels, key: &str, text: &str) {
    if let Some(l) = labels.get(key) {
        l.set_text(text);
        l.set_tooltip_text(Some(text));
    }
}

/// What sits behind the Performance mode row: `set()` would overwrite the
/// tooltip with the value, so the extra detail is re-applied after each update.
fn performance_tip() -> String {
    let mut tip = String::from("ACPI platform profile");
    if let Some(g) = sysinfo::cpu_governor() {
        tip.push_str(&format!("\nCPU governor: {g}"));
    }
    let choices = sysinfo::performance_mode_choices();
    if !choices.is_empty() {
        tip.push_str(&format!("\navailable profiles: {choices}"));
    }
    tip
}

fn set_performance_tip(labels: &Labels, tip: &str) {
    if let Some(l) = labels.get("Performance mode") {
        l.set_tooltip_text(Some(tip));
    }
}

/// The header chip reports what the compositor advertises, and doubles as the
/// switch between the translucent, compositor-blurred background and a plain
/// opaque one.
fn set_blur_chip(chip: &gtk::Button, state: wlscan::BlurState, off: bool) {
    chip.remove_css_class("ok");
    chip.remove_css_class("warn");
    if off {
        chip.set_label("blur: off");
        chip.add_css_class("warn");
        chip.set_tooltip_text(Some(
            "Compositor blur and window transparency are off — click to use the blurred background",
        ));
        return;
    }
    chip.set_label(&format!("blur: {}", state.short()));
    chip.add_css_class(match state {
        wlscan::BlurState::Active => "ok",
        _ => "warn",
    });
    chip.set_tooltip_text(Some(match state {
        wlscan::BlurState::Active => {
            "Blur is active — click to disable it and use a normal window background"
        }
        _ => "No compositor blur — click to switch between transparent and solid background",
    }));
}

#[derive(Clone)]
struct SummaryUi {
    root: gtk::Box,
    radar: Rc<radar::Radar>,
    /// CPU over time, directly under the radar
    chart: Rc<cpuchart::Chart>,
    comp: gtk::Label,
    /// one-line protocol coverage, shown above the always-visible feature list
    feat_label: gtk::Label,
    feats: gtk::Grid,
    /// collapsible at the bottom of the radar card, holding the one-line
    /// summary and the detailed numbers of the last run
    results_expander: gtk::Expander,
    results: gtk::Grid,
}

impl SummaryUi {
    fn build() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 2);
        root.add_css_class("summary");
        root.set_visible(false);

        let radar = Rc::new(radar::Radar::new(radar_limits()));
        root.append(&radar.area);

        let version = gtk::Label::new(Some(concat!(
            "Wayland Compositor Bench v",
            env!("CARGO_PKG_VERSION")
        )));
        version.set_xalign(0.5);
        version.add_css_class("version");
        root.append(&version);

        // the same CPU samples as the radar, but across the run instead of at a
        // single peak: a low strip, no axes furniture beyond the three ticks
        let chart = Rc::new(cpuchart::Chart::new());
        root.append(&chart.area);

        let hint = gtk::Label::new(Some(
            "further out is better · pacing vs the observed cadence · compositor CPU per frame vs the frame budget · compositor RAM and CPU vs what the system has (log axes)",
        ));
        hint.set_xalign(0.0);
        hint.set_wrap(true);
        hint.set_justify(gtk::Justification::Left);
        hint.add_css_class("stat-sub");

        let comp = gtk::Label::new(None);
        comp.set_xalign(0.0);
        comp.set_wrap(true);
        comp.add_css_class("stat-sub");

        // the detailed rows, filled in by the benchmark once a run finishes
        let results = gtk::Grid::builder()
            .row_spacing(4)
            .column_spacing(22)
            .margin_top(4)
            .build();

        let details = gtk::Box::new(gtk::Orientation::Vertical, 4);
        details.append(&hint);
        details.append(&comp);
        details.append(&results);

        // The collapsible grows with its content but stops at a sane height and
        // scrolls inside, so an expanded run never squeezes the rest of the card.
        let details_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(200)
            .build();
        details_scroll.set_child(Some(&details));

        let results_label = gtk::Label::new(Some("Benchmark results"));
        results_label.add_css_class("value");
        let results_expander = gtk::Expander::new(None);
        results_expander.set_label_widget(Some(&results_label));
        results_expander.set_child(Some(&details_scroll));
        results_expander.set_expanded(false);
        // a little air between the chart/version line and the collapsible head
        results_expander.set_margin_top(12);
        root.append(&results_expander);

        let feats = gtk::Grid::builder()
            .row_spacing(2)
            .column_spacing(12)
            .margin_top(6)
            .build();
        feats.set_visible(false);
        let feat_label = gtk::Label::new(Some("Wayland protocols"));
        feat_label.set_xalign(0.0);
        feat_label.add_css_class("value");

        Self {
            root,
            radar,
            chart,
            comp,
            feat_label,
            feats,
            results_expander,
            results,
        }
    }

    fn show(&self, s: &bench::Summary) {
        self.radar.set(&radar::Values {
            pacing_single: s.pacing_single,
            pacing_multi: s.pacing_multi,
            comp_ms_per_frame: s.comp_ms_per_frame_multi,
            cadence_hz: s.cadence_hz_multi,
            comp_pss_mib: s.comp_mib,
            comp_cpu_pct: s.comp_cpu_pct,
            comp_known: s.comp_mib > 0.0,
            feat_pct: s.feat_pct,
        });
        self.chart.set(&s.cpu_series, &s.cpu_marks);
        let gpu = s
            .gpu_ms_per_frame
            .map(|g| format!(" · {g:.3} ms GPU"))
            .unwrap_or_default();
        self.comp.set_text(&format!(
            "session RAM {} → {} peak · session CPU {} → {} peak · {:.1}% other CPU · compositor {:.0} MiB PSS · {:.3} ms CPU/frame{} · confidence {}",
            sysinfo::fmt_mib(s.ram_mib),
            sysinfo::fmt_mib(s.ram_peak_mib),
            sysinfo::fmt_pct(s.cpu_pct),
            sysinfo::fmt_pct(s.cpu_peak_pct),
            s.other_cpu_pct,
            s.comp_mib,
            s.comp_ms_per_frame,
            gpu,
            s.confidence
        ));

        self.set_features(
            &s.feat_items,
            s.feat_supported,
            s.feat_total,
            s.feat_supported_all,
            s.feat_total_all,
            s.feat_extra,
        );
        self.root.set_visible(true);
    }

    /// Populate the always-visible protocol list, grouped by tier so the
    /// wlroots-only and optional protocols are visibly not part of the score.
    /// Called at startup (from the idle scan) and again after every run.
    fn set_features(
        &self,
        items: &[wlscan::Item],
        supported: usize,
        total: usize,
        supported_all: usize,
        total_all: usize,
        extra: usize,
    ) {
        while let Some(child) = self.feats.first_child() {
            self.feats.remove(&child);
        }
        let mut row = 0;
        let mut tier: Option<wlscan::Tier> = None;
        for item in items {
            if Some(item.feature.tier) != tier {
                tier = Some(item.feature.tier);
                let h = gtk::Label::new(Some(item.feature.tier.title()));
                h.set_xalign(0.0);
                h.add_css_class("group");
                self.feats.attach(&h, 0, row, 3, 1);
                row += 1;
            }
            let ok = item.supported();
            let mark = gtk::Label::new(Some(if ok { "✓" } else { "✗" }));
            mark.add_css_class(if ok { "tick" } else { "cross" });
            let name = match item.version {
                Some(v) => format!("{} v{v}", item.feature.name),
                None => item.feature.name.to_string(),
            };
            let proto = gtk::Label::new(Some(&name));
            proto.set_xalign(0.0);
            proto.set_selectable(true);
            proto.add_css_class("proto");
            let desc = gtk::Label::new(Some(item.feature.label));
            desc.set_xalign(0.0);
            desc.add_css_class("stat-sub");
            if item.stale() {
                desc.set_text(&format!(
                    "{} · older than v{}",
                    item.feature.label, item.feature.since
                ));
            }
            self.feats.attach(&mark, 0, row, 1, 1);
            self.feats.attach(&proto, 1, row, 1, 1);
            self.feats.attach(&desc, 2, row, 1, 1);
            row += 1;
        }
        let mut text = format!(
            "Wayland protocols — {supported} of {total} expected · {supported_all} of {total_all} listed"
        );
        if extra > 0 {
            text.push_str(&format!(" · {extra} unknown"));
        }
        self.feat_label.set_text(&text);
        self.feats.set_visible(true);
    }
}

/// `--check`: can this session be measured, and what would the report be
/// missing? Every line is something the JSON records, so a bad run is visible
/// before spending two minutes on it.
fn check(opts: &cli::Options) -> glib::ExitCode {
    let comp = proc::compositor();
    let hint = sysinfo::DisplayHint {
        compositor_hz: None,
        ..Default::default()
    };
    let env = sysinfo::Env::collect(hint, comp.as_ref());
    let mut warn: Vec<String> = Vec::new();

    println!("wayland-compositor-bench check\n");

    let desktop = proc::desktop(comp.as_ref());
    println!("session");
    println!("  desktop          {}", pretty_desktop(&desktop.name));
    if desktop.mismatch() {
        println!(
            "  environment      XDG_CURRENT_DESKTOP={} (stale, not this session)",
            desktop.env
        );
    }
    println!(
        "  wayland display  {}",
        std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "unset".into())
    );
    let cg = proc::session_cgroup();
    println!(
        "  cgroup           {} ({})",
        cg.display(),
        proc::cgroup_label()
    );
    let cg_readable = ["memory.current", "memory.stat", "cpu.stat", "cgroup.procs"]
        .iter()
        .all(|f| proc::read(&cg.join(f).to_string_lossy()).is_some());
    if !cg_readable {
        warn.push(format!(
            "no readable cgroup v2 counters at {}: session RAM/CPU are unavailable (cgroup v1 or \
             no session scope)",
            cg.display()
        ));
    } else if !proc::cgroup_exact() {
        warn.push(
            "no session scope: the user slice also contains other sessions, so RAM and CPU are \
             not this session's"
                .into(),
        );
    }
    match &comp {
        Some(c) => {
            // The same string as the window's `Wayland compositor` row, so a
            // terminal check and a screenshot cannot disagree.
            let version = proc::compositor_version(&c.name);
            let line = compositor_identity(
                proc::compositor_impl(&c.name),
                version.as_deref(),
                &pretty_desktop(&desktop.name),
            );
            println!(
                "  compositor       {line} ({} pid {}, via {})",
                c.name, c.pid, c.source
            );
            if c.source != "socket" {
                warn.push(
                    "compositor was found by name, not by its WAYLAND_DISPLAY socket: a nested \
                     compositor may be the one being measured"
                        .into(),
                );
            }
            if let Some(hash) = proc::config_hash(&c.name) {
                println!("  config hash      {hash}");
            }
            if proc::pss_kb(c.pid) == 0 {
                warn.push(format!(
                    "/proc/{}/smaps_rollup is unreadable: compositor PSS will be 0 (hidepid or \
                     permissions)",
                    c.pid
                ));
            }
            let tasks = proc::tasks(c.pid);
            if tasks.is_empty() || tasks.iter().all(|t| t.ns == 0) {
                warn.push(
                    "schedstat is missing or zero: compositor CPU will be coarse or 0 (enable \
                     CONFIG_SCHEDSTATS)"
                        .into(),
                );
            }
        }
        None => {
            println!("  compositor       not found");
            warn.push("no compositor process: compositor CPU and memory cannot be measured".into());
        }
    }

    if desktop.mismatch() {
        warn.push(format!(
            "XDG_CURRENT_DESKTOP says {} but the Wayland socket belongs to {}: the environment \
             is left over from another session, so the run is named '{}'",
            desktop.env,
            comp.as_ref()
                .map(|c| c.name.as_str())
                .unwrap_or("another process"),
            crate::cli::slug(&desktop.name),
        ));
    }

    println!("\ndisplay");
    let d = &env.display;
    println!(
        "  connector        {} ({})",
        if d.connector.is_empty() {
            "?"
        } else {
            &d.connector
        },
        d.outputs
    );
    println!(
        "  physical mode    {}",
        if d.mode.is_empty() { "?" } else { &d.mode }
    );
    match d.refresh_hz {
        Some(hz) => println!("  refresh (EDID)   {hz:.2} Hz"),
        None => println!(
            "  refresh (EDID)   no detailed timing (common on laptop panels) — the run will use \
             the interval the compositor reports"
        ),
    }
    match sysinfo::backlight() {
        Some(b) => println!("  brightness       {} / {} ({})", b.value, b.max, b.name),
        None => println!("  brightness       no backlight"),
    }

    println!("\npower");
    let (psys, pkg) = power::readable();
    println!(
        "  RAPL             psys {} · package {}",
        if psys { "yes" } else { "no" },
        if pkg { "yes" } else { "no" }
    );
    if !psys && !pkg {
        warn.push(
            "RAPL is root-only (0400). Add the udev rule from the README to read whole-machine \
             power without sudo"
                .into(),
        );
    }
    match power::battery() {
        Some((_, status)) => {
            println!("  battery          {status}");
            if status != "Discharging" {
                warn.push(format!(
                    "battery is {status}: power figures will include charging, unplug to compare"
                ));
            }
        }
        None => println!("  battery          none"),
    }

    println!("\nsystem");
    println!("  cpu              {}", env.cpu);
    println!("  cores / threads  {} / {}", env.cpu_cores, env.cpu_threads);
    println!(
        "  gpu              {} · {} · {}",
        env.gpu, env.gpu_driver, env.gpu_pci
    );
    println!("  mesa             {}", env.mesa);
    println!("  kernel           {}", env.kernel);
    println!(
        "  profile          {} (governor {}, epp {})",
        env.profile,
        if env.governor.is_empty() {
            "?"
        } else {
            &env.governor
        },
        if env.epp.is_empty() { "?" } else { &env.epp }
    );
    if let Some(t) = env.temp_c {
        println!("  package temp     {t:.0} °C");
    }
    if let Some(g) = env.cpu_ghz {
        println!("  cpu clock        {g:.2} GHz average");
    }
    if env.throttled {
        warn.push("RAPL throttle counters are non-zero: the CPU is thermally limited".into());
    }
    if sysinfo::psi(cg, "cpu.pressure").is_some() && sysinfo::psi(cg, "memory.pressure").is_some() {
        println!("  pressure         cpu + memory PSI");
    } else {
        println!("  pressure         unavailable");
        warn.push(
            "no cpu.pressure / memory.pressure: pressure stalls are unavailable (enable CONFIG_PSI \
             and use a cgroup v2 mount)"
                .into(),
        );
    }

    println!("\nwhat a run would do");
    println!(
        "  idle {} s after up to {} s of settling, then two {} s frame tests",
        opts.idle_s, opts.settle_s, opts.frames_s
    );
    println!(
        "  opens {} extra toplevels for the memory and multi-surface phases",
        opts.windows
    );
    println!("  protocol scan: runs once the window is up, on its own Wayland connection");

    // Heavy neighbours are the most common reason two runs of the same
    // compositor disagree, so they surface before spending two minutes.
    let mut mine = vec![std::process::id()];
    if let Some(c) = &comp {
        mine.push(c.pid);
        let (_, tree) = proc::pss_tree(c.pid);
        mine.extend(tree);
    }
    let mut heavy: Vec<(String, f64)> = Vec::new();
    for (name, mib) in sysinfo::contaminants(&mine) {
        match heavy.iter_mut().find(|(n, _)| *n == name) {
            Some(e) => e.1 += mib,
            None => heavy.push((name, mib)),
        }
    }
    if !heavy.is_empty() {
        println!("\nheavy processes in this session:");
        for (name, mib) in &heavy {
            println!("  {name:<24} {mib:.0} MiB");
            warn.push(format!(
                "{name} holds {mib:.0} MiB: close it, or the run is marked as a busy session"
            ));
        }
    }

    if warn.is_empty() {
        println!("\nverdict: measurable — expect high confidence\n");
        glib::ExitCode::SUCCESS
    } else {
        // Exit 2, not 1: "measurable with caveats" is a usable session, but a
        // script that needs a clean one can still gate on the status.
        println!("\nverdict: measurable with caveats (exit 2)");
        for w in &warn {
            println!("  ! {w}");
        }
        println!();
        glib::ExitCode::from(2u8)
    }
}

fn main() -> glib::ExitCode {
    // Like any Unix CLI: exit quietly when the reader of `--report | head`
    // goes away instead of panicking on a broken pipe.
    #[cfg(unix)]
    unsafe {
        extern "C" {
            fn signal(sig: i32, handler: usize) -> usize;
        }
        signal(13 /* SIGPIPE */, 0 /* SIG_DFL */);
    }

    let opts = match cli::Options::parse(std::env::args().skip(1)) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("wayland-compositor-bench: {e}\n\n{}", cli::USAGE);
            return glib::ExitCode::FAILURE;
        }
    };
    if opts.help {
        print!("{}", cli::USAGE);
        return glib::ExitCode::SUCCESS;
    }
    if opts.version {
        println!("wayland-compositor-bench {}", env!("CARGO_PKG_VERSION"));
        return glib::ExitCode::SUCCESS;
    }
    if opts.report {
        return match report::print(&opts.files) {
            Ok(()) => glib::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("wayland-compositor-bench: {e}");
                glib::ExitCode::FAILURE
            }
        };
    }
    if let Some((a, b)) = &opts.compare {
        return match report::compare(a, b, &opts.files) {
            Ok(()) => glib::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("wayland-compositor-bench: {e}");
                glib::ExitCode::FAILURE
            }
        };
    }
    if opts.check {
        return check(&opts);
    }

    let app = gtk::Application::builder()
        .application_id("dev.bench.WaylandBench")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();

    app.connect_startup(|_| {
        let provider = gtk::CssProvider::new();
        provider.load_from_data(CSS);
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
    });

    // `--run` is scripted: a failed JSON write has to reach the process exit
    // status, not just a row in the window that is about to close.
    let failed = Rc::new(Cell::new(false));
    let failed_activate = failed.clone();
    app.connect_activate(move |app| build_ui(app, opts.clone(), failed_activate.clone()));
    // We already consumed our own flags; hand GApplication a clean argv so it
    // does not reject --run/--json/--windows.
    let code = app.run_with_args(&["wayland-compositor-bench"]);
    if failed.get() {
        glib::ExitCode::FAILURE
    } else {
        code
    }
}

fn build_ui(app: &gtk::Application, opts: cli::Options, failed: Rc<Cell<bool>>) {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Wayland Compositor Bench")
        .default_width(900)
        .default_height(1000)
        .decorated(false)
        .build();
    window.add_css_class("wbg");

    // No transparent margin: the card *is* the toplevel surface, so the
    // compositor's focus ring (drawn on the surface edge) hugs its border
    // instead of floating a few pixels away from it.
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let card = gtk::Box::new(gtk::Orientation::Vertical, 12);
    card.add_css_class("card");
    card.set_vexpand(true);
    root.append(&card);

    // ------------------------------------------------------------- header --
    let start = gtk::Button::with_label("Start");
    start.add_css_class("start");
    start.set_valign(gtk::Align::Center);

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let titles = gtk::Box::new(gtk::Orientation::Vertical, 1);
    let title = gtk::Label::new(Some("Wayland Compositor Bench"));
    title.set_xalign(0.0);
    title.add_css_class("title");
    let subtitle = gtk::Label::new(Some("system info, session footprint and frame pacing"));
    subtitle.set_xalign(0.0);
    subtitle.add_css_class("subtitle");
    titles.append(&title);
    titles.append(&subtitle);

    let blur_chip = gtk::Button::with_label("blur: …");
    blur_chip.add_css_class("chip");
    blur_chip.set_valign(gtk::Align::Center);
    blur_chip.set_tooltip_text(Some("Checking the compositor's blur support"));
    let close = gtk::Button::with_label("✕");
    close.add_css_class("ghost");
    close.set_valign(gtk::Align::Center);
    close.set_size_request(38, 38);
    close.set_tooltip_text(Some("Close"));
    {
        let w = window.clone();
        close.connect_clicked(move |_| w.close());
    }
    header.append(&titles);
    header.append(&blur_chip);
    header.append(&start);
    header.append(&close);
    titles.set_hexpand(true);
    card.append(&header);

    let status = gtk::Label::new(Some("idle"));
    status.set_xalign(0.0);
    status.add_css_class("key");
    card.append(&status);

    let progress = gtk::ProgressBar::new();
    progress.set_visible(false);
    card.append(&progress);

    // frame-test surface + post-run radar share the right-hand column
    let surface = gtk::Box::new(gtk::Orientation::Vertical, 0);
    surface.add_css_class("chart");
    surface.set_visible(false);

    // Resolve the compositor once: the pre-run probe and the run both need it.
    let comp = proc::compositor();
    let summary_ui = SummaryUi::build();

    // before the first run the right-hand pane explains itself; the animation
    // takes over during a run and the radar once it is done
    let placeholder = gtk::Box::new(gtk::Orientation::Vertical, 4);
    placeholder.add_css_class("chart");
    placeholder.set_valign(gtk::Align::Fill);
    let ph_inner = gtk::Box::new(gtk::Orientation::Vertical, 4);
    ph_inner.set_valign(gtk::Align::Center);
    ph_inner.set_halign(gtk::Align::Center);
    ph_inner.set_vexpand(true);
    let ph_title = gtk::Label::new(Some("no benchmark yet"));
    ph_title.add_css_class("title");
    let ph_hint = gtk::Label::new(Some(
        "press Start: the animation runs here, then the radar chart",
    ));
    ph_hint.add_css_class("stat-sub");
    ph_hint.set_wrap(true);
    ph_hint.set_justify(gtk::Justification::Center);
    // Filled after the first frame: `power::readable()` may fork `sudo -n`, and
    // that must not delay the window. Hidden until the probe has run; then it
    // always shows a tick, or a cross when a source is missing.
    let ph_warn = gtk::Label::new(None);
    ph_warn.add_css_class("access-check");
    ph_warn.set_xalign(0.0);
    ph_warn.set_wrap(true);
    ph_warn.set_justify(gtk::Justification::Left);
    ph_warn.set_halign(gtk::Align::Start);
    ph_warn.set_max_width_chars(54);
    ph_warn.set_margin_top(8);
    ph_warn.set_selectable(true);
    ph_warn.set_visible(false);
    ph_inner.append(&ph_title);
    ph_inner.append(&ph_hint);
    ph_inner.append(&ph_warn);
    placeholder.append(&ph_inner);

    let stack = gtk::Stack::new();
    stack.set_hexpand(true);
    stack.set_vexpand(false);
    // no fixed height: the left column is always the taller of the two, so it
    // sets the row height and both panels fill it (equal edges, no resizing
    // when the placeholder is swapped for the radar)
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    stack.set_transition_duration(220);
    stack.add_named(&placeholder, Some("placeholder"));
    stack.add_named(&surface, Some("surface"));
    stack.add_named(&summary_ui.root, Some("summary"));
    stack.set_visible_child(&placeholder);

    let mut labels: Labels = HashMap::new();

    // left column: session identity and hardware, with the live counters
    // folded away underneath in the same panel
    let basic_col = gtk::Box::new(gtk::Orientation::Vertical, 2);
    basic_col.add_css_class("panel");
    basic_col.set_size_request(390, -1);
    // fill the row height so this panel and the chart card line up on both
    // edges while both collapsibles are collapsed
    basic_col.set_valign(gtk::Align::Fill);
    // the value labels below set hexpand, which would otherwise propagate up
    // and make this column eat the extra space meant for the chart column
    basic_col.set_hexpand(false);
    let basic_title = gtk::Label::new(Some("BASIC SYSTEM INFO"));
    basic_title.set_xalign(0.0);
    basic_title.add_css_class("section");
    basic_col.append(&basic_title);
    basic_col.append(&build_basic_grid(&mut labels));

    let other_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(260)
        .margin_top(4)
        .build();
    other_scroll.set_child(Some(&build_other_grid(&mut labels)));
    let other_label = gtk::Label::new(Some("Other system info"));
    other_label.add_css_class("value");
    let other_expander = gtk::Expander::new(None);
    other_expander.set_label_widget(Some(&other_label));
    other_expander.set_child(Some(&other_scroll));
    other_expander.set_expanded(false);
    other_expander.set_margin_top(6);
    basic_col.append(&other_expander);

    let top = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    top.append(&basic_col);
    top.append(&stack);
    card.append(&top);

    // ------------------------------------------------------ ram snapshot ---
    // Full width under the two panels: what was holding memory when the window
    // opened. Taken once, here — a benchmark run must not change the picture.
    let ram_items = proc::ram_by_name();
    let ram_total: f64 = ram_items.iter().map(|(_, mib)| *mib).sum();
    let ram_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
    ram_box.add_css_class("panel");
    let ram_head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let ram_title = gtk::Label::new(Some("TOP RAM CONSUMERS"));
    ram_title.set_xalign(0.0);
    ram_title.add_css_class("section");
    ram_title.set_hexpand(true);
    let ram_meta = gtk::Label::new(Some(&format!(
        "at launch · RSS grouped by process name · {} names · {:.1} GiB",
        ram_items.len(),
        ram_total / 1024.0
    )));
    ram_meta.set_xalign(1.0);
    ram_meta.add_css_class("stat-sub");
    ram_head.append(&ram_title);
    ram_head.append(&ram_meta);
    ram_box.append(&ram_head);
    ram_box.append(&ramchart::Chart::new(&ram_items).root);
    card.append(&ram_box);

    // drag to move (undecorated) — only on the title block, so clicks on
    // Start/Restart and the close button are never swallowed by a move grab
    let drag = gtk::GestureDrag::new();
    drag.set_button(1);
    {
        let w = window.clone();
        drag.connect_drag_begin(move |g, x, y| {
            if let (Some(dev), Some(surface)) = (g.current_event_device(), w.surface()) {
                if let Ok(tl) = surface.downcast::<gtk::gdk::Toplevel>() {
                    tl.begin_move(&dev, 1, x, y, g.current_event_time());
                }
            }
        });
    }
    titles.add_controller(drag);

    // ---------------------------------------------------------- features --
    // always visible; only the protocol list itself scrolls
    let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
    card.append(&sep);

    let feat_title = gtk::Label::new(Some("WAYLAND FEATURES SUPPORTED"));
    feat_title.set_xalign(0.0);
    feat_title.add_css_class("section");
    card.append(&feat_title);
    card.append(&summary_ui.feat_label);

    let feat_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        // Let the list size the window, but stop before it pushes the toplevel
        // past the screen: the rest scrolls.
        .propagate_natural_height(true)
        .max_content_height(240)
        .vexpand(true)
        .margin_top(2)
        .build();
    feat_scroll.set_child(Some(&summary_ui.feats));
    card.append(&feat_scroll);

    // ---------------------------------------------------------- benchmark --
    // animation surface used by the frame test
    let area = gtk::DrawingArea::new();
    area.set_content_height(126);
    area.set_vexpand(true);
    let phase = Rc::new(Cell::new(0.0f64));
    let ph = phase.clone();
    area.set_draw_func(move |_, cr, w, h| {
        let p = ph.get() + 0.045;
        ph.set(p);
        // clear to transparent so the compositor's blurred backdrop shows through
        cr.set_operator(gtk::cairo::Operator::Source);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
        cr.paint().ok();
        cr.set_operator(gtk::cairo::Operator::Over);
        let w = w as f64;
        let h = h as f64;
        let bars = 26.0;
        for i in 0..26 {
            let f = i as f64;
            let v = (p + f * 0.19).sin() * 0.5 + 0.5;
            let bw = w / bars;
            let bh = 12.0 + v * (h - 40.0);
            let x = f * bw + 2.0;
            let y = h - bh - 8.0;
            cr.set_source_rgba(0.31, 0.55, 1.0, 0.35 + v * 0.6);
            cr.rectangle(x, y, bw - 4.0, bh);
            cr.fill().ok();
        }
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.75);
        cr.select_font_face(
            "Sans",
            gtk::cairo::FontSlant::Normal,
            gtk::cairo::FontWeight::Bold,
        );
        cr.set_font_size(14.0);
        cr.move_to(14.0, 24.0);
        cr.show_text("frame test surface").ok();
    });
    area.add_css_class("area");
    surface.append(&area);

    // ------------------------------------------------------------- wiring --
    let scan_cell: Rc<RefCell<Option<wlscan::Scan>>> = Rc::new(RefCell::new(None));
    let blur_state = Rc::new(Cell::new(wlscan::BlurState::Error));
    let blur_off = Rc::new(Cell::new(false));
    // Clicking the chip makes the window opaque: the CSS class drops the
    // backdrop-filter, so GTK stops asking the compositor to blur behind it.
    {
        let w = window.clone();
        let chip = blur_chip.clone();
        let off = blur_off.clone();
        let state = blur_state.clone();
        blur_chip.connect_clicked(move |_| {
            let now = !off.get();
            off.set(now);
            if now {
                w.add_css_class("solid");
            } else {
                w.remove_css_class("solid");
            }
            set_blur_chip(&chip, state.get(), now);
        });
    }
    let comp_pid = comp.as_ref().map(|c| c.pid);
    let monitor = gtk::gdk::Display::default()
        .and_then(|d| d.monitors().item(0))
        .and_then(|o| o.downcast::<gtk::gdk::Monitor>().ok());
    let refresh_hz = monitor
        .as_ref()
        .map(|m| {
            let hz = m.refresh_rate() as f64 / 1000.0;
            if hz > 0.0 {
                hz
            } else {
                60.0
            }
        })
        .unwrap_or(60.0);
    let (logical, scale) = monitor
        .as_ref()
        .map(|m| {
            let g = m.geometry();
            let (phys, _) = sysinfo::drm_mode().unwrap_or_default();
            let phys_w: f64 = phys
                .split('x')
                .next()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.0);
            let frac = if g.width() > 0 && phys_w > 0.0 {
                phys_w / g.width() as f64
            } else {
                m.scale_factor() as f64
            };
            ((g.width() as u32, g.height() as u32), frac)
        })
        .unwrap_or(((0, 0), 1.0));

    let busy: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    let ui = bench::BenchUi {
        app: app.clone(),
        window: window.clone(),
        area: area.clone(),
        surface: surface.clone(),
        stack: stack.clone(),
        start: start.clone(),
        progress: progress.clone(),
        status: status.clone(),
        grid: summary_ui.results.clone(),
        results_expander: summary_ui.results_expander.clone(),
        row: Rc::new(Cell::new(0)),
        comp: comp.clone(),
        refresh_hz,
        logical,
        scale,
        busy: busy.clone(),
        summary: {
            let sui = summary_ui.clone();
            let stack_c = stack.clone();
            Rc::new(move |sum: bench::Summary| {
                sui.show(&sum);
                stack_c.set_visible_child(&sui.root);
            })
        },
        blur_off: blur_off.clone(),
        failed: failed.clone(),
        scan: scan_cell.clone(),
        opts: opts.clone(),
    };
    {
        let ui_click = ui.clone();
        start.connect_clicked(move |_| bench::start(ui_click.clone()));
        if opts.run {
            glib::timeout_add_local_once(Duration::from_millis(900), move || {
                bench::start(ui.clone());
            });
        }
    }

    // static hardware info
    let gpu = sysinfo::gpu_info(comp_pid);
    set(&labels, "CPU", &cpu_model_clean());
    set(&labels, "CPU cores", &cpu_detail());
    set(&labels, "GPU", &gpu.name);
    set(
        &labels,
        "GPU driver",
        &match (gpu.driver.is_empty(), gpu.pci_id.is_empty()) {
            (true, true) => "unknown".into(),
            (true, false) => gpu.pci_id.clone(),
            (false, true) => gpu.driver.clone(),
            (false, false) => format!("{} · {}", gpu.driver, gpu.pci_id),
        },
    );
    if let Some(l) = labels.get("GPU") {
        if !gpu.others.is_empty() {
            l.set_tooltip_text(Some(&format!("other GPUs: {}", gpu.others.join(", "))));
        }
    }
    if let Some(l) = labels.get("GPU driver") {
        if !gpu.pci_slot.is_empty() {
            l.set_tooltip_text(Some(&format!("PCI {}", gpu.pci_slot)));
        }
    }
    set(&labels, "Mesa", &sysinfo::mesa_version());
    set(&labels, "OS", &sysinfo::os_name());
    set(
        &labels,
        "Kernel",
        &format!(
            "Linux {}",
            sysinfo::read("/proc/sys/kernel/osrelease").unwrap_or_default()
        ),
    );

    // live info
    let comp_line = comp
        .as_ref()
        .map(|c| {
            format!(
                "{} (pid {}, {} threads)",
                c.name,
                c.pid,
                proc::status(c.pid).threads
            )
        })
        .unwrap_or_else(|| "not detected".into());
    // Identity comes from the socket owner. GNOME Shell is a systemd user
    // service and inherits the user manager's environment, which survives a
    // switch from another desktop, so the variable alone is not reliable.
    let desktop = proc::desktop(comp.as_ref());
    let desktop_line = pretty_desktop(&desktop.name);
    // One `--version` run feeds both the row and its tooltip.
    let version_line = comp
        .as_ref()
        .and_then(|c| proc::compositor_version(&c.name));
    let compositor_line = match comp.as_ref() {
        Some(c) => compositor_identity(
            proc::compositor_impl(&c.name),
            version_line.as_deref(),
            &desktop_line,
        ),
        None => desktop_line.clone(),
    };
    let desktop_tip = desktop.mismatch().then(|| {
        format!(
            "XDG_CURRENT_DESKTOP={} is left over from another session; the Wayland socket \
             belongs to {}",
            desktop.env,
            comp.as_ref()
                .map(|c| c.name.as_str())
                .unwrap_or("an unknown process")
        )
    });
    let comp_tip = {
        let mut tips = Vec::new();
        if let (Some(c), Some(v)) = (comp.as_ref(), version_line.as_deref()) {
            tips.push(format!("{} --version: {v}", c.name));
        }
        if let Some(t) = &desktop_tip {
            tips.push(t.clone());
        }
        (!tips.is_empty()).then(|| tips.join("\n"))
    };

    let perf_tip = performance_tip();
    let labels_c = labels.clone();
    let monitor_c = monitor.clone();
    let comp_line_c = comp_line.clone();
    let perf_tip_c = perf_tip.clone();
    let update = move |labels: &Labels, monitor: &Option<gtk::gdk::Monitor>| {
        set(labels, "Wayland compositor", &compositor_line);
        if let Some(tip) = &comp_tip {
            if let Some(l) = labels.get("Wayland compositor") {
                l.set_tooltip_text(Some(tip));
            }
        }
        set(
            labels,
            "Wayland display",
            &std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "—".into()),
        );
        set(labels, "Compositor process", &comp_line_c);

        if let Some(m) = monitor {
            let g = m.geometry();
            let refresh = m.refresh_rate() as f64 / 1000.0;
            let (phys, _) = sysinfo::drm_mode().unwrap_or(("unknown".into(), 0));
            let phys_w: f64 = phys
                .split('x')
                .next()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.0);
            let frac = if g.width() > 0 && phys_w > 0.0 {
                phys_w / g.width() as f64
            } else {
                0.0
            };
            set(labels, "Physical mode", &phys);
            set(
                labels,
                "Logical size",
                &format!("{}×{}", g.width(), g.height()),
            );
            set(
                labels,
                "Resolution",
                &format!("{}×{} · {frac:.2}×", g.width(), g.height()),
            );
            set(
                labels,
                "Scale",
                &format!("{frac:.2}× (GDK integer {})", m.scale_factor()),
            );
            set(labels, "Refresh", &format!("{refresh:.2} Hz"));
        }
        if let Some(b) = sysinfo::backlight() {
            set(
                labels,
                "Brightness",
                &format!(
                    "{} / {} ({:.0}%)",
                    b.value,
                    b.max,
                    if b.max > 0 {
                        b.value as f64 / b.max as f64 * 100.0
                    } else {
                        0.0
                    }
                ),
            );
        }

        set(labels, "Performance mode", &sysinfo::performance_mode());
        set_performance_tip(labels, &perf_tip_c);

        let (total, avail) = sysinfo::total_ram_mib();
        let used = (total - avail).max(0.0);
        set(
            labels,
            "RAM",
            &format!("{:.1} GiB · {:.1} GiB used", total / 1024.0, used / 1024.0),
        );

        let snap = sysinfo::Snapshot::take(comp_pid);
        set(labels, "Session memory", &sysinfo::fmt_mem(&snap));
        set(
            labels,
            "Peak memory (since login)",
            &sysinfo::fmt_mib(proc::memory_peak_mib(proc::session_cgroup())),
        );
        if let Some(p) = comp_pid {
            set(
                labels,
                "Compositor memory (PSS)",
                &sysinfo::fmt_mib(proc::pss_tree(p).0 as f64 / 1024.0),
            );
            let gpu = proc::gpu(p);
            if gpu.available() {
                set(
                    labels,
                    "GPU memory (compositor)",
                    &sysinfo::fmt_mib(gpu.resident as f64 / 1048576.0),
                );
            }
        }
        let cg = proc::session_cgroup();
        let ps = sysinfo::psi(cg, "cpu.pressure").unwrap_or((0.0, 0.0));
        let pm = sysinfo::psi(cg, "memory.pressure").unwrap_or((0.0, 0.0));
        set(
            labels,
            "Pressure (cpu/mem)",
            &format!("cpu {:.1}% / mem {:.1}% (avg10)", ps.0, pm.0),
        );
        if let Some(t) = sysinfo::package_temp_c() {
            set(labels, "Package temp", &format!("{t:.0} °C"));
        }
    };

    // first pass + 1 s refresh
    update(&labels_c, &monitor_c);
    // Seed with the live counter, not zero: the first tick must not report the
    // cumulative time since boot as one second of CPU.
    let cg_t = proc::session_cgroup();
    let cpu_state = Cell::new((std::time::Instant::now(), proc::cpu_usage_usec(cg_t)));
    let labels_t = labels.clone();
    let perf_tip_t = perf_tip.clone();
    let comp_t = comp_pid;
    let busy_t = busy.clone();
    let tick = Cell::new(0u32);
    glib::timeout_add_local(Duration::from_secs(1), move || {
        // The benchmark is measuring this very process: park the pollers while a
        // run is in progress so the numbers do not include their own updater.
        // Re-baseline on the way out, or the first tick after the run would
        // present the run's average as one second of idle session.
        if busy_t.get() {
            cpu_state.set((std::time::Instant::now(), proc::cpu_usage_usec(cg_t)));
            return glib::ControlFlow::Continue;
        }
        let n = tick.get().wrapping_add(1);
        tick.set(n);
        // cgroup counters are two small reads: refresh them every second.
        let (last_t, last_cpu) = cpu_state.get();
        let snap = sysinfo::Snapshot::take_light();
        let dt = last_t.elapsed().as_secs_f64();
        if dt > 0.2 {
            let pct = snap.cpu_usec.saturating_sub(last_cpu) as f64 / 1e6 / dt * 100.0;
            set(&labels_t, "Session CPU", &format!("{pct:.2} % of one core"));
            cpu_state.set((std::time::Instant::now(), snap.cpu_usec));
        }
        set(&labels_t, "Session memory", &sysinfo::fmt_mem(&snap));
        // the profile can be switched at runtime by power-profiles-daemon
        set(&labels_t, "Performance mode", &sysinfo::performance_mode());
        set_performance_tip(&labels_t, &perf_tip_t);
        // The `/proc` walks and hwmon reads below are the expensive part; once
        // every five seconds is plenty, they do not change frame to frame.
        if n.is_multiple_of(5) {
            set(
                &labels_t,
                "Peak memory (since login)",
                &sysinfo::fmt_mib(proc::memory_peak_mib(cg_t)),
            );
            if let Some(p) = comp_t {
                set(
                    &labels_t,
                    "Compositor memory (PSS)",
                    &sysinfo::fmt_mib(proc::pss_tree(p).0 as f64 / 1024.0),
                );
                let gpu = proc::gpu(p);
                if gpu.available() {
                    set(
                        &labels_t,
                        "GPU memory (compositor)",
                        &sysinfo::fmt_mib(gpu.resident as f64 / 1048576.0),
                    );
                }
            }
            let ps = sysinfo::psi(cg_t, "cpu.pressure").unwrap_or((0.0, 0.0));
            let pm = sysinfo::psi(cg_t, "memory.pressure").unwrap_or((0.0, 0.0));
            set(
                &labels_t,
                "Pressure (cpu/mem)",
                &format!("cpu {:.1}% / mem {:.1}% (avg10)", ps.0, pm.0),
            );
            if let Some(t) = sysinfo::package_temp_c() {
                set(&labels_t, "Package temp", &format!("{t:.0} °C"));
            }
        }
        glib::ControlFlow::Continue
    });

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&root));
    add_resize_grips(&overlay, &window);
    window.set_child(Some(&overlay));
    window.present();

    // The access probe may fork `sudo -n` for RAPL, so run it after the first
    // frame; the placeholder is already visible by then. The result is always
    // shown: a tick says every source was checked, a cross names the ones that
    // are missing.
    {
        let warn = ph_warn.clone();
        let comp = comp.clone();
        glib::timeout_add_local_once(Duration::from_millis(200), move || {
            let issues = access::probe(comp.as_ref());
            if issues.is_empty() {
                warn.add_css_class("access-ok");
            } else {
                warn.add_css_class("access-warn");
            }
            warn.set_markup(&access::markup(&issues));
            warn.set_visible(true);
        });
    }

    // run the probe after the window is up, so its roundtrip does not delay
    // the first frame
    {
        let chip = blur_chip.clone();
        let blur_label = labels.get("Blur protocol").cloned();
        let cell = scan_cell.clone();
        let sui = summary_ui.clone();
        let state = blur_state.clone();
        let off = blur_off.clone();
        glib::timeout_add_local_once(Duration::from_millis(150), move || {
            let scan = wlscan::scan();
            state.set(scan.blur);
            set_blur_chip(&chip, scan.blur, off.get());
            if let Some(l) = blur_label {
                l.set_text(&format!(
                    "{} · {} globals advertised",
                    scan.blur.label(),
                    scan.globals.len()
                ));
            }
            // populate the features section straight away, before any run
            let feats = wlscan::evaluate(&scan.globals);
            sui.set_features(
                &feats.items,
                feats.supported,
                feats.total,
                feats.supported_all,
                feats.total_all,
                feats.extra,
            );
            *cell.borrow_mut() = Some(scan);
        });
    }
}

fn cpu_model_clean() -> String {
    sysinfo::cpu_model()
        .replace("(R)", "")
        .replace("(TM)", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn cpu_detail() -> String {
    let (cores, threads) = sysinfo::cpu_topology();
    let mut s = format!("{cores} cores · {threads} threads");
    if let Some(ghz) = sysinfo::cpu_max_ghz() {
        s.push_str(&format!(" · {ghz:.1} GHz max"));
    }
    s
}

/// Radar ceiling: this machine's total RAM, so the memory vertex shows a share
/// of what the system has instead of an arbitrary constant. The CPU axis is
/// relative to one core, which is the unit every CPU figure in the UI uses.
fn radar_limits() -> radar::Limits {
    let (total_ram_mib, _) = sysinfo::total_ram_mib();
    radar::Limits { total_ram_mib }
}

/// `labwc:wlroots` reads better as `labwc · wlroots` in a dense info column.
fn pretty_desktop(v: &str) -> String {
    let parts: Vec<&str> = v.split(':').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        "—".into()
    } else {
        parts.join(" · ")
    }
}

/// `Mutter 51.1 · GNOME`: the software that composites, its release, and the
/// desktop the session belongs to. A standalone compositor is its own desktop,
/// so `sway 1.11 · sway` collapses to `sway 1.11`, and `labwc · wlroots` keeps
/// only the part that adds information. A session flavour that is just the
/// compositor's own name (`cosmic-comp-traffic`) is dropped too: the row
/// already starts with it.
fn compositor_identity(name: &str, version_line: Option<&str>, desktop: &str) -> String {
    let mut line = match version_line.and_then(proc::version_token) {
        Some(v) => format!("{name} {v}"),
        None => name.to_string(),
    };
    let desktop = if desktop == "—" { "" } else { desktop };
    let parts: Vec<&str> = desktop
        .split(" · ")
        .filter(|p| !p.is_empty() && !names_the_compositor(p, name))
        .collect();
    if !parts.is_empty() {
        line.push_str(" · ");
        line.push_str(&parts.join(" · "));
    }
    line
}

/// True for `sway` next to `sway`, and for `cosmic-comp-traffic` next to
/// `cosmic-comp`; not for `GNOME` next to `Mutter` or `ubuntu` next to `GNOME`.
fn names_the_compositor(part: &str, name: &str) -> bool {
    let (part, name) = (part.to_ascii_lowercase(), name.to_ascii_lowercase());
    part == name
        || part
            .strip_prefix(&name)
            .is_some_and(|rest| rest.starts_with('-') || rest.starts_with(' '))
}

/// The window is undecorated, so give the compositor drag handles on all eight
/// edges. Each handle is a transparent widget that starts an interactive resize
/// on drag; without these the only way to resize is a keyboard shortcut.
fn add_resize_grips(overlay: &gtk::Overlay, window: &gtk::ApplicationWindow) {
    use gtk::gdk::SurfaceEdge;
    use gtk::Align;

    const EDGE: i32 = 6;
    const CORNER: i32 = 14;

    let grips = [
        (
            SurfaceEdge::North,
            Align::Fill,
            Align::Start,
            0,
            EDGE,
            "n-resize",
        ),
        (
            SurfaceEdge::South,
            Align::Fill,
            Align::End,
            0,
            EDGE,
            "s-resize",
        ),
        (
            SurfaceEdge::West,
            Align::Start,
            Align::Fill,
            EDGE,
            0,
            "w-resize",
        ),
        (
            SurfaceEdge::East,
            Align::End,
            Align::Fill,
            EDGE,
            0,
            "e-resize",
        ),
        (
            SurfaceEdge::NorthWest,
            Align::Start,
            Align::Start,
            CORNER,
            CORNER,
            "nw-resize",
        ),
        (
            SurfaceEdge::NorthEast,
            Align::End,
            Align::Start,
            CORNER,
            CORNER,
            "ne-resize",
        ),
        (
            SurfaceEdge::SouthWest,
            Align::Start,
            Align::End,
            CORNER,
            CORNER,
            "sw-resize",
        ),
        (
            SurfaceEdge::SouthEast,
            Align::End,
            Align::End,
            CORNER,
            CORNER,
            "se-resize",
        ),
    ];

    for (edge, halign, valign, w, h, cursor) in grips {
        let area = gtk::DrawingArea::new();
        area.set_size_request(w, h);
        area.set_halign(halign);
        area.set_valign(valign);
        area.set_hexpand(halign == Align::Fill);
        area.set_vexpand(valign == Align::Fill);
        area.set_cursor_from_name(Some(cursor));

        let drag = gtk::GestureDrag::new();
        drag.set_button(1);
        let win = window.clone();
        drag.connect_drag_begin(move |g, x, y| {
            if let (Some(dev), Some(surface)) = (g.current_event_device(), win.surface()) {
                if let Ok(tl) = surface.downcast::<gtk::gdk::Toplevel>() {
                    tl.begin_resize(edge, Some(&dev), 1, x, y, g.current_event_time());
                }
            }
        });
        area.add_controller(drag);
        overlay.add_overlay(&area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_compositor_row_names_the_implementation_and_the_desktop() {
        // Mutter is a library inside GNOME Shell: the row must not say
        // "GNOME" alone, which is the desktop, nor "gnome-shell", which is
        // the process.
        assert_eq!(
            compositor_identity("Mutter", Some("GNOME Shell 51.1"), "GNOME"),
            "Mutter 51.1 · GNOME"
        );
        assert_eq!(
            compositor_identity("KWin", Some("kwin 6.4.5"), "KDE Plasma"),
            "KWin 6.4.5 · KDE Plasma"
        );
        assert_eq!(
            compositor_identity("cosmic-comp", Some("cosmic-comp 1.0.0"), "COSMIC"),
            "cosmic-comp 1.0.0 · COSMIC"
        );
    }

    #[test]
    fn a_standalone_compositor_does_not_name_its_desktop_twice() {
        assert_eq!(
            compositor_identity("sway", Some("sway version 1.11"), "sway"),
            "sway 1.11"
        );
        assert_eq!(compositor_identity("labwc", None, "labwc"), "labwc");
        assert_eq!(
            compositor_identity("labwc", None, "labwc · wlroots"),
            "labwc · wlroots"
        );
        // the session flavour is the compositor's own name: the row already
        // starts with the implementation, so it adds nothing
        assert_eq!(
            compositor_identity(
                "cosmic-comp",
                Some("cosmic-comp 1.0.0 (git commit 8cb1802)"),
                "cosmic-comp-traffic · COSMIC"
            ),
            "cosmic-comp 1.0.0 · COSMIC"
        );
        // …but a desktop that merely resembles the name is kept
        assert_eq!(compositor_identity("sway", None, "swayfx"), "sway · swayfx");
    }

    #[test]
    fn an_unknown_session_keeps_the_desktop_it_has() {
        assert_eq!(
            compositor_identity("Mutter", Some("GNOME Shell 51.1"), "ubuntu · GNOME"),
            "Mutter 51.1 · ubuntu · GNOME"
        );
        assert_eq!(compositor_identity("Mutter", None, "—"), "Mutter");
    }
}
