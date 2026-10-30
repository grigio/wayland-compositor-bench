//! Six-axis radar chart for the post-run summary.
//!
//! Every axis is "further out is better" and every radius is measured against
//! what this particular machine can do, not against a hard-coded ceiling:
//!
//! - frame rate, relative to the panel refresh (`fps / target_hz`);
//! - protocol coverage, a percentage of the reference protocol list;
//! - memory before the test and its peak while it ran, against the machine's
//!   total RAM (`1 - ln(1 + MiB) / ln(1 + total)`), which keeps 80 MiB and
//!   1.2 GiB visibly apart instead of both hugging the outer ring;
//! - CPU idle before the test and its average while it ran, against the
//!   machine's full capacity (`threads * 100 %`), also logarithmically: the
//!   interesting idle range is 0.3–3 % and a linear axis would squash it into a
//!   pixel.
//!
//! The absolute number is printed at each vertex — that is what you quote.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;

/// Used when the machine cannot be probed, so the chart still draws something
/// sane instead of dividing by a zero-sized ceiling.
const FALLBACK_RAM_MIB: f64 = 2048.0;
const FALLBACK_CPU_PCT: f64 = 800.0;

/// The ceilings this machine offers, so a vertex reads as "share of what the
/// system has" rather than against an arbitrary constant.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// total RAM in MiB: 0 MiB of session plots at 1.0, all of it at 0.0
    pub total_ram_mib: f64,
    /// logical threads × 100 %: idle plots at 1.0, every core saturated at 0.0
    pub cpu_capacity_pct: f64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            total_ram_mib: FALLBACK_RAM_MIB,
            cpu_capacity_pct: FALLBACK_CPU_PCT,
        }
    }
}

const TITLES: [&str; 6] = [
    "FRAME RATE",
    "RAM BEFORE",
    "RAM PEAK",
    "FEATURES",
    "CPU IDLE",
    "CPU AVG",
];
const COLORS: [(f64, f64, f64); 6] = [
    (0.42, 0.62, 1.00), // blue    frame rate
    (0.35, 0.85, 0.60), // green   RAM before
    (0.20, 0.78, 0.64), // teal    RAM peak
    (0.72, 0.55, 1.00), // violet  features
    (0.96, 0.72, 0.30), // amber   CPU idle
    (0.96, 0.46, 0.46), // red     CPU average
];

/// What the chart plots. Everything is expressed in the unit you would quote.
#[derive(Clone, Copy, Debug, Default)]
pub struct Values {
    pub fps: f64,
    pub target_hz: f64,
    pub feat_pct: f64,
    pub ram_before_mib: f64,
    pub ram_peak_mib: f64,
    pub cpu_idle_pct: f64,
    pub cpu_avg_pct: f64,
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

        {
            let v = values.clone();
            let t = texts.clone();
            let p = shown.clone();
            area.set_draw_func(move |_, cr, w, h| {
                draw(cr, w as f64, h as f64, &v.borrow(), &t.borrow(), p.get());
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
        self.values.borrow_mut().copy_from_slice(&[
            fps_norm(v.fps, v.target_hz),
            ram_norm(v.ram_before_mib, l.total_ram_mib),
            ram_norm(v.ram_peak_mib, l.total_ram_mib),
            (v.feat_pct / 100.0).clamp(0.0, 1.0),
            cpu_norm(v.cpu_idle_pct, l.cpu_capacity_pct),
            cpu_norm(v.cpu_avg_pct, l.cpu_capacity_pct),
        ]);

        {
            let mut t = self.texts.borrow_mut();
            t[0] = if v.fps > 0.0 {
                format!("{:.0} fps", v.fps)
            } else {
                "—".to_string()
            };
            t[1] = crate::sysinfo::fmt_mib(v.ram_before_mib);
            t[2] = crate::sysinfo::fmt_mib(v.ram_peak_mib);
            t[3] = format!("{:.0}%", v.feat_pct);
            t[4] = crate::sysinfo::fmt_pct(v.cpu_idle_pct);
            t[5] = crate::sysinfo::fmt_pct(v.cpu_avg_pct);
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

fn fps_norm(fps: f64, target_hz: f64) -> f64 {
    if target_hz > 0.0 {
        (fps / target_hz).clamp(0.0, 1.0)
    } else {
        0.0
    }
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
        FALLBACK_CPU_PCT
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

fn draw(
    cr: &gtk::cairo::Context,
    w: f64,
    h: f64,
    values: &[f64; 6],
    texts: &[String; 6],
    grow: f64,
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

    cr.set_source_rgba(1.0, 1.0, 1.0, 0.07);
    for i in 0..n {
        let (x, y) = polar(cx, cy, r, axis_angle(i, n));
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
        cr.set_source_rgba(c.0, c.1, c.2, 0.16);
        cr.arc(p.0, p.1, 7.5, 0.0, std::f64::consts::TAU);
        cr.fill().ok();
        cr.set_source_rgba(c.0, c.1, c.2, 1.0);
        cr.arc(p.0, p.1, 3.4, 0.0, std::f64::consts::TAU);
        cr.fill().ok();
    }

    cr.set_source_rgba(1.0, 1.0, 1.0, 0.25);
    cr.arc(cx, cy, 2.0, 0.0, std::f64::consts::TAU);
    cr.fill().ok();

    // ----------------------------------------------------------- labels -----
    for i in 0..n {
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
        label_block(cr, x, y, align, i, texts);
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
) {
    let c = COLORS[i];
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
        for pct in [0.0, 0.3, 3.0, 50.0, 250.0, 1600.0] {
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
    }

    #[test]
    fn zero_capacity_falls_back_instead_of_dividing_by_zero() {
        assert!(ram_norm(500.0, 0.0).is_finite());
        assert!(cpu_norm(5.0, 0.0).is_finite());
    }

    #[test]
    fn further_out_is_better() {
        assert!(ram_norm(100.0, 30720.0) > ram_norm(900.0, 30720.0));
        assert!(cpu_norm(0.5, 1600.0) > cpu_norm(5.0, 1600.0));
        assert!(cpu_norm(5.0, 1600.0) > cpu_norm(25.0, 1600.0));
    }

    #[test]
    fn scale_follows_the_machine() {
        // the same footprint plots further out on a bigger machine
        assert!(ram_norm(900.0, 30720.0) > ram_norm(900.0, 4096.0));
        assert!(cpu_norm(25.0, 1600.0) > cpu_norm(25.0, 400.0));
        // and the full capacity pins to the centre
        assert!((cpu_norm(1600.0, 1600.0) - 0.0).abs() < 1e-9);
        assert!((ram_norm(30720.0, 30720.0) - 0.0).abs() < 1e-9);
    }
}
