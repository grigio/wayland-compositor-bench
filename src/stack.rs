//! Desktop-stack attribution.
//!
//! A compositor is not always the whole desktop. GNOME runs the shell, the
//! overview and Mutter inside `gnome-shell`; labwc is a compositor with a
//! separate shell such as Noctalia, Waybar or Quickshell. Measuring only the
//! socket owner therefore compares an integrated desktop with a compositor
//! core. This module finds the declared shell/panel/notification processes
//! inside the measured session cgroups — the login scope plus the systemd user
//! manager, because a panel started from a user unit lives in the latter — and
//! measures them separately, so a run can report both scopes:
//!
//! - `core`: the compositor process tree only;
//! - `stack`: the core plus the shell/panel/notification processes.
//!
//! Detection is intentionally conservative and auditable: the JSON stores the
//! declared selector and every process that was added. An explicit
//! `--stack name,...` wins over the auto-detection list.

use std::collections::{HashMap, HashSet};

use crate::proc;

/// Common shells, panels, launchers, notification daemons and portal frontends.
/// This is a starting point, not a definition of a desktop: `--stack` exists
/// because no external process can know what a user considers part of it.
pub const KNOWN_SHELLS: &[&str] = &[
    // Noctalia ships as `noctalia` on Arch and as `noctalia-shell` elsewhere;
    // the prefix match means one entry catches both.
    "noctalia",
    "quickshell",
    "ags",
    "eww",
    "waybar",
    "sfwbar",
    "plasmashell",
    "cosmic-panel",
    "xfce4-panel",
    "lxqt-panel",
    "mate-panel",
    "swaybar",
    "i3bar",
    "hyprpanel",
    "ironbar",
    "nwg-panel",
    "gbar",
    "dms",
    "mako",
    "dunst",
    "fnott",
    "swaync",
    "swaynotificationcenter",
    "notification-daemon",
    "xdg-desktop-portal",
];

#[derive(Clone, Debug, Default)]
pub struct Selection {
    /// `auto`, `none`, or the comma-separated names the user supplied
    pub declared: String,
    /// `integrated` when the compositor process already contains the shell
    pub style: &'static str,
    pub pids: Vec<u32>,
}

#[derive(Clone, Debug, Default)]
struct Entry {
    pid: u32,
    name: String,
    cpu_ns: u64,
    wakeups: u64,
    pss_mib: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Sample {
    entries: Vec<Entry>,
}

#[derive(Clone, Debug, Default)]
pub struct Process {
    pub pid: u32,
    pub name: String,
    pub pss_mib: f64,
    pub cpu_pct: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Delta {
    pub processes: Vec<Process>,
    pub cpu_pct: f64,
    pub pss_mib: f64,
    pub wakeups_per_s: f64,
}

fn normalized_selector(declared: &str) -> String {
    let s = declared.trim();
    if s.is_empty() {
        "auto".to_string()
    } else {
        s.to_ascii_lowercase()
    }
}

fn matches(candidate: &str, cmdline: &str, wanted: &str) -> bool {
    let wanted = wanted.trim().to_ascii_lowercase();
    if wanted.is_empty() {
        return false;
    }
    let comm = candidate.trim().to_ascii_lowercase();
    let argv0 = cmdline
        .split_whitespace()
        .next()
        .unwrap_or("")
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let cmd = cmdline.to_ascii_lowercase();
    comm == wanted
        || argv0 == wanted
        || comm.starts_with(&wanted)
        || argv0.starts_with(&wanted)
        || (wanted.len() >= 4 && cmd.contains(&wanted))
}

fn wanted_by_selector(declared: &str) -> Option<Vec<String>> {
    let selector = normalized_selector(declared);
    match selector.as_str() {
        "none" => Some(Vec::new()),
        "auto" => None,
        _ => Some(
            selector
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
        ),
    }
}

/// Find the declared stack processes. The compositor tree and this benchmark
/// are always excluded; a process cannot be added twice.
pub fn detect(comp_name: &str, comp_tree: &[u32], declared: &str) -> Selection {
    let style = if comp_name == "gnome-shell" {
        "integrated"
    } else {
        "modular"
    };
    let wanted = wanted_by_selector(declared);
    let mut skip: HashSet<u32> = comp_tree.iter().copied().collect();
    skip.insert(std::process::id());

    let mut pids = Vec::new();
    for pid in proc::session_pids() {
        if skip.contains(&pid) {
            continue;
        }
        let comm = proc::comm(pid).unwrap_or_default();
        if comm.is_empty() {
            continue;
        }
        let cmdline = proc::cmdline(pid);
        let hit = match &wanted {
            Some(names) => names.iter().any(|name| matches(&comm, &cmdline, name)),
            None => KNOWN_SHELLS
                .iter()
                .any(|name| matches(&comm, &cmdline, name)),
        };
        if hit {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    pids.dedup();
    Selection {
        declared: normalized_selector(declared),
        style,
        pids,
    }
}

pub fn sample(selection: &Selection) -> Sample {
    let entries = selection
        .pids
        .iter()
        .filter_map(|pid| {
            let name = proc::display_name(*pid)?;
            Some(Entry {
                pid: *pid,
                name,
                cpu_ns: proc::cpu_ns(*pid),
                wakeups: proc::process_wakeups(*pid),
                pss_mib: proc::pss_kb(*pid) as f64 / 1024.0,
            })
        })
        .collect();
    Sample { entries }
}

/// Per-process delta over a measured window. A process that did not exist at
/// the start is not charged its lifetime CPU, the same rule the rest of the
/// attribution uses.
pub fn delta(before: &Sample, after: &Sample, seconds: f64) -> Delta {
    if seconds <= 0.0 {
        return Delta::default();
    }
    let previous: HashMap<u32, &Entry> = before.entries.iter().map(|e| (e.pid, e)).collect();
    let mut processes = Vec::new();
    let mut cpu_pct = 0.0;
    let mut pss_mib = 0.0;
    let mut wakeups_per_s = 0.0;
    for entry in &after.entries {
        let (cpu, wakeups) = match previous.get(&entry.pid) {
            Some(old) => (
                entry.cpu_ns.saturating_sub(old.cpu_ns) as f64 / 1e9 / seconds * 100.0,
                entry.wakeups.saturating_sub(old.wakeups) as f64 / seconds,
            ),
            None => (0.0, 0.0),
        };
        cpu_pct += cpu;
        pss_mib += entry.pss_mib;
        wakeups_per_s += wakeups;
        processes.push(Process {
            pid: entry.pid,
            name: entry.name.clone(),
            pss_mib: entry.pss_mib,
            cpu_pct: cpu,
        });
    }
    processes.sort_by(|a, b| {
        b.pss_mib
            .partial_cmp(&a.pss_mib)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Delta {
        processes,
        cpu_pct,
        pss_mib,
        wakeups_per_s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_selector_matches_a_truncated_comm_or_the_command_line() {
        assert!(matches(
            "noctalia-shell",
            "/usr/bin/noctalia-shell",
            "noctalia-shell"
        ));
        // Arch packages Noctalia as `noctalia`; one auto-list entry must catch
        // both names.
        assert!(matches("noctalia", "/usr/bin/noctalia", "noctalia"));
        assert!(matches(
            "noctalia-shel",
            "/usr/bin/noctalia-shell",
            "noctalia"
        ));
        assert!(KNOWN_SHELLS
            .iter()
            .any(|name| matches("noctalia", "/usr/bin/noctalia", name)));
        assert!(matches(
            "xdg-desktop-por",
            "/usr/libexec/xdg-desktop-portal-gnome",
            "xdg-desktop-portal-gnome"
        ));
        assert!(matches("waybar", "/usr/bin/waybar -c config", "waybar"));
        assert!(!matches("waybar", "/usr/bin/waybar", "noctalia"));
        assert!(
            !matches("flags", "/usr/bin/flags", "ags"),
            "short names are exact"
        );
    }

    #[test]
    fn an_empty_selector_means_auto_and_none_means_nothing() {
        assert_eq!(normalized_selector(""), "auto");
        assert_eq!(normalized_selector(" None "), "none");
        assert!(wanted_by_selector("none").unwrap().is_empty());
        assert!(wanted_by_selector("auto").is_none());
        assert_eq!(
            wanted_by_selector("Noctalia, waybar").unwrap(),
            vec!["noctalia".to_string(), "waybar".to_string()]
        );
    }

    #[test]
    fn a_new_process_has_no_in_window_cpu() {
        let before = Sample::default();
        let after = Sample {
            entries: vec![Entry {
                pid: 42,
                name: "noctalia-shell".into(),
                cpu_ns: 10_000_000_000,
                wakeups: 100,
                pss_mib: 120.0,
            }],
        };
        let d = delta(&before, &after, 10.0);
        assert_eq!(d.cpu_pct, 0.0);
        assert_eq!(d.pss_mib, 120.0);
        assert_eq!(d.processes.len(), 1);
    }

    #[test]
    fn a_delta_uses_the_measured_window() {
        let before = Sample {
            entries: vec![Entry {
                pid: 7,
                name: "shell".into(),
                cpu_ns: 0,
                wakeups: 10,
                pss_mib: 50.0,
            }],
        };
        let after = Sample {
            entries: vec![Entry {
                pid: 7,
                name: "shell".into(),
                cpu_ns: 2_000_000_000,
                wakeups: 30,
                pss_mib: 50.0,
            }],
        };
        let d = delta(&before, &after, 2.0);
        assert!((d.cpu_pct - 100.0).abs() < 1e-9);
        assert!((d.wakeups_per_s - 10.0).abs() < 1e-9);
    }
}
