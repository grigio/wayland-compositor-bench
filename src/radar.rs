//! Six-axis radar chart for the post-run summary.
//!
//! Every axis is "further out is better" and every radius is measured against
//! a common reference, not against the machine or the session's own cadence:
//!
//! - pacing: delivered frames as a share of the panel refresh, for one surface
//!   and for the surface plus N toplevels. A clean 60 fps on a 120 Hz panel is
//!   50 %, which is the same number a 60 Hz panel would give;
//! - render cost: compositor CPU over the frame test, scaled from the panel
//!   refresh to a common 60 Hz panel (`1 - ln(1 + CPU@60Hz %) / ln(101)`), so a
//!   120 Hz panel does not make the same work look cheaper, and 2 % and 8 % of
//!   a core stay visibly apart instead of both hugging the outer ring;
//! - memory: the stack's PSS against a fixed 8 GiB reference
//!   (`1 - ln(1 + MiB) / ln(1 + 8192)`), which keeps 50 MiB and 1.2 GiB
//!   visibly apart without rewarding a larger machine;
//! - CPU: stack CPU at rest as a share of one core, also logarithmically:
//!   the interesting idle range is 0.3–3 % and a linear axis would squash it
//!   into a pixel;
//! - features: protocol coverage over the scored tiers.
//!
//! The stack is the compositor plus any declared shell/panel processes; the
//! absolute number is printed at each vertex — that is what you quote. Because
//! three axes print a cost and plot its inverse, each vertex carries the
//! direction its own number should move (`↓` lower is better, `↑` higher is
//! better), so the chart never asks the reader to hold two conventions at once.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;

/// Fixed memory reference for the chart. It is deliberately not the machine's
/// RAM: the same stack should land in the same place on every machine.
const RAM_REFERENCE_MIB: f64 = 8192.0;

/// The fixed reference this chart normalizes against.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// memory reference in MiB: 0 MiB plots at 1.0, the reference at 0.0
    pub ram_reference_mib: f64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            ram_reference_mib: RAM_REFERENCE_MIB,
        }
    }
}

const TITLES: [&str; 6] = [
    "PACING",
    "LOAD PACING",
    "RENDER COST",
    "STACK RAM",
    "STACK CPU",
    "FEATURES",
];
const COLORS: [(f64, f64, f64); 6] = [
    (0.42, 0.62, 1.00), // blue    pacing, one surface
    (0.20, 0.78, 0.64), // teal    pacing under load
    (0.96, 0.72, 0.30), // amber   render cost
    (0.35, 0.85, 0.60), // green   stack RAM
    (0.96, 0.46, 0.46), // red     stack CPU
    (0.72, 0.55, 1.00), // violet  features
];

/// Which way the number under the vertex should move. The radius is always
/// "further out is better"; the cost axes plot the inverse of the printed
/// number, so the vertex has to say so itself instead of leaving the reader
/// with a cost labelled "higher is better".
const BETTER: [&str; 6] = ["↑", "↑", "↓", "↓", "↓", "↑"];

/// Width of the drawn direction marker. It is a path, not a glyph: the toy
/// font stack the chart draws with has no guaranteed arrow coverage, and a
/// missing glyph turns into a box on exactly the axes that need explaining.
const ARROW_W: f64 = 5.0;

/// Hover help: what each axis plots and which direction is good. The cost axes
/// print a number that is better when lower, and are drawn inverted so that
/// further out still means cheaper.
const HELP: [&str; 6] = [
    "Pacing: delivered frames as a share of the panel refresh, with one animated surface. Higher is better — 100 % means the panel was fed every refresh. A deliberate 60 fps on a 120 Hz panel is 50 %.",
    "Pacing under load: the same share with the extra toplevels open, which is what a real desktop does all day. Higher is better.",
    "Render cost: compositor CPU over the load test, scaled from the panel refresh to a common 60 Hz panel and shown as a share of one core, log-scaled so the 1–10 % range stays readable. Lower is better — the axis is inverted, so less CPU per panel refresh reaches further out.",
    "Stack memory: compositor plus declared shell/panel PSS at rest, log-scaled against a fixed 8 GiB reference. Lower is better — the axis is inverted, so a smaller footprint reaches further out.",
    "Stack CPU: compositor plus declared shell/panel CPU at rest as a share of one core, log-scaled. Lower is better — the axis is inverted, so a quieter desktop reaches further out.",
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
    /// delivered frames as a share of the panel refresh, 0..1
    pub pacing_single: f64,
    pub pacing_multi: f64,
    /// compositor CPU in the load test, scaled from the panel refresh to a
    /// common 60 Hz panel, % of one core
    pub comp_cpu_pct_at_60hz: f64,
    /// false when the render test produced no frames: a missing measurement
    /// must not plot as a compositor that costs nothing per frame
    pub render_known: bool,
    /// compositor plus declared shell/panel PSS at rest
    pub stack_pss_mib: f64,
    /// compositor plus declared shell/panel CPU at rest, % of one core
    pub stack_cpu_pct: f64,
    /// false when no compositor process was resolved: the two stack axes have
    /// no measurement, so they must not plot as zero cost
    pub known: bool,
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
        // compositor must not look like a desktop that costs nothing.
        let (stack_ram, stack_cpu) = if v.known {
            (
                ram_norm(v.stack_pss_mib, l.ram_reference_mib),
                cpu_norm(v.stack_cpu_pct, 100.0),
            )
        } else {
            (0.0, 0.0)
        };
        let render = if v.render_known {
            render_cost_norm(v.comp_cpu_pct_at_60hz)
        } else {
            0.0
        };
        self.values.borrow_mut().copy_from_slice(&[
            pacing_norm(v.pacing_single),
            pacing_norm(v.pacing_multi),
            render,
            stack_ram,
            stack_cpu,
            (v.feat_pct / 100.0).clamp(0.0, 1.0),
        ]);

        {
            let mut t = self.texts.borrow_mut();
            t[0] = pacing_text(v.pacing_single);
            t[1] = pacing_text(v.pacing_multi);
            t[2] = if v.render_known {
                crate::sysinfo::fmt_pct(v.comp_cpu_pct_at_60hz)
            } else {
                "—".to_string()
            };
            t[3] = if v.known {
                crate::sysinfo::fmt_mib(v.stack_pss_mib)
            } else {
                "—".to_string()
            };
            t[4] = if v.known {
                crate::sysinfo::fmt_pct(v.stack_cpu_pct)
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

/// `1.0` when the compositor uses no CPU, `0.0` when a 60 Hz panel would take
/// a whole core. Scaling the measured CPU by the panel rate is what keeps a
/// 120 Hz panel from making the same work look cheaper — including when the
/// client was deliberately throttled below the panel refresh.
///
/// Logarithmic for the same reason as the stack axes: a compositor lives in the
/// 1–10 % range, and a linear axis turns 2 % and 8 % into 6 % of the radius,
/// which is a pixel or two. Against a whole-core reference the log curve spreads
/// that range over a third of the chart.
fn render_cost_norm(cpu_pct_at_60hz: f64) -> f64 {
    cpu_norm(cpu_pct_at_60hz, 100.0)
}

fn ram_norm(mib: f64, reference_mib: f64) -> f64 {
    let reference = if reference_mib > 1.0 {
        reference_mib
    } else {
        RAM_REFERENCE_MIB
    };
    let ratio = (1.0 + mib.max(0.0)).ln() / (1.0 + reference).ln();
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
    // The title and its direction marker are laid out as one run; the hit
    // target is the widest of the two lines plus a little air, so the pointer
    // does not have to land on a glyph.
    let title_w = text_width(cr, TITLES[i], 9.5);
    let arrow_w = ARROW_W;
    let arrow_gap = 4.5;
    let width = (title_w + arrow_gap + arrow_w).max(text_width(cr, &texts[i], 15.5));
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

    // Title + marker, aligned as a unit so the arrow never drifts away from the
    // word it qualifies.
    let run_w = title_w + arrow_gap + arrow_w;
    let (title_x, arrow_x) = match align {
        Align::Left => (x, x + title_w + arrow_gap),
        Align::Center => {
            let start = x - run_w / 2.0;
            (start, start + title_w + arrow_gap)
        }
        Align::Right => (x - run_w, x - arrow_w),
    };
    text(
        cr,
        gtk::cairo::FontWeight::Bold,
        title_x,
        y + 9.0,
        Align::Left,
        TITLES[i],
        9.5,
        (c.0 * 0.8 + 0.2, c.1 * 0.8 + 0.2, c.2 * 0.8 + 0.2, 0.92),
    );
    direction_marker(
        cr,
        arrow_x,
        y + 9.0,
        BETTER[i] == "↑",
        (c.0 * 0.8 + 0.2, c.1 * 0.8 + 0.2, c.2 * 0.8 + 0.2, 0.75),
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

/// Small arrow next to an axis title, pointing the way its number should move.
/// Drawn as a shaft plus two head strokes so it matches the weight of the 9.5 pt
/// label instead of sitting on the baseline as a symbol glyph would.
fn direction_marker(
    cr: &gtk::cairo::Context,
    x: f64,
    baseline: f64,
    up: bool,
    rgba: (f64, f64, f64, f64),
) {
    let h = 7.0;
    let cx = x + ARROW_W / 2.0;
    let (head, tail) = if up {
        (baseline - h, baseline - 1.0)
    } else {
        (baseline - 1.0, baseline - h)
    };
    let barb = if up { 2.8 } else { -2.8 };
    cr.set_source_rgba(rgba.0, rgba.1, rgba.2, rgba.3);
    cr.set_line_width(1.3);
    cr.set_line_cap(gtk::cairo::LineCap::Round);
    cr.move_to(cx, tail);
    cr.line_to(cx, head);
    cr.move_to(x, head + barb);
    cr.line_to(cx, head);
    cr.line_to(x + ARROW_W, head + barb);
    cr.stroke().ok();
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
            for reference in [2048.0, 8192.0] {
                let n = ram_norm(mib, reference);
                assert!(
                    (0.0..=1.0).contains(&n),
                    "ram_norm({mib}, {reference}) = {n}"
                );
            }
        }
        for pct in [0.0, 0.5, 3.0, 6.0, 40.0, 100.0, 250.0] {
            let n = render_cost_norm(pct);
            assert!((0.0..=1.0).contains(&n), "render_cost_norm({pct}) = {n}");
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
        assert!(render_cost_norm(5.0).is_finite());
    }

    #[test]
    fn further_out_is_better() {
        assert!(ram_norm(100.0, 8192.0) > ram_norm(900.0, 8192.0));
        assert!(cpu_norm(0.5, 100.0) > cpu_norm(5.0, 100.0));
        assert!(cpu_norm(5.0, 100.0) > cpu_norm(25.0, 100.0));
        assert!(pacing_norm(0.99) > pacing_norm(0.73));
        assert!(render_cost_norm(2.0) > render_cost_norm(10.0));
    }

    #[test]
    fn the_memory_reference_is_fixed_not_the_machine() {
        // The chart always normalizes against the same 8 GiB constant, so the
        // vertex is about the stack, not about how much RAM the machine has.
        assert_eq!(Limits::default().ram_reference_mib, RAM_REFERENCE_MIB);
        // the full reference pins to the centre, a larger footprint further in
        assert!((ram_norm(RAM_REFERENCE_MIB, RAM_REFERENCE_MIB) - 0.0).abs() < 1e-9);
        assert!(ram_norm(900.0, RAM_REFERENCE_MIB) > ram_norm(2000.0, RAM_REFERENCE_MIB));
    }

    #[test]
    fn render_cost_is_a_share_of_one_core_at_60hz() {
        // no CPU is the outer ring; a whole core at 60 Hz is the centre
        assert!((render_cost_norm(0.0) - 1.0).abs() < 1e-9);
        assert!(render_cost_norm(100.0) < 1e-9);
        // less CPU at a 60 Hz panel plots further out
        assert!(render_cost_norm(3.0) > render_cost_norm(6.0));
    }

    #[test]
    fn the_render_axis_spreads_the_range_a_compositor_lives_in() {
        // A compositor sits in the low single digits. A linear axis gave 2 %
        // and 8 % only 6 % of the radius apart — two pixels, i.e. nothing.
        let spread = render_cost_norm(2.0) - render_cost_norm(8.0);
        assert!(spread > 0.20, "2 % vs 8 % only {spread:.3} of the radius");
        // and a genuinely expensive compositor still lands near the centre
        assert!(render_cost_norm(50.0) < 0.2);
        // the axis is monotonic, with no cliff in the middle
        let mut prev = f64::INFINITY;
        for pct in [0.0, 0.3, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0, 100.0] {
            let n = render_cost_norm(pct);
            assert!(
                n < prev,
                "{pct} % did not plot further in than the step before"
            );
            prev = n;
        }
    }

    #[test]
    fn pacing_is_a_share_of_the_panel_refresh() {
        // 60 fps on a 120 Hz panel is half the panel, and that is the point:
        // the same number a 60 Hz panel would give for 30 fps.
        assert!((pacing_norm(0.5) - 0.5).abs() < 1e-9);
        assert!((pacing_norm(1.0) - 1.0).abs() < 1e-9);
        assert!(pacing_norm(1.0) > pacing_norm(0.5));
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
        for (i, help) in HELP.iter().enumerate() {
            let expected = if BETTER[i] == "↓" {
                "Lower is better"
            } else {
                "Higher is better"
            };
            assert!(help.contains(expected), "{help}");
        }
    }

    #[test]
    fn the_cost_axes_are_the_inverted_ones() {
        // A cost is better when lower, so these three print a number whose
        // good direction is down even though their vertex grows outwards.
        for i in [2, 3, 4] {
            assert_eq!(BETTER[i], "↓", "axis {} ({})", i, TITLES[i]);
            assert!(HELP[i].contains("inverted"), "{}", HELP[i]);
        }
        for i in [0, 1, 5] {
            assert_eq!(BETTER[i], "↑", "axis {} ({})", i, TITLES[i]);
        }
    }
}
