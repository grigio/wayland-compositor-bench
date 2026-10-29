# wayland-compositor-bench

[![CI](https://github.com/grigio/wayland-compositor-bench/actions/workflows/ci.yml/badge.svg)](https://github.com/grigio/wayland-compositor-bench/actions/workflows/ci.yml)

Measure the Wayland session you are logged into: how much memory and CPU the
compositor costs at rest, what it costs per presented frame, and how much power
the machine draws while it sits there. One Rust binary, no scripts, no Python.

Tested on: Xiaomi Book Pro 14 (Core Ultra X7 358H, Arc B390, 3120x2080 eDP).

## Quick start

```bash
cargo build --release

./target/release/wayland-bench            # window, press Start
./target/release/wayland-bench --check    # can this session be measured?
./target/release/wayland-bench --run --json
```

`--run` measures immediately and **exits when done**, so it can sit in a script.
The JSON lands in `results/<label>-<timestamp>.json`; the label defaults to the
desktop the running compositor belongs to (`GNOME`, `labwc`, …), falling back to
`XDG_CURRENT_DESKTOP` when the two agree, so a session flavour like
`ubuntu:GNOME` is kept (`--label` to override).

Log out, log into the next compositor, run it again. Three runs per compositor:
the difference between identical runs is usually bigger than the one you are
chasing.

```bash
./target/release/wayland-bench --report              # medians of results/*.json
./target/release/wayland-bench --report ~/runs/labwc # a directory, files, or labels
./target/release/wayland-bench --compare labwc gnome # side by side, with a noise floor
./target/release/wayland-bench --compare labwc gnome ~/runs # …reading a directory
```

Flags: `--idle S` (20), `--frames S` per frame test (6), `--windows N` (6),
`--settle S` max wait for quiet (45). `--help` for the rest.

## Packages

Tagged releases carry the Linux binary, an Arch package and a Debian package,
plus the source tarball and `SHA256SUMS`:

```bash
sudo pacman -U wayland-compositor-bench-*.pkg.tar.zst     # Arch
sudo apt install ./wayland-compositor-bench_*_amd64.deb   # Debian / Ubuntu
```

Both packages install `/usr/bin/wayland-bench`. The Debian package is built on
Debian 13 (GTK 4.18) so the dependency floor is honest; the Arch package is
built by `PKGBUILD` in the repository root.

## What it measures

| phase | what you get |
|---|---|
| **idle** | session working set, compositor PSS, session and compositor CPU, compositor wakeups/s, package and battery power |
| **toplevels** | the same session with N extra windows open: working set delta, GPU memory held by client and compositor |
| **frames (single)** | one animated surface: fps, cadence, p50/p95/max interval, missed frames, compositor CPU/frame, GPU/frame, app CPU/frame |
| **frames (multi)** | the same surface plus N animated toplevels — what a real desktop does all day |
| **protocols** | every `wl_registry` global with its version, grouped by tier |
| **peaks** | 25 ms sampler for RAM and CPU spikes during the run |

Everything is read from `/proc`, the session's cgroup v2 and sysfs:

- **CPU** from `schedstat` in nanoseconds, summed over threads — not
  `utime+stime`, which is quantised to 10 ms ticks and cannot see a lean
  compositor at 120 Hz.
- **Frame intervals, fps and drops** from `presentation_time`: when the frame
  was actually scanned out, not when the client painted it.
- **GPU time** from the DRM client counters in `/proc/<pid>/fdinfo`, scaled by
  the busy/total cycle ratio. Deduplicated per `drm-client-id`: several fds of
  one client report identical numbers.
- **The compositor** is the process holding the `WAYLAND_DISPLAY` listening
  socket, resolved through `/proc/net/unix` and `/proc/*/fd`. Name matching
  picks the wrong process as soon as a nested compositor is running.
- **The desktop** is derived from that process, not from
  `XDG_CURRENT_DESKTOP`: GNOME Shell is a systemd user service, so it inherits
  the user manager's environment and can still claim the desktop you logged out
  of. A stale environment is recorded as a failed check and reported, never used
  to name the run.
- **Memory** as `anon + shmem` (the working set). Page cache is excluded: it
  follows what the session read, not what the desktop costs. GPU buffers are
  reported separately, because dmabufs never enter the cgroup.

## Reading the numbers

- `comp ms/frame` is the honest "how heavy is this compositor's render path"
  number: CPU time per presented frame, independent of GPU speed. Lower is
  leaner.
- `app ms/frame` tells you whether the benchmark itself was the bottleneck. If
  it is close to `comp ms/frame`, the run measured a slow client as much as a
  slow compositor.
- `fps` next to `cadence`: 60 fps at a 60 Hz cadence on a 120 Hz panel means the
  scene is too heavy for 120, **not** that frames are being dropped. Drops are
  counted against the observed cadence for that reason.
- `RAM` is the working set, so a desktop that read 2 GiB of files does not look
  worse than one that read nothing.
- `psys` is the whole SoC (CPU + iGPU + fabric) — the best battery proxy. `pkg`
  is the CPU package alone, `bat` is what the battery reports.
- Coverage counts only the **Core / Rendering / Desktop** tiers. wlroots-only
  and optional protocols are listed with their versions but not scored;
  otherwise the number rewards a design choice.
- There is no single score. Any scalar over arbitrary weights can be quoted
  without its trade-offs; read the axes.

Every run carries a `confidence` of `high`, `medium` or `low`, derived from
checks stored in the JSON: session scope found, compositor resolved by socket,
presentation feedback, display refresh known, session clean. `--report` prints
what downgraded a run.

## Making runs comparable

1. **Same output, refresh and scale.** Idle power and RAM scale with pixels and
   Hz; 60 vs 120 Hz is the largest single variable on this panel. The display
   block is in every JSON, and `--report`/`--compare` warn when they differ.
2. **Same power state.** On battery, discharging, same platform profile and
   brightness. Plugging in changes both the drain and the thermals.
3. **Clean session.** The tool flags processes over 150 MiB; close them. Lazy
   portals arrive seconds after login, so the run waits for the session to go
   quiet instead of sleeping a fixed time.
4. **Compare within a tier.** labwc is a WM; GNOME is a WM plus a shell, search,
   notifications and portals. Measure bare sessions and full desktops, and say
   which one you are reporting.
5. **Three runs, and use `--compare`.** It subtracts the observed spread and
   refuses to call a difference smaller than the noise.

The benchmark's own window is a real client: it counts toward the compositor's
cost. That is intentional — it is the only way to animate something the
compositor must composite.

## Permissions

RAPL `energy_uj` is `0400 root` on most distros. Nothing here needs root; power
is optional. To read it without sudo:

```bash
# /etc/udev/rules.d/99-rapl-read.rules
SUBSYSTEM=="powercap", ACTION=="add", RUN+="chmod a+r %S%p/energy_uj"
```

```bash
sudo udevadm control --reload && sudo udevadm trigger --subsystem-match=powercap
```

`--check` says whether RAPL is readable. Without it, power columns are `null`,
not zero.

## What this does not measure

- **Input-to-photon latency.** Needs a photodiode or evdev injection plus
  capture; not a property of a compositor alone.
- **GPU utilisation percentage.** Drivers do not expose it. The per-client
  counters give GPU time for the compositor and its clients — an estimate, and
  `null` on drivers that expose neither cycles nor engine times.
- **Video, WebGL, games.** Built-in workloads only: a Cairo strip and animated
  toplevels. A fullscreen GL client you can run yourself is not measured either
  — the tool would have no presentation feedback for it.
- **HDR, colour accuracy, VRR behaviour, multi-monitor.** First connected output
  only.
- **XWayland** clients and the cost of the X server.
- **Power on machines with no battery and no readable RAPL.**

## License

GPL-3.0-only. See [LICENSE](LICENSE).

## Donations

If you find this project helpful, please consider making a donation to support its development.

- **Monero**: `88LyqYXn4LdCVDtPWKuton9hJwbo8ZduNEGuARHGdeSJ79BBYWGpMQR8VGWxGDKtTLLM6E9MJm8RvW9VMUgCcSXu19L9FSv`
- **Bitcoin**: `bc1q6mh77hfv8x8pa0clzskw6ndysujmr78j6se025`
