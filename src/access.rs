//! Pre-run access probe: which measurements this machine can actually provide.
//!
//! The placeholder in the window lists the failures before the first run, so a
//! cgroup v1 distro, a root-only RAPL or a `hidepid` `/proc` mount does not turn
//! into a radar full of zeros. `--check` prints the same conditions in more
//! detail; this module only answers "what will be missing, and why".

use std::path::Path;

use crate::{power, proc, sysinfo};

/// Why an issue is here, so `--check` and tests can tell the families apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// No cgroup v2 counters at all: cgroup v1 or a missing mount.
    SessionCgroup,
    /// cgroup v2 exists but is the user slice, not the session scope.
    SessionCgroupApprox,
    /// No process owns the Wayland socket.
    Compositor,
    /// `smaps_rollup` is unreadable, so PSS reads 0.
    CompositorRam,
    /// `/proc/<pid>/task` or `schedstat` is unreadable.
    CompositorCpu,
    /// RAPL energy counters are missing or root-only.
    Rapl,
    /// `cpu.pressure` / `memory.pressure` are missing.
    Psi,
}

#[derive(Clone, Debug)]
pub struct Issue {
    pub kind: Kind,
    /// The metric family, e.g. "session RAM/CPU".
    pub metric: &'static str,
    /// What is missing, with the path or the observed condition.
    pub detail: String,
    /// What the user can do about it, if anything.
    pub fix: &'static str,
}

/// Probe every source the radar and the session totals depend on.
///
/// Cheap except for `power::readable()`, which may fork `sudo -n` once. Call it
/// after the window is up, not on the activation path.
pub fn probe(comp: Option<&proc::Compositor>) -> Vec<Issue> {
    let mut out = Vec::new();
    let cgs = proc::session_cgroups();
    let paths = proc::cgroup_paths();

    // ------------------------------------------------------- session cgroup --
    // `exists()` is not enough: a cgroup file can be present and unreadable.
    // Every measured cgroup must be readable; a split desktop reads both.
    let readable = |name: &str| {
        cgs.iter()
            .all(|cg| proc::read(&cg.join(name).to_string_lossy()).is_some())
    };
    let has_mem = readable("memory.current");
    let has_cpu = readable("cpu.stat");
    let has_memstat = readable("memory.stat");
    let has_procs = readable("cgroup.procs");

    if !(has_mem && has_cpu && has_memstat && has_procs) {
        let v2 = Path::new("/sys/fs/cgroup/cgroup.controllers").exists();
        let v1 = Path::new("/sys/fs/cgroup/memory").exists()
            || Path::new("/sys/fs/cgroup/cpuacct").exists();
        let (detail, fix) = if !Path::new("/sys/fs/cgroup").exists() {
            (
                "the cgroup filesystem is not mounted at /sys/fs/cgroup".to_string(),
                "mount cgroup v2, or run inside a session that provides one",
            )
        } else if !v2 && v1 {
            (
                format!("cgroup v2 is not mounted; {paths} is a cgroup v1 path"),
                "boot with the unified cgroup v2 hierarchy, or run inside a cgroup v2 session scope",
            )
        } else if v2 && cgs.iter().any(|cg| !cg.exists()) {
            (
                format!("cgroup v2 is mounted, but {paths} does not exist"),
                "start the compositor from a cgroup v2 session scope, or run inside its cgroup",
            )
        } else {
            (
                format!(
                    "{paths} has no readable memory.current / memory.stat / cpu.stat / cgroup.procs"
                ),
                "enable the cgroup memory and cpu controllers for this session",
            )
        };
        out.push(Issue {
            kind: Kind::SessionCgroup,
            metric: "session RAM/CPU",
            detail,
            fix,
        });
    } else if !proc::cgroup_exact() {
        out.push(Issue {
            kind: Kind::SessionCgroupApprox,
            metric: "session RAM/CPU",
            detail: format!("using {paths} (the user slice), which also contains other sessions"),
            fix: "start the compositor from a session scope, or read the numbers as an upper bound",
        });
    }

    // ---------------------------------------------------------- compositor --
    match comp {
        None => out.push(Issue {
            kind: Kind::Compositor,
            metric: "compositor RAM/CPU",
            detail: format!(
                "no process owns the WAYLAND_DISPLAY socket ({})",
                std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "unset".into())
            ),
            fix: "run inside the compositor session, or point WAYLAND_DISPLAY at its socket",
        }),
        Some(c) => {
            if proc::pss_kb(c.pid) == 0 {
                out.push(Issue {
                    kind: Kind::CompositorRam,
                    metric: "compositor RAM",
                    detail: format!(
                        "/proc/{}/smaps_rollup is unreadable (hidepid or permissions)",
                        c.pid
                    ),
                    fix: "mount /proc without hidepid=2 for this user, or run as the compositor's user",
                });
            }
            let tasks = proc::tasks(c.pid);
            if tasks.is_empty() {
                out.push(Issue {
                    kind: Kind::CompositorCpu,
                    metric: "compositor CPU",
                    detail: format!("/proc/{}/task is unreadable", c.pid),
                    fix: "check /proc permissions (hidepid) and that the compositor runs as your user",
                });
            } else if tasks.iter().all(|t| t.ns == 0) {
                out.push(Issue {
                    kind: Kind::CompositorCpu,
                    metric: "compositor CPU",
                    detail: format!("/proc/{}/task/*/schedstat is missing or zero", c.pid),
                    fix: "enable CONFIG_SCHEDSTATS; without it idle CPU falls back to 10 ms ticks",
                });
            }
        }
    }

    // ----------------------------------------------------------- RAPL power --
    let (psys, pkg) = power::readable();
    if !psys && !pkg {
        let powercap = Path::new("/sys/class/powercap").exists();
        out.push(Issue {
            kind: Kind::Rapl,
            metric: "idle power",
            detail: if powercap {
                "RAPL energy_uj is unreadable (usually 0400 root)".to_string()
            } else {
                "no /sys/class/powercap on this system".to_string()
            },
            fix: if powercap {
                "install the udev rule from the README, or allow passwordless sudo"
            } else {
                "not available on this hardware or kernel"
            },
        });
    }

    // -------------------------------------------------------------- pressure --
    // A missing cgroup already explains why PSI is missing; do not list the
    // same cause twice.
    let cgroup_missing = out.iter().any(|i| i.kind == Kind::SessionCgroup);
    let has_psi = cgs.iter().all(|cg| {
        sysinfo::psi(cg, "cpu.pressure").is_some() && sysinfo::psi(cg, "memory.pressure").is_some()
    });
    if !cgroup_missing && !has_psi {
        out.push(Issue {
            kind: Kind::Psi,
            metric: "pressure stalls",
            detail: format!("{paths} has no cpu.pressure / memory.pressure"),
            fix: "enable CONFIG_PSI and use a cgroup v2 mount",
        });
    }

    out
}

/// Pango markup for the placeholder. A clean probe gets a green tick; failures
/// get a red cross and are listed with their detail and fix. At most
/// [`MAX_SHOWN`] issues are listed; the rest collapse into a count so the small
/// card cannot grow without bound.
pub fn markup(issues: &[Issue]) -> String {
    const MAX_SHOWN: usize = 4;

    if issues.is_empty() {
        return "<span foreground=\"#4ade80\">✓</span> <b>metric access checked</b> — all metric sources are readable"
            .to_string();
    }

    let mut s = if issues.len() == 1 {
        "<span foreground=\"#f87171\">✗</span> <b>metric access checked</b> — 1 metric is unavailable"
            .to_string()
    } else {
        format!(
            "<span foreground=\"#f87171\">✗</span> <b>metric access checked</b> — {} metrics are unavailable",
            issues.len()
        )
    };
    for issue in issues.iter().take(MAX_SHOWN) {
        s.push_str(&format!(
            "\n<span foreground=\"#f87171\">✗</span> <b>{}</b> — {}",
            escape(issue.metric),
            escape(&issue.detail)
        ));
        if !issue.fix.is_empty() {
            s.push_str(&format!(" <small>({})</small>", escape(issue.fix)));
        }
    }
    if issues.len() > MAX_SHOWN {
        s.push_str(&format!("\n… and {} more", issues.len() - MAX_SHOWN));
    }
    s.push_str("\n<small>run <tt>wayland-compositor-bench --check</tt> for details</small>");
    s
}

/// Pango treats `&`, `<` and `>` as markup. Paths can contain any of them.
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_returns_well_formed_issues() {
        let comp = proc::compositor();
        for issue in probe(comp.as_ref()) {
            assert!(!issue.metric.is_empty());
            assert!(!issue.detail.is_empty());
            assert!(!issue.fix.is_empty());
        }
    }

    #[test]
    fn markup_reports_a_clean_check_with_a_tick() {
        let s = markup(&[]);
        assert!(s.contains('✓'), "{s}");
        assert!(s.contains("metric access checked"), "{s}");
        assert!(s.contains("all metric sources are readable"), "{s}");
        assert!(!s.contains('✗'), "{s}");
    }

    #[test]
    fn markup_lists_every_issue_and_escapes_paths() {
        let issues = vec![
            Issue {
                kind: Kind::Rapl,
                metric: "idle power",
                detail: "RAPL <0400> & root".into(),
                fix: "udev",
            },
            Issue {
                kind: Kind::Psi,
                metric: "pressure",
                detail: "no psi".into(),
                fix: "CONFIG_PSI",
            },
        ];
        let s = markup(&issues);
        assert!(s.contains("2 metrics"));
        assert!(s.contains("idle power"));
        assert!(s.contains("RAPL &lt;0400&gt; &amp; root"));
        assert!(s.contains("pressure"));
        assert!(!s.contains("<0400>"));
    }

    #[test]
    fn markup_caps_the_list() {
        let issues: Vec<Issue> = (0..6)
            .map(|i| Issue {
                kind: Kind::Psi,
                metric: "pressure",
                detail: format!("issue {i}"),
                fix: "fix",
            })
            .collect();
        let s = markup(&issues);
        assert!(s.contains("… and 2 more"));
        assert!(s.contains("issue 3"));
        assert!(!s.contains("issue 4"));
    }
}
