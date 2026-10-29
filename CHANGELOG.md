# Changelog

All notable changes to this project. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[semantic versioning](https://semver.org/spec/v2.0.0.html).

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
