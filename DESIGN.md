# Design

How `wayland-compositor-bench` turns a running Wayland session into numbers,
why each figure is read the way it is, and what the tool deliberately refuses to
claim. For installation and usage see [README.md](README.md).

## Goals

- **Measure the session you are logged into.** A nested test compositor is easy
  to control and tells you nothing about the desktop you actually run.
- **One binary.** No helper daemon and no runtime shell/Python layer, and no
  compositor patches.
- **Traceable numbers.** Every measured value comes from `/proc`, the session's
  cgroup v2 or sysfs, and every run records the context needed to interpret it.
- **Honest uncertainty.** No single score, no silent zeros, and a confidence
  level derived from checks that are stored in the result.
- **Comparable history.** One JSON writer and one reader, with a stable schema
  for runs made by different builds.

The benchmark does **not** try to be a synthetic graphics benchmark. Its job is
to measure the cost of a real client (its own window) inside a real session.

## Module map

| file | responsibility |
|---|---|
| `src/main.rs` | GTK4 application, window layout, `--check`, live labels, results panels |
| `src/bench.rs` | run sequence, settle logic, sampling, frame tests, checks, confidence |
| `src/proc.rs` | `/proc`, cgroup, CPU, PSS, DRM fdinfo, compositor detection, versions, config hashes |
| `src/access.rs` | pre-run access probe behind the placeholder warning: which sources work, and the fix |
| `src/sysinfo.rs` | machine/display/power-supply information, contaminants, environment snapshot |
| `src/power.rs` | RAPL domains, battery gauge, optional `sudo -n` fallback |
| `src/wlscan.rs` | Wayland registry scan, blur capability probe, protocol tiers |
| `src/report.rs` | JSON schema, saved-run loading, medians, `--report`, `--compare` |
| `src/radar.rs` | six-axis post-run summary |
| `src/cpuchart.rs` | CPU timeline strip under the radar |
| `src/ramchart.rs` | `TOP RAM CONSUMERS` bar |
| `src/cli.rs` | argument parsing, result labels and filename slugs |

## Run pipeline

1. Resolve the session cgroups, the compositor process and the desktop identity.
2. Collect the environment snapshot (CPU, GPU, memory, display, power profile,
   kernel, Mesa, compositor config hash).
3. Scan the Wayland registry on a dedicated connection and probe blur support.
4. Wait for the session to settle.
5. Measure the idle session for `--idle` seconds (default 20).
6. Run the single-surface frame test for `--frames` seconds (default 6).
7. Open `--windows` extra animated toplevels (default 6), wait 3 s, and measure
   the memory delta against a settled, double-sampled baseline.
8. Run the multi-surface frame test for `--frames` seconds.
9. Close the extra windows, wait 1.5 s, take two post-close samples, and report
   both the drift between them and the memory not returned to the pre-open
   baseline.
10. Print the summary, draw the radar, and optionally write the JSON result.

The 25 ms peak sampler starts before the session settles, is reset when the idle
phase begins, and runs until the summary is built. Its phase marks are `idle`,
`single`, `+windows` and `multi`.

## Session identity

### Cgroup

The measurement scope is resolved from `/proc/self/cgroup`:

1. `.../session-N.scope` — a logind login session, where the compositor started
   by the display manager and the apps it spawned live.
2. `.../user@<uid>.service` — the systemd user manager. GNOME and Plasma 6 run
   the compositor and the session services as user services, and launch apps as
   `app-*.scope` units under the manager, outside `session-N.scope`. The
   manager is the directory that actually contains the session, so it counts as
   exact; in the same setup `session-N.scope` holds only the greeter worker.
3. Both at once — `login-session+user-manager`. A desktop can span the two
   halves: greetd starts labwc in `session-N.scope`, while a panel, notification
   daemon or portal started from a systemd user unit (Noctalia, SwayNC, the
   portals) lives under `user@<uid>.service`. The two subtrees are siblings, so
   their `memory.stat`, `cpu.stat` and process lists are disjoint and can be
   summed. Measuring one half alone would compare half a desktop.
4. `/sys/fs/cgroup/user.slice/user-<uid>.slice` — the last resort (containers,
   no user manager). It also contains other sessions and unrelated processes,
   so it is reported as `cgroup_exact: false` and `--report` warns about it.

A run on a user slice is usable, but session RAM and CPU include neighbours.
`--check` prints the resolved paths and which of the four kinds it is. The JSON
`cgroup` field joins the paths with ` + `, and `cgroup_kind` records the kind.

The directories are trees, not lists: systemd attaches every session process to a
child cgroup (`session.slice`, `app.slice`, …), so the manager's own
`cgroup.procs` is empty. Process-level figures — the process count, the idle CPU
attribution and the contaminant scan — walk every subtree, while RAM and CPU
totals come from the hierarchical `memory.stat` and `cpu.stat` of each measured
cgroup, added together.

PSI is the exception: `cpu.pressure` and `memory.pressure` are shares of wall
time, and two sibling cgroups can stall at the same instant, so adding them
would double-count. The live panel and `--check` report the highest `avg10` of
the measured cgroups, which is a floor for the desktop rather than a claim that
one half is the whole.

### Compositor

The compositor is the process that owns the **listening** `WAYLAND_DISPLAY`
socket:

1. Resolve the socket path (absolute, or `$XDG_RUNTIME_DIR/<name>`).
2. Find its kernel inode in `/proc/net/unix`.
3. Scan `/proc/*/fd` for `socket:[inode]`.
4. Fall back to the oldest known compositor name inside the session cgroup.

The fallback is recorded as `compositor_source: "comm"` and flagged, because a
nested compositor or a multi-seat setup can make name matching pick the wrong
process. Connected client sockets have different inodes, so the socket lookup is
unique.

The process name and the implementation are separate concepts. `gnome-shell` is
the process, Mutter is the implementation inside it, and GNOME is the desktop;
KWin inside `kwin_wayland` is the same idea. The UI renders this as
`Mutter 51.1 · GNOME`, while `--check` prints the identical string.

### Desktop identity

The desktop is derived from the socket owner's command name, not trusted from
`XDG_CURRENT_DESKTOP`. The environment is kept as `desktop_env` for provenance. It
wins only when it agrees with the detected desktop, which preserves flavours such
as `ubuntu:GNOME` and `labwc:wlroots`. A stale environment (for example a GNOME
session entered from COSMIC) is recorded as a failed `desktop identity` check and
reported, but never used to name the run.

### Versions and configuration

- **Compositor version:** run `<binary> --version`, keep the first non-empty
  line, kill the child after 2 s if it does not exit, and cache the answer.
  Version extraction for display looks for the first dotted numeric token, so
  `sway version 1.11` becomes `1.11`.
- **Config hash:** for compositors with known config paths (`~/.config/labwc`,
  `~/.config/kwinrc`, …), hash the file names and bytes (bounded recursion,
  512 KiB per file). Runs with the same label but a different config hash are
  flagged by `--report` instead of being silently averaged.

## Measurement model

### Memory

- **Working set** = `anon + shmem` from the `memory.stat` of each measured
  cgroup, added together.
  Page cache (`file`) is excluded because it follows what the session read, not
  what the desktop costs.
- **`memory.current`** is reported separately as the whole cgroup, cache
  included. `kernel` and `swap` are informational fields.
- **Peak RAM** is sampled every 25 ms from `memory.stat`, not taken from
  `memory.peak`: the kernel high-water mark can only be reset by root, so it
  would report "since login". The sampler takes the maximum of `anon + shmem`.
- **Compositor memory (core)** is the PSS of the compositor process and all its
  descendants, read from `smaps_rollup`. PSS avoids counting shared pages once
  per process.
- **Stack memory** adds the separate shell/panel/notification processes found by
  `stack::detect` (or named with `--stack`). A compositor whose shell lives in
  its own process reports `stack_style: "integrated"`; a compositor with a
  separate panel reports `"modular"` and lists the added processes. The
  selector, the added processes and their individual PSS/CPU are stored, so the
  scope is auditable rather than implied.
- **Benchmark correction:** `self_pss_mib` and `mem_excl_self_mib` record the
  benchmark client's own PSS and the session working set without it. The
  headline session figure still includes the client, because the client is a
  real client; the correction is there for readers who want the desktop alone.
- **Extra toplevels:** wait for the single-surface test's transient buffers to
  drain, take two baselines a second apart, then open the windows and wait for
  them to settle. The second baseline is the reference; the drift between the
  two is stored so a moving session is visible. The delta is signed: a negative
  value is a measurement, not zero growth, and `per_window_mib` is `null` when
  the delta is negative or the baseline drifted more than 5 MiB. The extra
  toplevels belong to this process, so the JSON labels the method
  `same-process GTK toplevels`; the client share is not a separate client's
  footprint.
- **Residual memory:** after close, two snapshots 1.5 s apart. `residual_mib`
  is the second sample against the pre-open baseline (what the session did not
  give back; this keeps its v3 meaning). `post_close_drift_mib` is the drift
  between the two post-close samples (the leak indicator). Neither is clamped:
  negative values mean the session shrank.
- **GPU memory** is not part of the cgroup: dmabufs live in the driver. It is
  read from DRM fdinfo and reported as a total for the clients and for the
  compositor, never as a per-window number, because clients pool buffers.

### CPU

- **Session CPU** comes from the cgroup's `cpu.stat` `usage_usec`.
- **Compositor CPU** is the sum of `schedstat` nanoseconds over every thread in
  `/proc/<pid>/task/*`. `schedstat` is used instead of `utime + stime` because
  the latter is quantised to 10 ms ticks; a lean compositor at 120 Hz burns only
  a few milliseconds per second, which is below tick resolution.
- **Other CPU** is session CPU minus the compositor process tree and the
  benchmark process, floored at zero. It keeps its v3 meaning: a separate shell
  counts as other.
- **Stack CPU** is compositor CPU plus the CPU of the declared shell/panel
  processes, measured with the same before/after `schedstat` snapshots.
- **Unrelated CPU** is session CPU minus the stack and the benchmark. This is
  the number the `quiet cpu` check uses, and the top-offender list excludes the
  declared stack so a panel is not reported as an intruder. A process that did
  not exist at the start of the window is not charged its whole lifetime CPU.
  It is not a guess about what "should" be running.
- **Idle CPU** is a before/after snapshot over `--idle` seconds. The start
  snapshot scans first and reads the counters last; the end snapshot reads the
  counters first and scans last, so neither `/proc` walk is inside the interval.
  The denominator is the measured `Instant` difference, stored as `measured_s`,
  not the requested `dur_s`. A `quiet cpu` check fails when unrelated CPU
  exceeds 5 % of one core and names the busiest processes from that same idle
  window.
- **Wakeups/s** sums voluntary and non-voluntary context switches over every
  `/proc/<pid>/task/*/status`, not just the thread-group leader, and divides by
  the measured idle window.
- **Particularly busy threads** are reported as a share of the compositor's CPU,
  capped at the four busiest.

The probe scans first and reads the cheap session counters last at the start of
a window, and reads the counters first and scans last at the end, so scanning
for the measurement is not charged to the session. `Snapshot::take_light()`
exists for the 1 s live labels and avoids the expensive PSS walk.

### Frame pacing

The frame test uses a Cairo strip in the benchmark window; the multi-surface test
adds animated toplevels that repaint every frame. Frames are collected from GDK's
`FrameClock` history.

- **Intervals, fps and drops** prefer `presentation_time`, which is when the
  frame was actually scanned out. If fewer than three presentation timestamps are
  available, the tool falls back to `frame_time` and records
  `pacing_source: "frame_clock"`.
- **Advertised refresh** is the median of the non-zero refresh intervals the
  compositor reports, falling back to the panel refresh known at startup.
- **fps** is `(frames - 1) / span`.
- **p50/p95/max** are computed from frame deltas.
- **Cadence** is the rate the compositor actually settled into. If the median
  interval is more than 1.3× the advertised period, the median interval becomes
  the cadence; otherwise the advertised period does. This is why a clean 60 fps
  on a 120 Hz panel is reported as a 60 Hz cadence, not as every other frame
  dropped.
- **Dropped frames** are counted against the observed cadence:
  `sum(max(round(delta / cadence) - 1, 0))`.
- **Panel pacing** is the cross-compositor number: delivered frames as a share
  of the advertised panel refresh (`fps / advertised`, clamped to 0..1).
  `dropped_vs_panel` counts misses against the panel period and
  `p95_over_panel` expresses the p95 interval in panel periods. A deliberate
  60 fps on a 120 Hz panel is 50 %, which is what it is; `dropped` against the
  observed cadence would call it perfect.
- **Workload target:** `--target-hz HZ` (or `--profile reference`, which
  defaults to 60) makes the client skip frame-clock ticks that arrive sooner
  than the target period. Only drawn frames are collected, so the skipped
  history is not counted as delivered work. `workload_hz` records the target;
  `null` means native refresh.
- **`comp ms/frame`** is compositor CPU time in the test window divided by
  presented frames. It is independent of GPU speed and is the main "render path
  weight" figure. `comp_cpu_pct_load` is the same CPU as a share of one core
  over the measured wall clock. `comp_cpu_pct_at_60hz` scales that measured CPU
  by `60 / panel_hz`, so a 120 Hz run is not flattered. Scaling total CPU, not
  CPU per presented frame, matters when the client is throttled: a compositor
  can keep doing per-vblank work while the client presents half as often, and
  `comp_ms/frame × 60` would then report twice the cost it would have on a
  60 Hz panel.
- **`app ms/frame`** is the benchmark's own CPU per frame. If it approaches
  `comp ms/frame`, the run measured a slow client as much as a slow compositor.
- **Session CPU** for the window, the benchmark's own share, and the session
  figure with the benchmark subtracted are all stored.
- **Measured duration** is stored as `measured_s`; `dur_s` remains the requested
  `--frames` value. Power and CPU deltas use the measured wall clock.
- **GPU time** is read for the compositor and for the benchmark client.
  `gpu_ms_per_frame` keeps its v3 meaning (compositor only);
  `comp_gpu_ms_per_frame` and `app_gpu_ms_per_frame` make the two components
  explicit. Consumers that want total GPU work add them when both exist.

### GPU time

DRM client counters are read from `/proc/<pid>/fdinfo` for the compositor and
for the benchmark client. Values are deduplicated per `drm-client-id` with
`max` **per engine**, never summed across file descriptors: several file
descriptors of one client report identical numbers. Between two samples, each
engine is handled separately:

- if `drm-engine-<engine>` nanoseconds advanced, use that delta;
- otherwise, if `drm-total-cycles-<engine>` advanced, `busy =
  Δdrm-cycles / Δtotal-cycles × wall_time` for that engine;
- sum the per-engine contributions across engines and clients.

Keeping engines separate matters: `rcs`, `bcs`, `vcs` and `ccs` can all run in
the same window, and taking the maximum silently reports only the busiest one.
`drm-engine-capacity-*` is a ring count, not a duration, and is ignored.

The result is an estimate of GPU time attributable to the compositor and its
clients. Drivers that expose neither counters nor engine times produce `null`,
not zero. GPU memory uses `drm-total-*`/`drm-memory-*` for held buffers and
`drm-shared-*` to derive the private part.

### Power

- **`psys`** is the whole SoC (CPU + iGPU + fabric) and the best battery proxy.
  **`pkg`** is `package-0`, the CPU package alone. Domains are found by name,
  not index, and their wraparound width is read for counter rollover.
- **RAPL** `energy_uj` is root-only on most systems. The tool first reads the
  file directly; if that fails and `WG_BENCH_NO_SUDO` is unset, it tries
  `sudo -n cat` (non-interactive, never prompts). Without either, power fields
  are `null`.
- **Battery draw** is `power_now` and is only used while the battery reports
  `Discharging`; many embedded controllers report charge power when plugged in.
- Power samples bracket only the sleep between the idle or frame snapshots, and
  watts are computed from the measured `Instant` difference, never from the
  requested CLI duration. A delta shorter than 0.5 s is rejected.
- On a root-only-RAPL machine the `sudo -n` fork needed to read the counters
  falls inside the CPU window; it is small, and that machine's power fields are
  `null` anyway.
- RAPL domains and readability are cached for the process lifetime.

### Peaks and the CPU timeline

A sampler thread wakes every 25 ms and reads the cgroup's `memory.stat` and
`cpu.stat` directly. This keeps GTK's main loop from smoothing out spikes and
avoids the root-only `memory.peak` reset. CPU points are emitted at most every
100 ms; the timeline is capped at 12,000 points (about 20 minutes).

The average CPU across the run uses the trapezoid rule on those points, matching
the fact that each sample describes the interval ending at its timestamp. The
peak is kept separately. The timeline shows *when* the load happened, and phase
marks delimit idle, single, windows and multi.

### Protocol scan and blur

The registry scan runs on its own `wayland-client` connection after the window is
up. It records every global and its version. Feature coverage is scored only on
the **Core / Rendering / Desktop** tiers; wlroots-only and optional protocols are
listed but not scored, so the number does not reward a design choice. A global
advertised below the useful version is counted in `supported` (it is advertised)
but marked `stale` per item and in the `features.stale` count. The run JSON
contains the whole snapshot under `features`; a failed scan writes `null`, not
an empty block.

The blur probe binds `ext_background_effect_manager_v1` and reads its
capabilities event. It never creates an `ext_background_effect_surface_v1`: GTK
already owns one per toplevel, and a second one is a protocol error that kills the
connection. The header chip reflects `active`, `not supported` or `unknown`, and
clicking it toggles a solid window background. The state is recorded as
`blur: "on"` or `"off"`; blur is real compositor work, so reports warn when a
label mixes both states.

## Checks and confidence

Every run stores a list of checks with a name, a boolean and a human-readable
detail.

| check | what it proves |
|---|---|
| `session cgroup` | the measurement scope covers this desktop, not the user slice |
| `compositor process` | a compositor process was found |
| `desktop identity` | `XDG_CURRENT_DESKTOP` agrees with the socket owner |
| `display refresh` | refresh is known from EDID or the compositor |
| `RAPL power` | at least one RAPL domain is readable |
| `battery discharging` | battery power is meaningful |
| `clean session` | no process outside the declared stack holds 150 MiB or more |
| `quiet cpu` | unrelated CPU stayed at or below 5 % of one core |
| `presentation feedback` | frame timings came from `presentation_time` |
| `GPU counters` | DRM fdinfo exposed usable counters |

Confidence is derived, not estimated:

- **low** when `session cgroup` or `compositor process` fails;
- **medium** when `presentation feedback`, `display refresh`, `clean session` or
  `quiet cpu` fails;
- **high** otherwise.

The other checks are still recorded and surfaced by `--report`, but a missing
battery or RAPL domain does not make CPU or frame numbers worse.

### Pre-run access probe

The placeholder in the window runs the same source checks before the first
benchmark, so a machine that cannot provide a metric says so instead of drawing
a zero. It distinguishes a missing cgroup v2 mount (cgroup v1 distros), a
cgroup v2 user-slice fallback, an unresolved compositor, an unreadable
`smaps_rollup` or `schedstat` (`hidepid`, permissions), root-only RAPL, and
missing PSI. The result is always shown: a green tick when every source is
readable, otherwise a red cross with the metric, path or condition and a short
fix, such as the RAPL udev rule or a cgroup v2 session scope. The probe is
deferred until after the first frame because the RAPL check may fork `sudo -n`;
`--check` prints the same conditions from the terminal.

## Result schema

The schema is versioned with `schema_version` (currently **3**). Every phase is a
flat `#[serde(default)]` object, so a new optional field does not bump the
version and old v3 files keep loading. Files written by an older schema are
ignored with an explicit message, because they measured different things under
the same names.

Top-level fields:

| group | fields |
|---|---|
| identity | `schema_version`, `label`, `compositor`, `compositor_version`, `compositor_pid`, `compositor_source` |
| context | `when`, `desktop`, `desktop_env`, `cgroup`, `cgroup_exact`, `cgroup_kind`, `stack_style`, `profile`, `env` |
| validity | `blur`, `confidence`, `checks`, `clean`, `contaminants` |
| coverage | `features` (`null` when the registry scan failed) |
| phases | `idle`, `windows`, `render`, `multi`, `peaks` |

`cgroup_kind` is `login-session`, `user-manager`,
`login-session+user-manager` or `user-slice`;
`stack_style` is `integrated` or `modular`; `profile` is `native` or
`reference`. An old v3 file has empty strings here, and `--report` reconstructs
the cgroup kind from the stored cgroup path.

Numeric fields added after v3 are `Option`/`#[serde(default)]`, so an old file
that never measured them loads as `null` and prints `-`, not `0`. This applies
to `idle.other_cpu_pct`, `render.self_cpu_s`,
`render.session_cpu_excl_self_s`, `windows.per_window_mib`, `idle.measured_s`,
`render.measured_s`, and the `comp_gpu_ms_per_frame` /
`app_gpu_ms_per_frame` split. `idle` also stores `stack_cpu_pct`,
`unrelated_cpu_pct`, `stack_pss_mib`, `stack_wakeups_per_s`, `self_pss_mib`,
`mem_excl_self_mib`, plus `stack_declared` and a `stack_processes` list of
`{pid, name, pss_mib, cpu_pct}`. `render`/`multi` store `workload_hz`,
`pacing_vs_panel`, `dropped_vs_panel`, `p95_over_panel`, `comp_cpu_pct_load`
and `comp_cpu_pct_at_60hz`. `windows` also stores optional
`baseline_drift_mib` and `post_close_drift_mib`, `delta_valid`, and the signed
`delta_mib`; `residual_mib` keeps its v3 meaning. `features` stores `stale` and
per-item `{name, label, tier, version, since, stale}`.

`env` carries CPU and GPU identity, cores/threads, Mesa and kernel versions,
display connector/mode/refresh/scale/output count/brightness, power profile,
governor, EPP, temperature, current clock, throttle flag and config hash. The
`display` block is what `--report` and `--compare` use to warn that runs are not
comparable.

Automatic files land in
`$XDG_STATE_HOME/wayland-compositor-bench/results/<label>-<timestamp>.json`
(usually `~/.local/state/...`). `--json [FILE]` picks an explicit path and
implies `--run`; `--run` exits non-zero if the file cannot be written. The label
defaults to the resolved desktop and is slugged to ASCII lowercase with dashes.

## Reporting and comparison

`--report` and `--compare` accept directories, JSON files, or label substrings.
With no arguments they search `./results` and the XDG results directory. Results
are grouped by label; a filter matches a label substring first and falls back to
a path substring. Only schema v3 files are read.

- **Medians, not means.** Each cell is the median of the runs with that label.
  When runs disagree by more than 1 %, the cell shows `[min-max]`; a single run
  shows just the value.
- **Noise floor.** `Stats::noise()` is half the observed range, floored at the
  scaled median absolute deviation (`1.57 × MAD / √n`) and at 2 % of the
  median. `--compare` reports "same (noise ±…)" when the difference is within
  that floor, otherwise the percentage change. MAD keeps one outlier from
  inflating the spread the way the range can.
- **Enough runs.** Fewer than three runs per label cannot establish a spread, so
  every row is labelled `anecdotal (n a vs b)` instead of declaring a winner.
  `--strict` turns an anecdotal or otherwise incomparable comparison into a
  non-zero exit.
- **Missing is not zero.** A file without a newer optional field prints `-`;
  `--compare` prints `not measured` for that row and an absolute delta with
  `n/a` when the baseline median is zero, rather than a fabricated `+0 %`.
- **Context warnings.** `--report` warns when a label mixes displays, config
  hashes, blur states, cgroup kinds, stack styles, workload profiles,
  user-slice scopes, contaminated sessions, compositor fallback resolution or a
  stale desktop environment. `--compare` additionally warns on different power
  settings and presentation-feedback sources. It also prints the failing checks
  behind a `medium` or `low` confidence.
- **Skipped files** (unreadable, wrong schema) are counted and reported.

There is deliberately no single score. Any scalar over arbitrary weights
(CPU, RAM, power, features) hides the trade-offs behind the number, so the
tables expose the axes and the reader chooses what matters.

## Window design

The window is a GTK4 application with a transparent, blurred card. Its layout
follows the measurement:

- **Live system panel:** session identity and hardware, with CPU and RAM
  refreshed every second from cheap cgroup files; PSS, GPU memory, pressure and
  package temperature refresh every fifth tick.
- **Six-axis radar:** pacing against the panel refresh (one surface and the
  multi-surface load case), render cost (compositor CPU scaled to a common
  60 Hz panel), **stack** PSS and idle CPU, and protocol coverage.
  The memory axis is scaled against a fixed 8 GiB reference, not the machine's
  RAM, so the same stack lands in the same place on every machine; the CPU axes
  are shares of one core. All three cost axes are logarithmic because the
  interesting ranges are small: 50 MiB and 1.2 GiB stay visibly apart, so do
  0.3 % and 3 % idle CPU, and render cost spreads the 1–10 % band a compositor
  actually lives in over a third of the radius instead of two pixels. Every
  axis is drawn further-out-is-better, including the cost axes,
  which plot `1 - cost` and therefore print a number whose good direction is
  *down*. Each vertex carries the direction its own number should move (`↓` for
  render cost, stack RAM and stack CPU, `↑` for pacing and features), so the
  chart never labels a cost "higher is better" and asks the reader to hold two
  conventions at once. The marker is drawn as a path rather than a glyph,
  because the Cairo toy font stack has no guaranteed arrow coverage and a
  missing glyph would become a box on exactly the axes that need explaining.
  Hovering a label highlights that axis and explains what it plots and which
  direction is good.
- **CPU timeline:** the 100 ms session CPU samples, drawn left to right with
  phase boundaries, so a spike while windows opened is visible instead of folded
  into one number.
- **`TOP RAM CONSUMERS`:** a snapshot taken when the window opens, so a run never
  moves it. RSS is grouped by process name from `/proc/<pid>/status`, turning a
  dozen WebKit children into one stripe. Clicking a stripe highlights it.
- **Protocol list:** every global with its version, always visible, with the
  scored coverage summary above it.
- **Blur chip:** reports and toggles `ext-background-effect-v1` support.

The benchmark surface is a 126 px Cairo strip. It is intentionally a real
client: it counts toward the compositor's cost, which is the only way to animate
something the compositor must composite. `app ms/frame` makes the client's own
cost visible.

## Non-goals

- **Input-to-photon latency:** needs a photodiode or evdev injection plus
  capture, and is not a property of a compositor alone.
- **GPU utilisation percentage:** drivers do not expose it. Per-client DRM
  counters estimate GPU *time*, not a utilisation percentage.
- **HDR, colour accuracy, VRR behaviour, multi-monitor:** only the first
  connected output is measured.
- **XWayland clients and the X server cost.**
- **Video, WebGL and games:** the built-in workloads are Cairo and animated
  toplevels. A fullscreen GL client would have no presentation feedback from this
  tool.
- **Power on machines with no battery and no readable RAPL.**

## Build, test and release

- Rust 2021, MSRV **1.92** (`rust-version` in `Cargo.toml`); GTK 4.16+ is needed
  because the CSS uses `backdrop-filter`.
- Dependencies: `gtk4` (`gtk` 0.11), `glib`, `serde`, `serde_json`,
  `wayland-client` 0.31, `wayland-protocols` 0.32.
- Release profile: `opt-level = 2`, binary stripped.
- CI checks formatting, runs Clippy with `-D warnings`, runs tests, builds the
  release binary, verifies the declared MSRV, runs `cargo audit`, builds the Arch
  package and the Debian 13 package, and publishes the binary, packages, source
  tarball and `SHA256SUMS` on tagged releases.
- `Cargo.toml` is the source of truth for the version; CI checks that
  `PKGBUILD`, `Cargo.lock` and the tag agree.
- Packaging installs `/usr/bin/wayland-compositor-bench`, a `.desktop` launcher
  and a hicolor SVG icon.

## Known trade-offs

- The benchmark's window is part of the measured session. That is deliberate,
  but it means `comp ms/frame` includes compositing the benchmark itself.
- The GPU figure is an estimate based on driver counters and can be unavailable.
- Working-set memory intentionally ignores page cache, so it measures cost, not
  everything the session touched.
- The 25 ms sampler is cheap but not free; the sampler thread and its file reads
  are part of the session CPU. The benchmark's own share is recorded so the
  honest figure remains available.
- Only the first connected output is used for display context; multi-monitor
  power and memory are not attributed per output.
