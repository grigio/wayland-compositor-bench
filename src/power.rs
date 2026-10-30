//! Idle power, from RAPL (whole SoC / CPU package) and the battery gauge.
//!
//! RAPL energy counters are root-only on most distros, so reading them falls
//! back to `sudo -n cat`: passwordless sudo is a no-op prompt-wise, and without
//! it the power columns simply come out empty instead of wrong. Set
//! `WG_BENCH_NO_SUDO=1` to keep the tool from escalating at all.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Instant;

#[derive(Clone, Copy, Debug)]
pub struct Reading {
    /// energy counter, microjoules
    uj: u64,
    /// wrap-around width of that counter
    max: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct Sample {
    /// psys: CPU + iGPU + fabric, the best proxy for battery drain
    pub psys: Option<Reading>,
    /// package-0: CPU package only
    pub pkg: Option<Reading>,
    /// when the counters were read, used instead of the requested phase length
    pub at: Instant,
}

impl Default for Sample {
    fn default() -> Self {
        Self {
            psys: None,
            pkg: None,
            at: Instant::now(),
        }
    }
}

/// One reading of both RAPL domains. Cheap enough to call twice per phase.
pub fn sample() -> Sample {
    let at = Instant::now();
    let (psys, pkg) = domains();
    Sample {
        at,
        psys: psys.as_ref().and_then(|d| d.read()),
        pkg: pkg.as_ref().and_then(|d| d.read()),
    }
}

/// Watts between two samples, or `None` when either side is missing or the
/// actual measured window is too short to mean anything. The requested CLI
/// duration is not used: sleeps and GTK's main loop both overshoot.
pub fn delta(a: Sample, b: Sample) -> (Option<f64>, Option<f64>) {
    let seconds = b.at.saturating_duration_since(a.at).as_secs_f64();
    if seconds < 0.5 {
        return (None, None);
    }
    let watts = |a: Option<Reading>, b: Option<Reading>| {
        let (a, b) = (a?, b?);
        let d = if b.uj >= a.uj {
            b.uj - a.uj
        } else {
            b.max + b.uj - a.uj
        };
        Some(d as f64 / 1e6 / seconds)
    };
    (watts(a.psys, b.psys), watts(a.pkg, b.pkg))
}

struct Domain {
    energy: PathBuf,
    max: u64,
}

impl Domain {
    fn read(&self) -> Option<Reading> {
        let uj = read_u64(&self.energy)?;
        Some(Reading { uj, max: self.max })
    }
}

fn read_u64(path: &Path) -> Option<u64> {
    if let Ok(s) = fs::read_to_string(path) {
        return s.trim().parse().ok();
    }
    if std::env::var_os("WG_BENCH_NO_SUDO").is_some() {
        return None;
    }
    // root-only: energy_uj, and the cgroup files the script used to touch
    let out = Command::new("sudo")
        .args(["-n", "cat"])
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

/// The psys and package-0 domains, found by name rather than by index: the
/// numbering differs between platforms. Cached: the powercap tree does not
/// change while the tool runs, and every sample used to re-read it.
fn domains() -> &'static (Option<Domain>, Option<Domain>) {
    static DOMAINS: OnceLock<(Option<Domain>, Option<Domain>)> = OnceLock::new();
    DOMAINS.get_or_init(|| {
        let mut psys = None;
        let mut pkg = None;
        let Ok(rd) = fs::read_dir("/sys/class/powercap") else {
            return (None, None);
        };
        for entry in rd.flatten() {
            let dir = entry.path();
            let Ok(name) = fs::read_to_string(dir.join("name")) else {
                continue;
            };
            let name = name.trim();
            let Some(max) = fs::read_to_string(dir.join("max_energy_range_uj"))
                .ok()
                .and_then(|s| s.trim().parse().ok())
            else {
                continue;
            };
            let domain = Domain {
                energy: dir.join("energy_uj"),
                max,
            };
            match name {
                "psys" => psys = Some(domain),
                "package-0" => pkg = Some(domain),
                _ => {}
            }
        }
        (psys, pkg)
    })
}

/// The first battery, with its status string.
pub fn battery() -> Option<(PathBuf, String)> {
    let rd = fs::read_dir("/sys/class/power_supply").ok()?;
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("BAT") {
            continue;
        }
        let status = fs::read_to_string(entry.path().join("status"))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "unknown".into());
        return Some((entry.path(), status));
    }
    None
}

/// Whether the two RAPL domains can be read at all, for `--check`.
///
/// Cached per domain: the answer cannot change while the process runs, and on a
/// root-only machine every call used to fork `sudo` twice.
pub fn readable() -> (bool, bool) {
    static PSYS: OnceLock<bool> = OnceLock::new();
    static PKG: OnceLock<bool> = OnceLock::new();
    let (psys, pkg) = domains();
    (
        *PSYS.get_or_init(|| psys.as_ref().and_then(|d| d.read()).is_some()),
        *PKG.get_or_init(|| pkg.as_ref().and_then(|d| d.read()).is_some()),
    )
}

/// Battery draw in watts. `power_now` is only meaningful while discharging:
/// many embedded controllers report charge power when plugged in.
pub fn battery_watts() -> Option<f64> {
    let (dir, status) = battery()?;
    if status != "Discharging" {
        return None;
    }
    let uw: f64 = fs::read_to_string(dir.join("power_now"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(uw / 1e6)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn reading(uj: u64, max: u64) -> Reading {
        Reading { uj, max }
    }

    #[test]
    fn watts_use_the_measured_interval_not_the_requested_one() {
        let t0 = Instant::now();
        let a = Sample {
            psys: Some(reading(1_000_000, u64::MAX)),
            pkg: Some(reading(500_000, u64::MAX)),
            at: t0,
        };
        let b = Sample {
            psys: Some(reading(3_000_000, u64::MAX)),
            pkg: Some(reading(1_500_000, u64::MAX)),
            at: t0 + Duration::from_secs(2),
        };
        // 2 J over 2 s = 1 W; 1 J over 2 s = 0.5 W
        let (psys, pkg) = delta(a, b);
        assert_eq!(psys, Some(1.0));
        assert_eq!(pkg, Some(0.5));
    }

    #[test]
    fn a_window_shorter_than_half_a_second_is_not_a_measurement() {
        let t0 = Instant::now();
        let a = Sample {
            psys: Some(reading(0, u64::MAX)),
            at: t0,
            ..Default::default()
        };
        let b = Sample {
            psys: Some(reading(1_000_000, u64::MAX)),
            at: t0 + Duration::from_millis(400),
            ..Default::default()
        };
        assert_eq!(delta(a, b), (None, None));
    }
}
