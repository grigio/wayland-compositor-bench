# Changelog

All notable changes to this project. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[semantic versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.0] — 2026-10-30

### Added

- A CPU timeline under the radar: the same 100 ms samples that feed the
  radar's `CPU avg` vertex, drawn as a strip across the run (idle, single-surface
  frame test, extra toplevels, multi-surface frame test), so it is visible *when*
  the session got busy instead of only how busy it got. The axis is 0–100 % of
  one core and only grows in 100 % steps when a run really goes past one core.
- A `TOP RAM CONSUMERS` row spanning both panels, just above the protocol list:
  one horizontal bar cut into a stripe per process name — the nine heaviest in
  their own colour, a grey `Others` for everything below the cut — with the name
  and its resident size in each legend tag under the bar. It is a single snapshot
  taken when the window opens, so a benchmark run never moves it, and the numbers
  are RSS from `/proc/<pid>/status` grouped by `comm`, which turns a dozen WebKit
  children into one stripe instead of a dozen. Clicking a stripe dims the others
  and highlights that name's tag; clicking the same stripe again clears it. The
  window is taller to hold it.
- A `quiet cpu` check and an `other CPU` figure: session CPU that is neither the
  compositor nor this tool is reported as `idle.other_cpu_pct`; above 5 % the
  check fails and names the busiest processes seen while the session settles.
  The `idle` table in `--report` gained an `other CPU %` column.
- Every JSON run records the blur chip state (`blur`), package and battery power
  around each frame test, the benchmark's own CPU share (`self_cpu_s`) and the
  session figure with it subtracted (`session_cpu_excl_self_s`). With `--json`,
  `peaks.cpu_series` and `peaks.cpu_marks` also keep the 100 ms CPU timeline and
  its phase boundaries.
- `--check` prints the actual `--idle`, `--settle`, `--frames` and `--windows`
  values instead of hardcoded defaults, and exits 2 when the session is
  measurable only with caveats.
- Both packages install the `.desktop` launcher and the hicolor SVG icon.

### Changed

- The radar's red vertex is `CPU AVG`, not `CPU PEAK`: the mean of the samples
  across the run is the load the session actually carried, where one spike while
  a window opened used to decide the whole axis. The peak is still in the
  details and in the JSON.
- The run JSON records `peaks.cpu_avg_pct` next to the peak, and the `peaks`
  table in `--report` now has a `CPU avg %` column. The field is optional, so
  files written by 0.3.1 and earlier keep loading and print `-` there rather
  than a zero that was never measured — no schema bump, no runs thrown away.
- The `Wayland compositor` row reads `Mutter 51.1 · GNOME` instead of just
  `GNOME`: the implementation and its release first, then the desktop. Mutter
  is a library inside GNOME Shell, so the process name never named it; KWin,
  cosmic-comp and the standalone compositors are mapped the same way, and a
  standalone compositor that is its own desktop (`sway`, `labwc`) is not named
  twice. `--check` prints the identical string, so the terminal and the window
  cannot disagree about what the session is.
- Automatic runs are written under
  `$XDG_STATE_HOME/wayland-compositor-bench/results` (usually
  `~/.local/state/...`) instead of the working directory. `--report` and
  `--compare` read that directory and `./results`, and match label substrings
  before falling back to file paths. `--compare` now includes the `peaks` phase,
  and `--report` prints the failing checks behind a `medium`/`low` confidence.
- RAPL domains and readability are cached, power is sampled outside the CPU
  windows, and `WG_BENCH_NO_SUDO=1` disables the non-interactive `sudo -n`
  fallback. The README now says when the tool escalates instead of claiming it
  never needs root.
- The 1 s live updater reads only the cheap cgroup counters every second and
  refreshes PSS, GPU memory, pressure and package temperature every fifth tick.
  `pci.ids`, the RAPL domains and `compositor_version` are parsed once.
- CI caches the Rust build, runs an MSRV job pinned to `rust-version`, runs
  `cargo audit`, and can be dispatched manually.
- The schema stays at v3: every new field is optional or defaulted, so results
  written by 0.3.x still load and compare.

### Fixed

- `Session CPU` seeded its first tick with zero, so it reported the cumulative
  time since boot; after a run it reported the run's average as idle. It now
  seeds from the live cgroup counter and re-baselines when a run starts or ends.
- `--run` exits non-zero when the JSON cannot be written instead of leaving an
  empty file and reporting success.
- `mean_cpu` used the following interval to weight each sample, dropping the
  last real sample; it now uses the trapezoid rule.
- The sampler's settle/run reset could leak one pre-run interval into the new
  timeline. The baseline lives in the shared series now, so `reset` and the
  sampling thread cannot disagree.
- `Probe` read the session counters after its own thread scans, charging part of
  the probe to the measured window; counters are read first now, and the
  compositor's threads are scanned once instead of twice.
- `--report` filtered on file paths while `--compare` filtered on labels;
  both parse the run and match its label first.
- The report table sorts by a numeric key instead of re-parsing the formatted
  cell, and field access is typed instead of round-tripping every phase through
  JSON.

## [0.3.1] — 2026-10-29

### Changed

- The binary is now called `wayland-compositor-bench` (it was `wayland-bench`);
  both packages install `/usr/bin/wayland-compositor-bench`.

### Added

- The `blur:` chip next to Start is now a switch: clicking it drops the
  translucent, compositor-blurred window background for a plain opaque one, so
  a run can be measured with and without the blur the compositor pays for.
- GPU detection walks every `/sys/class/drm/card*` instead of assuming
  `card0`, follows the compositor's open card/render node, and reads the driver
  from the driver symlink when the uevent omits it. This finds AMD integrated
  GPUs and other platform devices that used to show as `unknown GPU`; extra
  GPUs are listed in the GPU tooltip.
- `pci.ids` is looked up in the hwdata, misc and pciutils locations, so names
  resolve on Debian as well as Arch.

### Removed

- The C capability probe (`src/blur.c`, `build.rs`, the vendored protocol XML)
  is now Rust: `wayland-client` + `wayland-protocols` enumerate the registry and
  read the blur capabilities on a dedicated connection.

## [0.3.0] — 2026-10-29

### Fixed

- The session is identified by the process that owns the `WAYLAND_DISPLAY`
  socket, not by `XDG_CURRENT_DESKTOP`. GNOME Shell runs as a systemd user
  service and inherits the user manager's environment, so a GNOME session
  entered from another desktop used to be labelled with that desktop (for
  example `cosmic-comp-traffic · COSMIC`) in the window, in `--check`, in the
  JSON and in the results filename. The environment is still used when it
  agrees with the socket owner, so flavours like `ubuntu:GNOME` are kept.

### Added

- `desktop` in the JSON is now the resolved desktop, and `desktop_env` records
  the raw `XDG_CURRENT_DESKTOP` for provenance.
- A `desktop identity` check fails, and `--check`/`--report` warn, when the
  environment names a different desktop than the running compositor.
