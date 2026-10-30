//! The strip under the radar: how hard the session's CPU ran while the
//! benchmark was running.
//!
//! Same samples the radar averages into its "CPU avg" vertex — one point every
//! ~100 ms from the peak sampler — drawn as a shape so the idle baseline, the
//! frame tests and the window-open spike can be told apart.
//!
//! The axis is a plain percentage of one core: 0–100 %. A 16-thread machine can
//! spend more than one core, and it usually does during a frame test, so the
//! ceiling only grows — in round hundreds — when the run really went past
//! 100 %. Clipping there would turn the whole measurement into a flat line at
//! the top and hide the thing the chart exists to show.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;

/// Reveal animation length. Long enough to read as "this just happened, left to
/// right", short enough not to sit through.
const REVEAL_S: f64 = 0.65;
/// Below this many pixels between two phase boundaries, the label would collide
/// with the next one, so only the line is drawn.
const LABEL_ROOM: f64 = 44.0;

pub struct Chart {
    pub area: gtk::DrawingArea,
    points: Rc<RefCell<Vec<(f32, f32)>>>,
    marks: Rc<RefCell<Vec<(f32, String)>>>,
    /// 0..1 reveal, so the line sweeps in instead of appearing at once
    reveal: Rc<Cell<f64>>,
    generation: Rc<Cell<u64>>,
}

impl Chart {
    pub fn new() -> Self {
        let area = gtk::DrawingArea::new();
        area.set_content_height(78);
        area.set_content_width(320);
        area.set_hexpand(true);
        area.set_halign(gtk::Align::Fill);
        area.set_valign(gtk::Align::Center);
        area.set_margin_top(4);

        let points: Rc<RefCell<Vec<(f32, f32)>>> = Rc::new(RefCell::new(Vec::new()));
        let marks: Rc<RefCell<Vec<(f32, String)>>> = Rc::new(RefCell::new(Vec::new()));
        let reveal = Rc::new(Cell::new(1.0f64));

        {
            let p = points.clone();
            let m = marks.clone();
            let r = reveal.clone();
            area.set_draw_func(move |_, cr, w, h| {
                draw(cr, w as f64, h as f64, &p.borrow(), &m.borrow(), r.get());
            });
        }

        Self {
            area,
            points,
            marks,
            reveal,
            generation: Rc::new(Cell::new(0)),
        }
    }

    /// `points` are (seconds since the run started, CPU % of one core) and
    /// `marks` are (seconds, phase name) boundaries drawn as hairlines.
    pub fn set(&self, points: &[(f32, f32)], marks: &[(f32, &'static str)]) {
        *self.points.borrow_mut() = points.to_vec();
        *self.marks.borrow_mut() = marks
            .iter()
            .map(|(t, name)| (*t, (*name).to_string()))
            .collect();

        let gen = self.generation.get().wrapping_add(1);
        self.generation.set(gen);
        self.reveal.set(0.0);

        let area = self.area.clone();
        let reveal = self.reveal.clone();
        let generation = self.generation.clone();
        let t0 = Instant::now();
        glib::timeout_add_local(Duration::from_millis(16), move || {
            if generation.get() != gen {
                return glib::ControlFlow::Break;
            }
            let p = (t0.elapsed().as_secs_f64() / REVEAL_S).min(1.0);
            reveal.set(1.0 - (1.0 - p).powi(3));
            area.queue_draw();
            if p < 1.0 {
                glib::ControlFlow::Continue
            } else {
                glib::ControlFlow::Break
            }
        });
    }
}

impl Default for Chart {
    fn default() -> Self {
        Self::new()
    }
}

/// Ticks stay at 0–100 % until the run actually saturates more than one core.
fn ceiling(peak: f64) -> f64 {
    if peak <= 100.0 {
        100.0
    } else {
        (peak / 100.0).ceil() * 100.0
    }
}

fn draw(
    cr: &gtk::cairo::Context,
    w: f64,
    h: f64,
    points: &[(f32, f32)],
    marks: &[(f32, String)],
    reveal: f64,
) {
    // Left gutter holds the percentage ticks; the bottom strip the time codes.
    let x0 = 30.0;
    let x1 = (w - 8.0).max(x0 + 10.0);
    let y0 = 9.0;
    let y1 = (h - 13.0).max(y0 + 10.0);

    let span = points.last().map(|p| p.0 as f64).unwrap_or(0.0).max(1.0);
    let peak = points.iter().map(|p| p.1 as f64).fold(0.0, f64::max);
    let top = ceiling(peak);

    let y = |v: f64| y1 - (v / top).clamp(0.0, 1.0) * (y1 - y0);
    let x = |t: f64| x0 + (t / span).clamp(0.0, 1.0) * (x1 - x0);

    // ------------------------------------------------------------- grid -----
    for k in [0.0, 0.5, 1.0] {
        let gy = y(top * k);
        cr.set_line_width(1.0);
        cr.set_source_rgba(1.0, 1.0, 1.0, if k == 0.0 { 0.10 } else { 0.05 });
        cr.move_to(x0, gy);
        cr.line_to(x1, gy);
        cr.stroke().ok();
        text(
            cr,
            x0 - 5.0,
            gy + 2.6,
            Align::Right,
            &format!("{:.0}%", top * k),
            (1.0, 1.0, 1.0, 0.32),
        );
    }

    // ------------------------------------------------------------- phases ---
    let xs: Vec<f64> = marks.iter().map(|(t, _)| x(*t as f64)).collect();
    for (i, (_, name)) in marks.iter().enumerate() {
        let mx = xs[i];
        cr.set_dash(&[2.0, 3.0], 0.0);
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.07);
        cr.move_to(mx, y0);
        cr.line_to(mx, y1);
        cr.stroke().ok();
        cr.set_dash(&[], 0.0);
        let limit = xs.get(i + 1).copied().unwrap_or(x1);
        if limit - mx >= LABEL_ROOM {
            text(
                cr,
                mx + 3.0,
                y0 + 7.5,
                Align::Left,
                name,
                (1.0, 1.0, 1.0, 0.30),
            );
        }
    }

    // ------------------------------------------------------------- shape ----
    if points.len() >= 2 {
        let reveal = reveal.clamp(0.0, 1.0);
        let reach = x0 + (x1 - x0) * reveal;
        cr.save().ok();
        cr.rectangle(x0 - 1.0, y0 - 3.0, reach - x0 + 1.0, y1 - y0 + 6.0);
        cr.clip();

        // filled area, fading out towards the baseline
        cr.move_to(x(points[0].0 as f64), y1);
        for (t, v) in points {
            cr.line_to(x(*t as f64), y(*v as f64));
        }
        cr.line_to(x(points[points.len() - 1].0 as f64), y1);
        cr.close_path();
        let grad = gtk::cairo::LinearGradient::new(0.0, y0, 0.0, y1);
        grad.add_color_stop_rgba(0.0, 0.42, 0.62, 1.0, 0.30);
        grad.add_color_stop_rgba(1.0, 0.42, 0.62, 1.0, 0.02);
        cr.set_source(&grad).ok();
        cr.fill().ok();

        cr.move_to(x(points[0].0 as f64), y(points[0].1 as f64));
        for (t, v) in points.iter().skip(1) {
            cr.line_to(x(*t as f64), y(*v as f64));
        }
        cr.set_source_rgba(0.55, 0.72, 1.0, 0.95);
        cr.set_line_width(1.8);
        cr.set_line_join(gtk::cairo::LineJoin::Round);
        cr.stroke().ok();
        cr.restore().ok();

        // the run's single worst moment, in the same red as the radar's CPU axis
        if let Some((pt, pv)) = points
            .iter()
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        {
            let px = x(*pt as f64);
            if px <= reach + 0.5 {
                cr.set_source_rgba(0.96, 0.46, 0.46, 0.20);
                cr.arc(px, y(*pv as f64), 6.0, 0.0, std::f64::consts::TAU);
                cr.fill().ok();
                cr.set_source_rgba(0.96, 0.46, 0.46, 1.0);
                cr.arc(px, y(*pv as f64), 2.6, 0.0, std::f64::consts::TAU);
                cr.fill().ok();
            }
        }
    }

    // --------------------------------------------------------------- axis ---
    text(cr, x0, h - 3.0, Align::Left, "0 s", (1.0, 1.0, 1.0, 0.26));
    text(
        cr,
        x1,
        h - 3.0,
        Align::Right,
        &format!("{span:.0} s"),
        (1.0, 1.0, 1.0, 0.26),
    );
}

#[derive(Clone, Copy)]
enum Align {
    Left,
    Right,
}

fn text(
    cr: &gtk::cairo::Context,
    x: f64,
    y: f64,
    align: Align,
    s: &str,
    rgba: (f64, f64, f64, f64),
) {
    cr.select_font_face(
        "Sans",
        gtk::cairo::FontSlant::Normal,
        gtk::cairo::FontWeight::Normal,
    );
    cr.set_font_size(7.5);
    let Ok(ext) = cr.text_extents(s) else { return };
    let x = match align {
        Align::Left => x,
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
    fn ticks_stay_at_100_until_the_run_needs_more() {
        assert!((ceiling(0.0) - 100.0).abs() < 1e-9);
        assert!((ceiling(46.0) - 100.0).abs() < 1e-9);
        assert!((ceiling(100.0) - 100.0).abs() < 1e-9);
        assert!((ceiling(100.4) - 200.0).abs() < 1e-9);
        assert!((ceiling(182.0) - 200.0).abs() < 1e-9);
        assert!((ceiling(460.0) - 500.0).abs() < 1e-9);
    }
}
