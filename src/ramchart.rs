//! Where the machine's memory was when the window opened.
//!
//! One horizontal bar, cut into one stripe per process name — every thread and
//! child sharing a name is summed into one stripe — with a grey `Others` stripe
//! for the names below the cut. The legend under the bar names each stripe and
//! carries its share, so nothing has to be read off an axis.
//!
//! A snapshot taken once at launch, not a timeline: nothing here moves while the
//! benchmark runs, which is the point. The bars are RSS from `/proc/<pid>/status`,
//! readable for every process on the machine, unlike `smaps_rollup`; the number is
//! "resident under this name", not "freed if it quit".

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;

/// Names that get their own stripe; everything smaller is summed into `Others`.
pub const TOP: usize = 9;

type Color = (f64, f64, f64);

const COLORS: [Color; TOP] = [
    (0.42, 0.62, 1.00), // blue
    (0.35, 0.85, 0.60), // green
    (0.20, 0.78, 0.64), // teal
    (0.72, 0.55, 1.00), // violet
    (0.96, 0.72, 0.30), // amber
    (0.96, 0.46, 0.46), // red
    (0.36, 0.82, 0.94), // cyan
    (0.86, 0.55, 0.92), // orchid
    (0.78, 0.86, 0.36), // lime
];
/// The remainder is not a category, it is a leftover: no colour claims it.
const OTHERS: Color = (0.52, 0.55, 0.62);

pub struct Chart {
    pub root: gtk::Box,
}

impl Chart {
    /// `items` are (process name, resident MiB) already grouped and sorted
    /// largest first; see [`crate::proc::ram_by_name`].
    pub fn new(items: &[(String, f64)]) -> Self {
        let rows = rows(items);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 7);

        // ------------------------------------------------------------- bar ---
        let bar = gtk::DrawingArea::new();
        bar.set_content_height(22);
        bar.set_hexpand(true);
        bar.set_halign(gtk::Align::Fill);
        bar.set_cursor_from_name(Some("pointer"));

        let shares = shares(&rows);
        // which stripe the click landed on; `None` means "show the bar plain"
        let picked: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));
        // the tags are built before the bar is wired, so the click handler can
        // hold them directly instead of hunting them down in the FlowBox
        let chips: Rc<Vec<gtk::Label>> = Rc::new(rows.iter().map(chip).collect());

        {
            let shares = shares.clone();
            let picked = picked.clone();
            let chips = chips.clone();
            let area = bar.clone();
            let click = gtk::GestureClick::new();
            click.connect_released(move |_, _, x, _| {
                let next = stripe_at(&shares, x, area.width() as f64);
                // clicking the same stripe again clears the highlight
                let next = if picked.get() == next { None } else { next };
                picked.set(next);
                for (i, chip) in chips.iter().enumerate() {
                    if Some(i) == next {
                        chip.add_css_class("on");
                    } else {
                        chip.remove_css_class("on");
                    }
                }
                area.queue_draw();
            });
            bar.add_controller(click);
        }
        {
            let shares = shares.clone();
            let picked = picked.clone();
            bar.set_draw_func(move |_, cr, w, h| {
                draw_bar(cr, w as f64, h as f64, &shares, picked.get());
            });
        }
        root.append(&bar);

        // ---------------------------------------------------------- legend ---
        // A FlowBox, not a fixed grid: ten tags of different widths reflow into
        // as many rows as the panel is wide.
        let legend = gtk::FlowBox::new();
        legend.set_selection_mode(gtk::SelectionMode::None);
        legend.set_homogeneous(false);
        legend.set_min_children_per_line(1);
        legend.set_max_children_per_line(TOP as u32 + 1);
        legend.set_column_spacing(6);
        legend.set_row_spacing(4);
        legend.set_valign(gtk::Align::Start);
        legend.set_halign(gtk::Align::Start);
        for chip in chips.iter() {
            legend.insert(chip, -1);
        }
        root.append(&legend);

        Self { root }
    }
}

struct Row {
    name: String,
    mib: f64,
    color: Color,
    /// the grey remainder, drawn dimmer than a named process
    others: bool,
}

/// The leading names keep their stripe; the tail is added up.
fn split(items: &[(String, f64)]) -> (Vec<(String, f64)>, f64) {
    if items.len() <= TOP {
        return (items.to_vec(), 0.0);
    }
    let named = items[..TOP].to_vec();
    let others = items[TOP..].iter().map(|(_, mib)| *mib).sum();
    (named, others)
}

fn rows(items: &[(String, f64)]) -> Vec<Row> {
    let (named, others) = split(items);
    let mut rows: Vec<Row> = named
        .into_iter()
        .enumerate()
        .map(|(i, (name, mib))| Row {
            name,
            mib,
            color: COLORS[i % TOP],
            others: false,
        })
        .collect();
    if others > 0.0 {
        rows.push(Row {
            name: "Others".to_string(),
            mib: others,
            color: OTHERS,
            others: true,
        });
    }
    rows
}

/// Each stripe's width as a fraction of the bar, left to right.
fn shares(rows: &[Row]) -> Vec<(f64, Color, bool)> {
    let total: f64 = rows.iter().map(|r| r.mib).sum();
    if total <= 0.0 {
        return Vec::new();
    }
    rows.iter()
        .map(|r| (r.mib / total, r.color, r.others))
        .collect()
}

/// Which stripe sits at `x`, in the same left-to-right order they are drawn.
fn stripe_at(shares: &[(f64, Color, bool)], x: f64, w: f64) -> Option<usize> {
    if shares.is_empty() || w <= 0.0 || x < 0.0 || x > w {
        return None;
    }
    let mut edge = 0.0;
    for (i, (frac, _, _)) in shares.iter().enumerate() {
        edge += frac * w;
        if x <= edge {
            return Some(i);
        }
    }
    // rounding can leave a hairline past the last computed edge
    Some(shares.len() - 1)
}

fn mib_text(mib: f64) -> String {
    if mib >= 1024.0 {
        format!("{:.1} GiB", mib / 1024.0)
    } else {
        format!("{mib:.0} MiB")
    }
}

fn hex(c: Color) -> String {
    let to = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", to(c.0), to(c.1), to(c.2))
}

/// One legend tag: the stripe's colour, the name, its share. The number lives in
/// the tag so the bar itself stays free of text.
fn chip(row: &Row) -> gtk::Label {
    let name = glib::markup_escape_text(&row.name);
    let label = gtk::Label::new(None);
    let name_color = if row.others { "#9aa3b2" } else { "#ffffff" };
    label.set_markup(&format!(
        "<span foreground=\"{}\">\u{25a0}</span> <span foreground=\"{name_color}\">{name}</span> \
         <span foreground=\"#939bab\">{}</span>",
        hex(row.color),
        mib_text(row.mib)
    ));
    label.add_css_class("ram-item");
    label.set_valign(gtk::Align::Center);
    label
}

fn draw_bar(
    cr: &gtk::cairo::Context,
    w: f64,
    h: f64,
    shares: &[(f64, Color, bool)],
    picked: Option<usize>,
) {
    if shares.is_empty() || w < 8.0 || h < 4.0 {
        return;
    }
    let radius = (h / 2.0).min(5.0);

    // Stripe edges are square, the silhouette is not: clip the whole bar to a
    // rounded rectangle and paint each stripe inside it.
    rounded(cr, 0.0, 0.0, w, h, radius);
    cr.clip();

    let gap = 1.5;
    let mut x = 0.0;
    for (i, (frac, color, others)) in shares.iter().enumerate() {
        let sw = frac * w;
        let last = i + 1 == shares.len();
        // the last stripe takes the rounding error, so the bar ends flush
        let right = if last { w } else { x + (sw - gap).max(0.6) };
        // picking one stripe pushes the others back rather than recolouring them
        let alpha = match picked {
            Some(sel) if sel == i => 1.0,
            Some(_) => 0.22,
            None if *others => 0.70,
            None => 0.95,
        };
        cr.set_source_rgba(color.0, color.1, color.2, alpha);
        cr.rectangle(x, 0.0, (right - x).max(0.4), h);
        cr.fill().ok();
        x += sw;
    }

    cr.reset_clip();
    rounded(cr, 0.5, 0.5, w - 1.0, h - 1.0, radius);
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.10);
    cr.set_line_width(1.0);
    cr.stroke().ok();
}

fn rounded(cr: &gtk::cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(h / 2.0).min(w / 2.0).max(0.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -std::f64::consts::FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, std::f64::consts::FRAC_PI_2);
    cr.arc(
        x + r,
        y + h - r,
        r,
        std::f64::consts::FRAC_PI_2,
        std::f64::consts::PI,
    );
    cr.arc(
        x + r,
        y + r,
        r,
        std::f64::consts::PI,
        std::f64::consts::FRAC_PI_2 * 3.0,
    );
    cr.close_path();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(n: usize) -> Vec<(String, f64)> {
        (0..n)
            .map(|i| (format!("proc{i}"), 1000.0 - i as f64))
            .collect()
    }

    #[test]
    fn the_tail_becomes_one_grey_stripe() {
        let (named, others) = split(&items(12));
        assert_eq!(named.len(), TOP);
        assert_eq!(others, (1000.0 - 9.0) + (1000.0 - 10.0) + (1000.0 - 11.0));
        let rows = rows(&items(12));
        assert_eq!(rows.len(), TOP + 1);
        assert!(rows[TOP].others);
        assert_eq!(rows[TOP].name, "Others");
    }

    #[test]
    fn a_short_list_has_no_others_stripe() {
        let (named, others) = split(&items(3));
        assert_eq!(named.len(), 3);
        assert_eq!(others, 0.0);
        assert_eq!(rows(&items(3)).len(), 3);
        assert!(rows(&[]).is_empty());
    }

    #[test]
    fn stripes_fill_the_bar_exactly() {
        let rows = rows(&items(12));
        let stripes = shares(&rows);
        assert_eq!(stripes.len(), TOP + 1);
        let total: f64 = stripes.iter().map(|(frac, _, _)| frac).sum();
        assert!((total - 1.0).abs() < 1e-9, "shares sum to {total}");
        // and every named stripe gets its own colour, so the legend is readable
        for (i, (_, color, others)) in stripes.iter().take(TOP).enumerate() {
            assert!(!others);
            assert_eq!(*color, COLORS[i]);
        }
        assert!(stripes[TOP].2, "Others must be last");
        // an empty snapshot draws nothing rather than dividing by zero
        assert!(shares(&[]).is_empty());
    }

    #[test]
    fn a_click_finds_the_stripe_under_it() {
        let rows = rows(&items(12));
        let stripes = shares(&rows);
        let w = 1000.0;
        assert_eq!(stripe_at(&stripes, 0.0, w), Some(0));
        assert_eq!(stripe_at(&stripes, w, w), Some(stripes.len() - 1));
        // outside the bar, and a bar with nothing in it, select nothing
        assert_eq!(stripe_at(&stripes, w + 1.0, w), None);
        assert_eq!(stripe_at(&stripes, -1.0, w), None);
        assert_eq!(stripe_at(&stripes, 10.0, 0.0), None);
        assert_eq!(stripe_at(&[], 10.0, w), None);

        // every stripe is reachable at its own midpoint
        let mut edge = 0.0;
        for (i, (frac, _, _)) in stripes.iter().enumerate() {
            let mid = edge + frac * w / 2.0;
            assert_eq!(stripe_at(&stripes, mid, w), Some(i), "stripe {i}");
            edge += frac * w;
        }
    }
}
