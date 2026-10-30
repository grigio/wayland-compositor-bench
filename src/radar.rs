//! Six-axis radar chart for the post-run summary.
//!
//! Every axis is "further out is better" and every radius is measured against
//! what the run and this machine establish, not against a hard-coded ceiling:
//!
//! - pacing: the share of frames the compositor delivered at the cadence it
//!   settled into, for one surface and for the surface plus N toplevels. A
//!   clean 60 fps on a 120 Hz panel is 100 %, not 50 %;
//! - render cost: compositor CPU per presented frame against the frame period
//!   of the observed cadence (`1 - ms / (1000 / cadence_hz)`), so the vertex
//!   reads as the share of a frame the compositor spends rendering. It is
//!   independent of GPU speed;
//! - memory: the compositor's PSS against the machine's total RAM
//!   (`1 - ln(1 + MiB) / ln(1 + total)`), which keeps 50 MiB and 1.2 GiB
//!   visibly apart instead of both hugging the outer ring;
//! - CPU: compositor CPU at rest as a share of one core, also logarithmically:
//!   the interesting idle range is 0.3–3 % and a linear axis would squash it
//!   into a pixel;
//! - features: protocol coverage over the scored tiers.
//!
//! The absolute number is printed at each vertex — that is what you quote.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;

/// Used when the machine cannot be probed, so the chart still draws something
/// sane instead of dividing by a zero-sized ceiling.
const FALLBACK_RAM_MIB: f64 = 2048.0;

/// The ceiling this machine offers, so a vertex reads as "share of what the
/// system has" rather than against an arbitrary constant.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// total RAM in MiB: 0 MiB of compositor plots at 1.0, all of it at 0.0
    pub total_ram_mib: f64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            total_ram_mib: FALLBACK_RAM_MIB,
        }
    }
}

const TITLES: [&str; 6] = [
    "PACING",
    "LOAD PACING",
    "RENDER COST",
    "COMP RAM",
    "COMP CPU",
    "FEATURES",
];
const COLORS: [(f64, f64, f64); 6] = [
    (0.42, 0.62, 1.00), // blue    pacing, one surface
    (0.20, 0.78, 0.64), // teal    pacing under load
    (0.96, 0.72, 0.30), // amber   render cost
    (0.35, 0.85, 0.60), // green   compositor RAM
    (0.96, 0.46, 0.46), // red     compositor CPU
    (0.72, 0.55, 1.00), // violet  features
];

/// Hover help: what each axis plots and which direction is good. Every axis is
/// drawn "further out is better", including the cost axes, where the raw number
/// is better when lower.
const HELP: [&str; 6] = [
    "Pacing: share of frames the compositor presented at the cadence it settled into, with one animated surface. Higher is better — 100 % means no frame was missed.",
    "Pacing under load: the same share with the extra toplevels open, which is what a real desktop does all day. Higher is better.",
    "Render cost: compositor CPU time per presented frame in the load test, as a share of the frame period at the observed cadence. Higher is better — further out means less CPU per frame.",
    "Compositor memory: its process-tree PSS at rest, log-scaled against this machine's total RAM. Higher is better — further out means a smaller footprint.",
    "Compositor CPU: its CPU at rest as a share of one core, log-scaled. Higher is better — further out means a quieter compositor.",
    "Features: scored Wayland protocol coverage (Core, Rendering and Desktop tiers). Higher is better.",
];

/// Where one axis label was last drawn, in widget pixels. Filled by `draw` and
/// read by the motion controller and the tooltip query.
#[derive(Clone, Copy, Debug, Default)]
struct HitBox {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl HitBox {
    fn contains(&self, x: f64, y: f64) -> bool {
        self.x1 > self.x0 && x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }
}

fn hit_index(boxes: &[HitBox; 6], x: f64, y: f64) -> Option<usize> {
    boxes.iter().position(|b| b.contains(x, y))
}

/// What the chart plots. Everything is expressed in the unit you would quote.
#[derive(Clone, Copy, Debug, Default)]
pub struct Values {
    /// share of frames delivered at the observed cadence, 0..1
    pub pacing_single: f64,
    pub pacing_multi: f64,
    /// compositor CPU per presented frame, multi-surface test
    pub comp_ms_per_frame: f64,
    /// the cadence that test settled into: its period is the frame budget
    pub cadence_hz: f64,
    /// compositor process-tree PSS at rest
    pub comp_pss_mib: f64,
    /// compositor CPU at rest, % of one core
    pub comp_cpu_pct: f64,
    /// false when no compositor process was resolved: the two compositor axes
    /// have no measurement, so they must not plot as zero cost
    pub comp_known: bool,
    pub feat_pct: f64,
}

pub struct Radar {
    pub area: gtk::DrawingArea,
    limits: Limits,
    /// normalized 0..1 per axis
    values: Rc<RefCell<[f64; 6]>>,
    texts: Rc<RefCell<[String; 6]>>,
    /// 0..1 grow-in progress
    shown: Rc<Cell<f64>>,
    generation: Rc<Cell<u64>>,
}

impl Radar {
    pub fn new(limits: Limits) -> Self {
        let area = gtk::DrawingArea::new();
        area.set_content_width(360);
        // Just tall enough for the ring plus the label lines above it and the
        // air below: the version line sits right under the chart, not under a
        // band of empty pixels.
        area.set_content_height(228);
        area.set_hexpand(true);
        area.set_halign(gtk::Align::Fill);

        let values = Rc::new(RefCell::new([0.0f64; 6]));
        let texts = Rc::new(RefCell::new(std::array::from_fn(|_| "—".to_string())));
        let shown = Rc::new(Cell::new(0.0f64));
        let hovered = Rc::new(Cell::new(None::<usize>));
        let hitboxes = Rc::new(RefCell::new([HitBox::default(); 6]));

        {
            let v = values.clone();
            let t = texts.clone();
            let p = shown.clone();
            let h = hovered.clone();
            let b = hitboxes.clone();
            area.set_draw_func(move |_, cr, w, hh| {
                draw(
                    cr,
                    w as f64,
                    hh as f64,
                    &v.borrow(),
                    &t.borrow(),
                    p.get(),
                    h.get(),
                    &mut b.borrow_mut(),
                );
            });
        }

        // A pointer over an axis label highlights that axis, and the tooltip
        // explains what it plots and which direction is good.
        let motion = gtk::EventControllerMotion::new();
        {
            let b = hitboxes.clone();
            let h = hovered.clone();
            let a = area.clone();
            motion.connect_motion(move |_, x, y| {
                let i = hit_index(&b.borrow(), x, y);
                if h.get() != i {
                    h.set(i);
                    a.queue_draw();
                }
            });
        }
        {
            let h = hovered.clone();
            let a = area.clone();
            motion.connect_leave(move |_| {
                if h.get().is_some() {
                    h.set(None);
                    a.queue_draw();
                }
            });
        }
        area.add_controller(motion);

        area.set_has_tooltip(true);
        {
            let b = hitboxes.clone();
            area.connect_query_tooltip(move |_, x, y, _, tooltip| {
                match hit_index(&b.borrow(), f64::from(x), f64::from(y)) {
                    Some(i) => {
                        tooltip.set_text(Some(HELP[i]));
                        true
                    }
                    None => false,
                }
            });
        }

        Self {
            area,
            limits,
            values,
            texts,
            shown,
            generation: Rc::new(Cell::new(0)),
        }
    }

    pub fn set(&self, v: &Values) {
        let l = self.limits;
        // Without a resolved compositor there is no cost to plot. The centre is
        // the "no data" position, matching the dash on the label: a missing
        // compositor must not look like a compositor that costs nothing.
        let (comp_ram, comp_cpu) = if v.comp_known {
            (
                ram_norm(v.comp_pss_mib, l.total_ram_mib),
                cpu_norm(v.comp_cpu_pct, 100.0),
            )
        } else {
            (0.0, 0.0)
        };
        self.values.borrow_mut().copy_from_slice(&[
            pacing_norm(v.pacing_single),
            pacing_norm(v.pacing_multi),
            render_cost_norm(v.comp_ms_per_frame, v.cadence_hz),
            comp_ram,
            comp_cpu,
            (v.feat_pct / 100.0).clamp(0.0, 1.0),
        ]);

        {
            let mut t = self.texts.borrow_mut();
            t[0] = pacing_text(v.pacing_single);
            t[1] = pacing_text(v.pacing_multi);
            t[2] = if v.comp_ms_per_frame > 0.0 {
                format!("{:.1} ms", v.comp_ms_per_frame)
            } else {
                "—".to_string()
            };
            t[3] = if v.comp_known {
                crate::sysinfo::fmt_mib(v.comp_pss_mib)
            } else {
                "—".to_string()
            };
            t[4] = if v.comp_known {
                crate::sysinfo::fmt_pct(v.comp_cpu_pct)
            } else {
                "—".to_string()
            };
            t[5] = format!("{:.0}%", v.feat_pct);
        }

        // Start a fresh grow-in and cancel any in-flight animation.
        let gen = self.generation.get().wrapping_add(1);
        self.generation.set(gen);
        self.shown.set(0.0);

        let area = self.area.clone();
        let shown = self.shown.clone();
        let generation = self.generation.clone();
        let t0 = Instant::now();
        glib::timeout_add_local(Duration::from_millis(16), move || {
            if generation.get() != gen {
                return glib::ControlFlow::Break;
            }
            let p = (t0.elapsed().as_secs_f64() / 0.55).min(1.0);
            let e = 1.0 - (1.0 - p).powi(3);
            shown.set(e);
            area.queue_draw();
            if p < 1.0 {
                glib::ControlFlow::Continue
            } else {
                glib::ControlFlow::Break
            }
        });
    }
}

fn pacing_norm(share: f64) -> f64 {
    share.clamp(0.0, 1.0)
}

fn pacing_text(share: f64) -> String {
    if share > 0.0 {
        format!("{:.0}%", share * 100.0)
    } else {
        "—".to_string()
    }
}

/// `1.0` when the compositor uses no CPU per frame, `0.0` when one frame of
/// work costs a whole frame period at the cadence it settled into. Measuring
/// against the observed cadence, not the panel refresh, is what keeps a
/// compositor that legitimately runs at 60 Hz on a 120 Hz panel from being
/// judged against half the budget it actually needs.
fn render_cost_norm(ms: f64, cadence_hz: f64) -> f64 {
    let budget_ms = if cadence_hz > 1.0 {
        1000.0 / cadence_hz
    } else {
        1000.0 / 60.0
    };
    (1.0 - ms.max(0.0) / budget_ms).clamp(0.0, 1.0)
}

fn ram_norm(mib: f64, total_mib: f64) -> f64 {
    let total = if total_mib > 1.0 {
        total_mib
    } else {
        FALLBACK_RAM_MIB
    };
    let ratio = (1.0 + mib.max(0.0)).ln() / (1.0 + total).ln();
    (1.0 - ratio).clamp(0.0, 1.0)
}

fn cpu_norm(pct: f64, capacity_pct: f64) -> f64 {
    let capacity = if capacity_pct > 1.0 {
        capacity_pct
    } else {
        100.0
    };
    let ratio = (1.0 + pct.max(0.0)).ln() / (1.0 + capacity).ln();
    (1.0 - ratio).clamp(0.0, 1.0)
}

fn axis_angle(i: usize, n: usize) -> f64 {
    -std::f64::consts::FRAC_PI_2 + i as f64 * std::f64::consts::TAU / n as f64
}

fn polar(cx: f64, cy: f64, r: f64, a: f64) -> (f64, f64) {
    (cx + r * a.cos(), cy + r * a.sin())
}

#[allow(clippy::too_many_arguments)]
fn draw(
    cr: &gtk::cairo::Context,
    w: f64,
    h: f64,
    values: &[f64; 6],
    texts: &[String; 6],
    grow: f64,
    hovered: Option<usize>,
    hitboxes: &mut [HitBox; 6],
) {
    let n = values.len();
    let cx = w / 2.0;
    // The two label lines of a top/bottom vertex are drawn *above* their vertex,
    // so the ring needs room up there but only a little air underneath. Centring
    // it with the same band on both sides left ~50 px of nothing below the chart,
    // which is what pushed the version line away from it.
    const LABEL_TOP: f64 = 30.0;
    const LABEL_BOTTOM: f64 = 18.0;
    // Side labels need their width too: never let the ring reach the card edge.
    let r = (w / 2.0 - 96.0)
        .min((h - LABEL_TOP - LABEL_BOTTOM) / 2.0)
        .max(40.0);
    let cy = h - LABEL_BOTTOM - r;

    // ------------------------------------------------------------- grid -----
    cr.set_line_width(1.0);
    for k in [0.25f64, 0.5, 0.75, 1.0] {
        cr.set_source_rgba(1.0, 1.0, 1.0, if k == 1.0 { 0.13 } else { 0.065 });
        for i in 0..n {
            let (x, y) = polar(cx, cy, r * k, axis_angle(i, n));
            if i == 0 {
                cr.move_to(x, y);
            } else {
                cr.line_to(x, y);
            }
        }
        cr.close_path();
        cr.stroke().ok();
    }

    for i in 0..n {
        let hot = hovered == Some(i);
        let (x, y) = polar(cx, cy, r, axis_angle(i, n));
        cr.set_source_rgba(1.0, 1.0, 1.0, if hot { 0.20 } else { 0.07 });
        cr.set_line_width(if hot { 1.5 } else { 1.0 });
        cr.move_to(cx, cy);
        cr.line_to(x, y);
        cr.stroke().ok();
    }

    // ------------------------------------------------------------- shape ----
    let eased = grow.clamp(0.0, 1.0);
    let pts: Vec<(f64, f64)> = (0..n)
        .map(|i| {
            polar(
                cx,
                cy,
                r * values[i].clamp(0.0, 1.0) * eased,
                axis_angle(i, n),
            )
        })
        .collect();

    cr.move_to(pts[0].0, pts[0].1);
    for p in &pts[1..] {
        cr.line_to(p.0, p.1);
    }
    cr.close_path();
    cr.set_source_rgba(0.42, 0.62, 1.0, 0.18);
    cr.fill_preserve().ok();
    cr.set_source_rgba(0.55, 0.72, 1.0, 0.90);
    cr.set_line_width(2.0);
    cr.set_line_join(gtk::cairo::LineJoin::Round);
    cr.stroke().ok();

    // ------------------------------------------------------ vertex dots -----
    for (i, p) in pts.iter().enumerate() {
        let c = COLORS[i];
        let hot = hovered == Some(i);
        cr.set_source_rgba(c.0, c.1, c.2, if hot { 0.28 } else { 0.16 });
        cr.arc(
            p.0,
            p.1,
            if hot { 11.0 } else { 7.5 },
            0.0,
            std::f64::consts::TAU,
        );
        cr.fill().ok();
        cr.set_source_rgba(c.0, c.1, c.2, 1.0);
        cr.arc(
            p.0,
            p.1,
            if hot { 5.0 } else { 3.4 },
            0.0,
            std::f64::consts::TAU,
        );
        cr.fill().ok();
    }

    cr.set_source_rgba(1.0, 1.0, 1.0, 0.25);
    cr.arc(cx, cy, 2.0, 0.0, std::f64::consts::TAU);
    cr.fill().ok();

    // ----------------------------------------------------------- labels -----
    for (i, slot) in hitboxes.iter_mut().enumerate().take(n) {
        let a = axis_angle(i, n);
        let (vx, vy) = polar(cx, cy, r, a);
        let (align, x, y) = if a.sin().abs() < 0.35 {
            // top / bottom: stack the block outside the ring
            let y = if a.sin() < 0.0 { vy - 34.0 } else { vy + 8.0 };
            (Align::Center, vx, y)
        } else if a.cos() > 0.0 {
            (Align::Left, vx + 12.0, vy - 20.0)
        } else {
            (Align::Right, vx - 12.0, vy - 20.0)
        };
        *slot = label_block(cr, x, y, align, i, texts, hovered == Some(i));
    }
}

#[derive(Clone, Copy)]
enum Align {
    Left,
    Center,
    Right,
}

fn label_block(
    cr: &gtk::cairo::Context,
    x: f64,
    y: f64,
    align: Align,
    i: usize,
    texts: &[String; 6],
    hot: bool,
) -> HitBox {
    let c = COLORS[i];
    // The hit target is the widest of the two lines plus a little air, so the
    // pointer does not have to land on a glyph.
    let width = text_width(cr, TITLES[i], 9.5).max(text_width(cr, &texts[i], 15.5));
    let (x0, x1) = match align {
        Align::Left => (x, x + width),
        Align::Center => (x - width / 2.0, x + width / 2.0),
        Align::Right => (x - width, x),
    };
    let (y0, y1) = (y - 4.0, y + 32.0);
    let pad = 6.0;

    if hot {
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.07);
        rounded_rect(cr, x0 - pad, y0, (x1 - x0) + pad * 2.0, y1 - y0, 6.0);
        cr.fill().ok();
    }

    text(
        cr,
        gtk::cairo::FontWeight::Bold,
        x,
        y + 9.0,
        align,
        TITLES[i],
        9.5,
        (c.0 * 0.8 + 0.2, c.1 * 0.8 + 0.2, c.2 * 0.8 + 0.2, 0.92),
    );
    text(
        cr,
        gtk::cairo::FontWeight::Bold,
        x,
        y + 25.0,
        align,
        &texts[i],
        15.5,
        (1.0, 1.0, 1.0, 0.96),
    );

    HitBox {
        x0: x0 - pad,
        y0,
        x1: x1 + pad,
        y1,
    }
}

fn text_width(cr: &gtk::cairo::Context, s: &str, size: f64) -> f64 {
    cr.select_font_face(
        "Sans",
        gtk::cairo::FontSlant::Normal,
        gtk::cairo::FontWeight::Bold,
    );
    cr.set_font_size(size);
    cr.text_extents(s).map(|e| e.width()).unwrap_or(0.0)
}

/// Rounded rectangle path, so the hover highlight matches the card language.
fn rounded_rect(cr: &gtk::cairo::Context, x: f64, y: f64, w: f64, h: f64, radius: f64) {
    let r = radius.min(w / 2.0).min(h / 2.0).max(0.0);
    let pi = std::f64::consts::PI;
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -pi / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, pi / 2.0);
    cr.arc(x + r, y + h - r, r, pi / 2.0, pi);
    cr.arc(x + r, y + r, r, pi, 1.5 * pi);
    cr.close_path();
}

#[allow(clippy::too_many_arguments)]
fn text(
    cr: &gtk::cairo::Context,
    weight: gtk::cairo::FontWeight,
    x: f64,
    y: f64,
    align: Align,
    s: &str,
    size: f64,
    rgba: (f64, f64, f64, f64),
) {
    cr.select_font_face("Sans", gtk::cairo::FontSlant::Normal, weight);
    cr.set_font_size(size);
    let Ok(ext) = cr.text_extents(s) else { return };
    let x = match align {
        Align::Left => x,
        Align::Center => x - ext.width() / 2.0 - ext.x_bearing(),
        Align::Right => x - ext.width() - ext.x_bearing(),
    };
    cr.set_source_rgba(rgba.0, rgba.1, rgba.2, rgba.3);
    cr.move_to(x, y);
    cr.show_text(s).ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axes_stay_inside_the_unit_circle() {
        for pct in [0.0, 0.3, 3.0, 50.0, 100.0, 250.0, 1600.0] {
            for cap in [100.0, 1600.0] {
                let n = cpu_norm(pct, cap);
                assert!((0.0..=1.0).contains(&n), "cpu_norm({pct}, {cap}) = {n}");
            }
        }
        for mib in [0.0, 120.0, 2048.0, 4096.0, 30720.0] {
            for total in [2048.0, 30720.0] {
                let n = ram_norm(mib, total);
                assert!((0.0..=1.0).contains(&n), "ram_norm({mib}, {total}) = {n}");
            }
        }
        for ms in [0.0, 0.5, 8.3, 16.7, 40.0] {
            for hz in [0.0, 60.0, 120.0] {
                let n = render_cost_norm(ms, hz);
                assert!(
                    (0.0..=1.0).contains(&n),
                    "render_cost_norm({ms}, {hz}) = {n}"
                );
            }
        }
        for share in [-0.5, 0.0, 0.73, 1.0, 2.0] {
            let n = pacing_norm(share);
            assert!((0.0..=1.0).contains(&n), "pacing_norm({share}) = {n}");
        }
    }

    #[test]
    fn zero_capacity_falls_back_instead_of_dividing_by_zero() {
        assert!(ram_norm(500.0, 0.0).is_finite());
        assert!(cpu_norm(5.0, 0.0).is_finite());
        assert!(render_cost_norm(5.0, 0.0).is_finite());
    }

    #[test]
    fn further_out_is_better() {
        assert!(ram_norm(100.0, 30720.0) > ram_norm(900.0, 30720.0));
        assert!(cpu_norm(0.5, 100.0) > cpu_norm(5.0, 100.0));
        assert!(cpu_norm(5.0, 100.0) > cpu_norm(25.0, 100.0));
        assert!(pacing_norm(0.99) > pacing_norm(0.73));
        assert!(render_cost_norm(2.0, 60.0) > render_cost_norm(10.0, 60.0));
    }

    #[test]
    fn scale_follows_the_machine() {
        // the same footprint plots further out on a bigger machine
        assert!(ram_norm(900.0, 30720.0) > ram_norm(900.0, 4096.0));
        // the full capacity pins to the centre
        assert!((cpu_norm(100.0, 100.0) - 0.0).abs() < 1e-9);
        assert!((ram_norm(30720.0, 30720.0) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn render_cost_is_measured_against_the_observed_cadence() {
        // a whole frame period of CPU per frame is the centre, at any cadence
        assert!(render_cost_norm(1000.0 / 60.0, 60.0) < 1e-9);
        assert!(render_cost_norm(1000.0 / 120.0, 120.0) < 1e-9);
        // and no CPU per frame is the outer ring
        assert!((render_cost_norm(0.0, 60.0) - 1.0).abs() < 1e-9);
        // the same ms/frame is cheaper at a lower cadence
        assert!(render_cost_norm(8.0, 60.0) > render_cost_norm(8.0, 120.0));
    }

    #[test]
    fn pacing_does_not_punish_a_60hz_cadence_on_a_120hz_panel() {
        // 60 fps at a 60 Hz cadence delivered every frame is a full axis;
        // fps / panel_refresh would have plotted 0.5
        assert!((pacing_norm(1.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn hit_testing_finds_the_label_under_the_pointer() {
        let mut boxes = [HitBox::default(); 6];
        boxes[2] = HitBox {
            x0: 10.0,
            y0: 20.0,
            x1: 30.0,
            y1: 40.0,
        };
        assert_eq!(hit_index(&boxes, 20.0, 30.0), Some(2));
        assert_eq!(hit_index(&boxes, 9.9, 30.0), None);
        assert_eq!(hit_index(&boxes, 20.0, 40.1), None);
        // an untouched chart has no hit targets at all
        assert_eq!(hit_index(&[HitBox::default(); 6], 0.0, 0.0), None);
    }

    #[test]
    fn every_axis_says_which_direction_is_better() {
        for help in HELP {
            assert!(help.contains("Higher is better"), "{help}");
        }
    }
}
