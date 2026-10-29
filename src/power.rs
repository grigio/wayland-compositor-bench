//! Idle power, from RAPL (whole SoC / CPU package) and the battery gauge.
//!
//! RAPL energy counters are root-only, so reading them falls back to
//! `sudo -n cat`: passwordless sudo is a no-op prompt-wise, and without it the
//! power columns simply come out empty instead of wrong.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Copy, Debug)]
pub struct Reading {
    /// energy counter, microjoules
    uj: u64,
    /// wrap-around width of that counter
    max: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Sample {
    /// psys: CPU + iGPU + fabric, the best proxy for battery drain
    pub psys: Option<Reading>,
    /// package-0: CPU package only
    pub pkg: Option<Reading>,
}

/// One reading of both RAPL domains. Cheap enough to call twice per phase.
pub fn sample() -> Sample {
    let (psys, pkg) = domains();
    Sample {
        psys: psys.and_then(|d| d.read()),
        pkg: pkg.and_then(|d| d.read()),
    }
}

/// Watts between two samples, or `None` when either side is missing or the
/// window is too short to mean anything.
pub fn delta(a: Sample, b: Sample, seconds: f64) -> (Option<f64>, Option<f64>) {
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
/// numbering differs between platforms.
fn domains() -> (Option<Domain>, Option<Domain>) {
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
pub fn readable() -> (bool, bool) {
    let (psys, pkg) = domains();
    (
        psys.and_then(|d| d.read()).is_some(),
        pkg.and_then(|d| d.read()).is_some(),
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
