# Changelog

All notable changes to this project. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[semantic versioning](https://semver.org/spec/v2.0.0.html).

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
