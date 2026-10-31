# Changelog

All notable changes to this project. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[semantic versioning](https://semver.org/spec/v2.0.0.html).

## [0.6.0] — 2026-10-31

### Added

- **Desktop-stack scopes.** `core` is the compositor process tree; `stack` adds
  the separate shell/panel/notification processes (Waybar, Noctalia,
  Quickshell, SwayNC, portals, …). `--stack auto|none|NAME,…` picks the scope,
  the JSON records the selector, the style (`integrated`/`modular`) and every
  added process, and the idle table reports both scopes plus `unrelated CPU`.
  This stops a GNOME desktop, whose shell lives inside `gnome-shell`, from being
  compared with a bare labwc compositor.
- **The measured session can be two cgroups.** When logind holds the compositor
  (`session-N.scope`) and the panel or portals are systemd user services
  (`user@UID.service`), both are measured and summed. `cgroup_kind` reports
  `login-session+user-manager`, and the JSON `cgroup` field lists both paths.
  PSI reports the higher of the two, since pressure is a share of wall time and
  cannot be added. Without this, a labwc + Noctalia desktop lost its panel from
  every session and stack figure.
- `--profile native|reference`: `reference` starts with blur off, targets 60 Hz
  and uses a 10 s frame test, so the render comparison measures the compositor
  rather than the session's configuration.
- `--target-hz HZ` throttles the benchmark client to a fixed rate.
- Panel-relative pacing: `pacing_vs_panel`, `dropped_vs_panel`,
  `p95_over_panel`, and `comp_cpu_pct_at_60hz`, which scales compositor CPU from
  the panel refresh to a common 60 Hz panel. A deliberate 60 fps on a 120 Hz
  panel is now 50 % of the panel.
- `--strict` makes `--compare` fail when the two sides are not comparable.
- `cgroup_kind` in the JSON, and `self_pss_mib` / `mem_excl_self_mib` so the
  session memory can be read without the benchmark client.

### Changed

- The radar plots stack RAM and stack CPU against a fixed 8 GiB memory
  reference, panel-relative pacing, and render cost at a common 60 Hz panel.
- Each radar vertex now carries the direction its own number should move: `↓`
  on render cost, stack RAM and stack CPU, `↑` on pacing and features. The cost
  axes are drawn inverted (a lower number lands further out), so the hover help
  said "Higher is better" next to a raw cost percentage — it now says "Lower is
  better" and explains the inversion.
- Render cost is log-scaled like the two stack axes. The old linear `1 - CPU %`
  put every realistic compositor between 0.92 and 0.99 of the radius, so 2 % and
  8 % of a core looked identical.
- `Stats::noise()` includes a MAD term; `--compare` labels fewer than three
  runs per label `anecdotal` instead of declaring a winner.
- `--compare` warns on different cgroup kinds, stack styles, workload profiles,
  blur states, power settings and presentation-feedback sources.
- `other_cpu_pct` keeps its v3 meaning; the `quiet cpu` check and its offender
  list now use `unrelated_cpu_pct`, which excludes the declared stack.

## [0.5.0] — 2026-10-30

### Added

- Hover help on the radar: hovering an axis label highlights that axis and
  shows a tooltip with what it plots and which direction is better. Every axis
  is further-out-is-better, including the cost axes, so the tooltip spells out
  that the plotted value is inverted.
- The placeholder before the radar now warns which metrics this machine cannot
  provide, with the path and the fix: cgroup v1 or a missing session scope,
  unreadable RAPL, a `hidepid` `/proc`, missing `schedstat` or PSI, and an
  unresolved compositor. A green tick confirms the check when every source is
  readable; a red cross marks the ones that are not. The probe runs after the
  first frame so the RAPL `sudo -n` fallback cannot delay the window.
- The run JSON now carries the whole protocol snapshot in `features`, including
  per-item versions and a `stale` flag for globals advertised below the useful
  version. A failed scan writes `null`, not `0 %`.
- `idle.measured_s` and `render.measured_s` record the actual wall-clock window;
  `windows` gained optional `baseline_drift_mib` and `post_close_drift_mib`,
  `delta_valid`, and a signed `delta_mib`; `render` stores
  `comp_gpu_ms_per_frame` and `app_gpu_ms_per_frame` separately, while
  `gpu_ms_per_frame` keeps its compositor-only v3 meaning.

### Changed

- The radar now plots the compositor's attributable cost instead of session
  totals: `PACING` and `LOAD PACING` (frames delivered at the observed cadence,
  one surface and the multi-surface load case), `RENDER COST` (compositor CPU
  per frame against the cadence period), `COMP RAM` (compositor PSS relative to
  total RAM), `COMP CPU` (compositor idle CPU relative to one core) and
  `FEATURES`. A clean 60 fps on a 120 Hz panel now reads 100 %, not 50 %, and
  RAM before/peak — two vertices a few MiB apart — are gone. The session totals
  stay in the line under the chart.
- Idle CPU and power now use the measured wall-clock interval. The start
  snapshot scans before reading counters, the end snapshot reads counters before
  scanning, and the power samples bracket only the sleep, so neither `/proc`
  walk nor the requested `--idle` value is in the denominator.
- `other CPU` and its top offenders now come from the same idle window and the
  same exclusion set (compositor process tree plus this tool); a process that
  appeared mid-window is no longer charged its whole lifetime CPU.
- `wakeups/s` sums context switches over every compositor thread instead of only
  the thread-group leader.
- GPU counters are parsed per engine (`rcs`, `bcs`, `vcs`, `ccs`, …), merged per
  client with `max` per engine, and summed across engines. Frame tests sample
  the benchmark client as well as the compositor; `gpu_ms_per_frame` keeps its
  compositor-only v3 meaning and the client is stored separately.
- Extra-toplevel memory settles for a second, takes two baselines, keeps the raw
  signed delta, and marks a negative or drifting measurement invalid instead of
  clamping it to zero. `residual_mib` keeps its v3 meaning (what the session did
  not give back); `post_close_drift_mib` is the drift between two post-close
  samples.

### Fixed

- GNOME sessions were measured as the whole user slice. GNOME Shell runs as a
  systemd user service and the apps it launches live in `app-*.scope` units
  below `user@UID.service`, so `/proc/self/cgroup` never contains a
  `session-N.scope`: the tool fell back to `user-1000.slice`, `--check` exited
  2 with "no session scope", and session RAM/CPU could include other logins
  even though the window still showed numbers. The user manager is now the
  measured scope; the user slice is only used when neither it nor a login
  session scope exists. Process-level data (process count, idle CPU
  attribution, contaminants) now walks the cgroup subtree: the manager's own
  `cgroup.procs` is empty because systemd attaches every process to a child
  cgroup.
- A v3 file without `other_cpu_pct` no longer prints `0.00` (the best possible
  value). Missing optional numerics print `-`, and `--compare` prints
  `not measured` or an absolute delta with `n/a` for a zero baseline instead of
  a fabricated `+0 %`.
- `--idle NaN` no longer reaches `Duration::from_secs_f64` and panics; the
  duration options must be finite.

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
