//! Command line: what to measure, where to write it, and how to read it back.

use std::path::PathBuf;

pub const USAGE: &str = "\
wayland-compositor-bench — measure the Wayland session you are logged into

USAGE:
    wayland-compositor-bench                        open the window, press Start
    wayland-compositor-bench --run [--json [FILE]]  start measuring as soon as it is up
    wayland-compositor-bench --report [PATH…]       print medians of saved runs
    wayland-compositor-bench --compare A B [PATH…]  two labels side by side, with noise floor
    wayland-compositor-bench --check                can this session be measured reliably?
    wayland-compositor-bench --help

OPTIONS:
    --run               run the benchmark ~1 s after the window appears
    --json [FILE]       write the finished run to FILE (default:
                        $XDG_STATE_HOME/wayland-compositor-bench/results/
                        <label>-<timestamp>.json)
    --label NAME        name the run; defaults to the running desktop
                        (XDG_CURRENT_DESKTOP, unless it contradicts the
                        compositor process)
    --settle SECONDS    max time to wait for the session to go quiet (default 45)
    --idle SECONDS      idle sampling window (default 20)
    --frames SECONDS    length of each frame test (default 6)
    --windows N         extra toplevels (default 6)
    --report [PATH…]    aggregate results: a directory, JSON files, or
                        label substrings; no PATH means the XDG results
                        directory plus ./results
    --compare A B [PATH…]
                        compare two saved labels, reading PATH like --report
    --check             probe the session and print what is measurable;
                        exits 2 when it is measurable only with caveats
    -h, --help          this text
    -V, --version       print the version

ENVIRONMENT:
    WG_BENCH_AUTORUN=1  same as --run
    WG_BENCH_PRINT=1    echo every result row to stdout
    WG_BENCH_NO_SUDO=1  never use `sudo -n` for root-only RAPL counters
";

/// Where the finished run goes.
#[derive(Clone, Debug)]
pub enum JsonTarget {
    /// `$XDG_STATE_HOME/wayland-compositor-bench/results/<label>-<timestamp>.json`.
    Auto,
    Path(PathBuf),
}

#[derive(Clone, Debug)]
pub struct Options {
    pub run: bool,
    pub json: Option<JsonTarget>,
    pub report: bool,
    pub check: bool,
    pub compare: Option<(String, String)>,
    /// files or directories for `--report`; empty means `results/`
    pub files: Vec<PathBuf>,
    pub label: Option<String>,
    pub settle_s: f64,
    pub idle_s: f64,
    pub frames_s: f64,
    pub windows: u32,
    pub help: bool,
    pub version: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            run: std::env::var("WG_BENCH_AUTORUN").is_ok(),
            json: None,
            report: false,
            check: false,
            compare: None,
            files: Vec::new(),
            label: None,
            settle_s: 45.0,
            idle_s: 20.0,
            frames_s: 6.0,
            windows: 6,
            help: false,
            version: false,
        }
    }
}

impl Options {
    pub fn parse<I: Iterator<Item = String>>(args: I) -> Result<Self, String> {
        let mut o = Self::default();
        let mut it = args.peekable();

        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--run" => o.run = true,
                "--report" => o.report = true,
                "--check" => o.check = true,
                "-h" | "--help" => o.help = true,
                "-V" | "--version" => o.version = true,
                "--compare" => {
                    let a = it.next().ok_or("--compare needs two labels")?;
                    let b = it.next().ok_or("--compare needs two labels")?;
                    o.compare = Some((a, b));
                }
                "--json" => {
                    // the path is optional: `--json --run` still means "auto file"
                    o.json = Some(match it.peek() {
                        Some(p) if !p.starts_with('-') => {
                            JsonTarget::Path(PathBuf::from(it.next().unwrap()))
                        }
                        _ => JsonTarget::Auto,
                    });
                    o.run = true;
                }
                "--label" => o.label = Some(it.next().ok_or("--label needs a value")?),
                "--settle" => o.settle_s = number(it.next(), "--settle")?,
                "--idle" => o.idle_s = number(it.next(), "--idle")?,
                "--frames" => o.frames_s = number(it.next(), "--frames")?,
                "--windows" => o.windows = number::<u32>(it.next(), "--windows")?,
                other if other.starts_with("--") => {
                    return Err(format!("unknown option {other}"));
                }
                other => {
                    if !(o.report || o.compare.is_some()) {
                        return Err(format!("unexpected argument {other}"));
                    }
                    o.files.push(PathBuf::from(other));
                }
            }
        }

        if !o.idle_s.is_finite() || !o.frames_s.is_finite() || !o.settle_s.is_finite() {
            return Err("--idle, --frames and --settle must be finite numbers".into());
        }
        if o.idle_s < 1.0 || o.frames_s < 1.0 {
            return Err("--idle and --frames must be at least 1 second".into());
        }
        if o.settle_s < 0.0 {
            return Err("--settle must not be negative".into());
        }
        Ok(o)
    }

    /// Name used for the label column and the results filename. `session` is
    /// the desktop resolved from the running compositor, which is already the
    /// environment value when the two agree — so `ubuntu:GNOME` keeps its
    /// flavour and a stale `XDG_CURRENT_DESKTOP` cannot name a GNOME run.
    pub fn label(&self, session: &str) -> String {
        let raw = self
            .label
            .clone()
            .or_else(|| Some(session.to_string()))
            .filter(|s| !s.is_empty() && s != "unknown")
            .or_else(|| std::env::var("XDG_CURRENT_DESKTOP").ok())
            .unwrap_or_else(|| "unknown".into());
        slug(&raw)
    }
}

/// `GNOME:GNOME-Classic` -> `gnome-gnome-classic`.
pub fn slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() {
        "unknown".into()
    } else {
        out
    }
}

fn number<T: std::str::FromStr>(v: Option<String>, flag: &str) -> Result<T, String> {
    let v = v.ok_or_else(|| format!("{flag} needs a value"))?;
    v.parse()
        .map_err(|_| format!("{flag}: {v} is not a number"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Options {
        Options::parse(args.iter().map(|s| s.to_string())).unwrap()
    }

    #[test]
    fn json_without_path_is_auto_and_implies_run() {
        let o = parse(&["--json"]);
        assert!(o.run);
        assert!(matches!(o.json, Some(JsonTarget::Auto)));
    }

    #[test]
    fn json_with_path() {
        let o = parse(&["--json", "out.json"]);
        assert!(matches!(o.json, Some(JsonTarget::Path(ref p)) if p == &PathBuf::from("out.json")));
    }

    #[test]
    fn report_collects_positional_arguments() {
        let o = parse(&["--report", "labwc", "results/gnome-1.json"]);
        assert!(o.report);
        assert_eq!(o.files.len(), 2);
    }

    #[test]
    fn compare_takes_two_labels() {
        let o = parse(&["--compare", "labwc", "niri"]);
        assert_eq!(o.compare, Some(("labwc".into(), "niri".into())));
    }

    #[test]
    fn compare_needs_both_labels() {
        assert!(
            Options::parse(["--compare".to_string(), "labwc".to_string()].into_iter()).is_err()
        );
    }

    #[test]
    fn non_finite_durations_are_rejected() {
        for flag in ["--idle", "--frames", "--settle"] {
            assert!(
                Options::parse([flag.to_string(), "NaN".to_string()].into_iter()).is_err(),
                "{flag} NaN must not reach Duration::from_secs_f64"
            );
            assert!(
                Options::parse([flag.to_string(), "inf".to_string()].into_iter()).is_err(),
                "{flag} inf must not reach Duration::from_secs_f64"
            );
        }
    }

    #[test]
    fn positionals_without_report_are_rejected() {
        assert!(Options::parse(["--run".to_string(), "stray".to_string()].into_iter()).is_err());
    }

    #[test]
    fn slugs_are_lowercase_and_dash_separated() {
        assert_eq!(slug("labwc:wlroots"), "labwc-wlroots");
        assert_eq!(slug("GNOME"), "gnome");
        assert_eq!(slug(""), "unknown");
    }

    #[test]
    fn label_uses_the_session_and_keeps_a_flavour() {
        let o = parse(&["--run"]);
        assert_eq!(o.label("ubuntu:GNOME"), "ubuntu-gnome");
        // a stale environment never names the run
        assert_eq!(o.label("GNOME"), "gnome");
        let named = parse(&["--run", "--label", "gnome-shell 51"]);
        assert_eq!(named.label("GNOME"), "gnome-shell-51");
    }
}
