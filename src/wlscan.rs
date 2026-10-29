//! What the running compositor actually advertises, with versions.
//!
//! Presence alone is a weak signal: every desktop ships `wl_compositor`. The
//! version of a global is what separates "implements fractional scale" from
//! "implemented the first draft of it", so the scan keeps it.
//!
//! Compatibility protocols are split by tier. Coverage is only scored over the
//! tiers that a modern desktop is expected to have — counting `zwlr_screencopy`
//! the same as `wp_presentation` turns the number into a vanity metric, and
//! wlroots-only protocols are a design choice, not a missing feature.

use std::ffi::c_void;

use gtk::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum BlurState {
    /// ext_background_effect_manager_v1 present, blur capability advertised.
    Active,
    /// No such global, or the capability is missing.
    Unsupported,
    /// Probe did not run, or the connection could not be reached.
    #[default]
    Error,
}

impl BlurState {
    pub fn label(self) -> &'static str {
        match self {
            BlurState::Active => "active — ext-background-effect-v1",
            BlurState::Unsupported => "not supported by this compositor",
            BlurState::Error => "probe failed",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Global {
    pub name: String,
    pub version: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Scan {
    pub globals: Vec<Global>,
    pub blur: BlurState,
}

extern "C" {
    fn wbg_scan(display: *mut c_void, out: *mut u8, outlen: i32) -> i32;
    fn gdk_wayland_display_get_wl_display(display: *mut c_void) -> *mut c_void;
}

pub fn scan(window: &gtk::Window) -> Scan {
    let display = <gtk::Window as gtk::prelude::WidgetExt>::display(window);
    let mut buf = vec![0u8; 32 * 1024];
    let rc = unsafe {
        let dpy = gdk_wayland_display_get_wl_display(display.as_ptr() as *mut c_void);
        wbg_scan(dpy, buf.as_mut_ptr(), buf.len() as i32)
    };
    let text = String::from_utf8_lossy(&buf)
        .split('\0')
        .next()
        .unwrap_or("")
        .to_string();
    Scan {
        globals: text
            .lines()
            .filter_map(|l| {
                let mut it = l.split('\t');
                let name = it.next()?.trim();
                if name.is_empty() {
                    return None;
                }
                Some(Global {
                    name: name.to_string(),
                    version: it.next().and_then(|v| v.trim().parse().ok()).unwrap_or(1),
                })
            })
            .collect(),
        blur: match rc {
            1 => BlurState::Active,
            0 => BlurState::Unsupported,
            _ => BlurState::Error,
        },
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    /// No desktop works without these.
    Core,
    /// How buffers, scaling and frames reach the screen.
    Rendering,
    /// Integration a full desktop is expected to provide.
    Desktop,
    /// wlroots heritage: useful, but not every compositor wants them.
    Wlroots,
    /// Niche or optional.
    Optional,
}

impl Tier {
    pub fn title(self) -> &'static str {
        match self {
            Tier::Core => "CORE",
            Tier::Rendering => "RENDERING",
            Tier::Desktop => "DESKTOP INTEGRATION",
            Tier::Wlroots => "WLROOTS HERITAGE",
            Tier::Optional => "OPTIONAL",
        }
    }

    /// Whether the tier counts towards the headline coverage number.
    pub fn relevant(self) -> bool {
        matches!(self, Tier::Core | Tier::Rendering | Tier::Desktop)
    }
}

pub struct Feature {
    pub name: &'static str,
    pub label: &'static str,
    pub tier: Tier,
    /// lowest version that is actually useful
    pub since: u32,
}

impl std::fmt::Debug for Feature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Feature")
            .field("name", &self.name)
            .field("tier", &self.tier)
            .field("since", &self.since)
            .finish()
    }
}

const fn f(name: &'static str, label: &'static str, tier: Tier, since: u32) -> Feature {
    Feature {
        name,
        label,
        tier,
        since,
    }
}

pub const FEATURES: &[Feature] = &[
    // core / window management
    f("wl_compositor", "compositor", Tier::Core, 4),
    f("wl_subcompositor", "subsurfaces", Tier::Core, 1),
    f("wl_shm", "shared memory buffers", Tier::Core, 1),
    f("wl_seat", "input seats", Tier::Core, 5),
    f("wl_output", "outputs", Tier::Core, 4),
    f("xdg_wm_base", "xdg-shell", Tier::Core, 2),
    f(
        "xdg_activation_v1",
        "activation / focus stealing",
        Tier::Desktop,
        1,
    ),
    f(
        "zxdg_decoration_manager_v1",
        "server-side decorations",
        Tier::Core,
        1,
    ),
    f("xdg_wm_dialog_v1", "modal dialogs", Tier::Desktop, 1),
    f("xwayland_shell_v1", "XWayland", Tier::Optional, 1),
    // rendering
    f(
        "zwp_linux_dmabuf_v1",
        "dmabuf (zero-copy)",
        Tier::Rendering,
        4,
    ),
    f(
        "wp_single_pixel_buffer_manager_v1",
        "solid colour buffers",
        Tier::Rendering,
        1,
    ),
    f("wp_viewporter", "cropping / scaling", Tier::Rendering, 1),
    f(
        "wp_fractional_scale_manager_v1",
        "fractional scaling",
        Tier::Rendering,
        1,
    ),
    f("wp_presentation", "presentation timing", Tier::Rendering, 1),
    f(
        "wp_linux_drm_syncobj_manager_v1",
        "explicit sync",
        Tier::Rendering,
        1,
    ),
    f(
        "wp_tearing_control_manager_v1",
        "tearing control",
        Tier::Optional,
        1,
    ),
    f(
        "wp_color_representation_manager_v1",
        "colour representation",
        Tier::Optional,
        1,
    ),
    f(
        "wp_alpha_modifier_v1",
        "per-surface alpha",
        Tier::Optional,
        1,
    ),
    f(
        "ext_background_effect_manager_v1",
        "native blur",
        Tier::Optional,
        1,
    ),
    // input
    f(
        "zwp_relative_pointer_manager_v1",
        "relative pointer",
        Tier::Desktop,
        1,
    ),
    f(
        "zwp_pointer_constraints_v1",
        "pointer constraints",
        Tier::Desktop,
        1,
    ),
    f(
        "zwp_pointer_gestures_v1",
        "touchpad gestures",
        Tier::Desktop,
        1,
    ),
    f(
        "wp_cursor_shape_manager_v1",
        "cursor shapes",
        Tier::Desktop,
        2,
    ),
    f(
        "zwp_text_input_manager_v3",
        "text input / IME",
        Tier::Desktop,
        1,
    ),
    f(
        "zwp_input_method_manager_v2",
        "input methods",
        Tier::Optional,
        1,
    ),
    f(
        "zwp_tablet_manager_v2",
        "graphics tablets",
        Tier::Optional,
        1,
    ),
    // clipboard & dnd
    f("wl_data_device_manager", "clipboard / DnD", Tier::Core, 3),
    f(
        "wp_primary_selection_device_manager_v1",
        "primary selection",
        Tier::Desktop,
        1,
    ),
    f(
        "ext_data_control_manager_v1",
        "clipboard managers",
        Tier::Desktop,
        1,
    ),
    // shell / panels
    f(
        "zwlr_layer_shell_v1",
        "layer shell (panels, launchers)",
        Tier::Wlroots,
        4,
    ),
    f(
        "zwlr_foreign_toplevel_manager_v1",
        "window list",
        Tier::Wlroots,
        3,
    ),
    f(
        "ext_foreign_toplevel_list_v1",
        "window list (new)",
        Tier::Desktop,
        1,
    ),
    f(
        "ext_workspace_manager_v1",
        "workspace protocol",
        Tier::Desktop,
        1,
    ),
    f(
        "zwlr_output_manager_v1",
        "output configuration",
        Tier::Wlroots,
        4,
    ),
    f(
        "zxdg_output_manager_v1",
        "output names / positions",
        Tier::Desktop,
        3,
    ),
    // capture & colour
    f("zwlr_screencopy_manager_v1", "screencopy", Tier::Wlroots, 3),
    f(
        "ext_image_copy_capture_manager_v1",
        "screen capture (new)",
        Tier::Desktop,
        1,
    ),
    f(
        "ext_output_image_capture_source_manager_v1",
        "capture sources",
        Tier::Desktop,
        1,
    ),
    f(
        "zwlr_gamma_control_manager_v1",
        "gamma / night light",
        Tier::Wlroots,
        1,
    ),
    // power & session
    f(
        "ext_idle_notifier_v1",
        "idle notification",
        Tier::Desktop,
        1,
    ),
    f(
        "wp_idle_inhibit_manager_v1",
        "idle inhibit",
        Tier::Desktop,
        1,
    ),
    f(
        "ext_session_lock_manager_v1",
        "session lock",
        Tier::Desktop,
        1,
    ),
    f(
        "zwlr_output_power_manager_v1",
        "output power",
        Tier::Optional,
        1,
    ),
    f(
        "wp_drm_lease_device_v1",
        "DRM leasing (VR)",
        Tier::Optional,
        1,
    ),
    f(
        "wp_security_context_manager_v1",
        "sandboxed clients",
        Tier::Optional,
        1,
    ),
];

#[derive(Clone, Debug)]
pub struct Item {
    pub feature: &'static Feature,
    /// `None` when the compositor does not advertise it; otherwise the version,
    /// which may still be below `since`.
    pub version: Option<u32>,
}

impl Item {
    pub fn supported(&self) -> bool {
        self.version.is_some()
    }

    /// Advertised, but too old to be worth much.
    pub fn stale(&self) -> bool {
        matches!(self.version, Some(v) if v < self.feature.since)
    }
}

pub struct Features {
    pub items: Vec<Item>,
    /// coverage over the tiers that matter
    pub supported: usize,
    pub total: usize,
    /// every tier, including wlroots heritage and optional protocols
    pub supported_all: usize,
    pub total_all: usize,
    /// globals the reference list does not know about
    pub extra: usize,
}

impl Features {
    pub fn percent(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.supported as f64 / self.total as f64 * 100.0
        }
    }
}

pub fn evaluate(globals: &[Global]) -> Features {
    let mut supported = 0;
    let mut supported_all = 0;
    let mut total = 0;
    let mut items = Vec::with_capacity(FEATURES.len());
    for feature in FEATURES {
        let version = globals
            .iter()
            .find(|g| g.name == feature.name)
            .map(|g| g.version);
        if version.is_some() {
            supported_all += 1;
            if feature.tier.relevant() {
                supported += 1;
            }
        }
        if feature.tier.relevant() {
            total += 1;
        }
        items.push(Item { feature, version });
    }
    let extra = globals
        .iter()
        .filter(|g| !FEATURES.iter().any(|f| f.name == g.name))
        .count();
    Features {
        items,
        supported,
        total,
        supported_all,
        total_all: FEATURES.len(),
        extra,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn globals(names: &[(&str, u32)]) -> Vec<Global> {
        names
            .iter()
            .map(|(n, v)| Global {
                name: n.to_string(),
                version: *v,
            })
            .collect()
    }

    #[test]
    fn coverage_ignores_wlroots_only_protocols() {
        // only wlroots heritage protocols: coverage of the relevant tiers is 0
        let f = evaluate(&globals(&[("zwlr_layer_shell_v1", 4)]));
        assert_eq!(f.supported, 0);
        assert_eq!(f.supported_all, 1);
    }

    #[test]
    fn versions_are_kept_and_stale_ones_flagged() {
        let f = evaluate(&globals(&[("wl_seat", 2)]));
        let item = f
            .items
            .iter()
            .find(|i| i.feature.name == "wl_seat")
            .unwrap();
        assert_eq!(item.version, Some(2));
        assert!(item.stale(), "wl_seat 2 is below the useful version");
    }

    #[test]
    fn unknown_globals_are_counted_not_scored() {
        let f = evaluate(&globals(&[
            ("wl_compositor", 5),
            ("my_compositor_thing", 1),
        ]));
        assert_eq!(f.extra, 1);
        assert_eq!(f.supported, 1);
    }
}
