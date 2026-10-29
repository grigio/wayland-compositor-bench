/*
 * Compositor capability scan.
 *
 * Enumerates the globals advertised on GDK's existing Wayland connection and
 * reports whether ext-background-effect-v1 offers blur. It deliberately does
 * not create an ext_background_effect_surface_v1: GTK already owns one per
 * toplevel and a second one is a protocol error that kills the connection.
 */
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <wayland-client.h>

#include "ext-bg-effect.h"

#define WBG_MAX_NAMES 32768

static char g_names[WBG_MAX_NAMES];
static size_t g_names_len;
static struct ext_background_effect_manager_v1 *g_manager;
static uint32_t g_caps;

static void registry_global(void *data, struct wl_registry *reg, uint32_t name,
                            const char *iface, uint32_t version) {
    (void)data;
    /* One line per global: "<interface>\t<version>". The version is what tells
     * an implementation from a stub, so it has to survive the scan. */
    int n = snprintf(g_names + g_names_len, WBG_MAX_NAMES - g_names_len,
                     "%s\t%u\n", iface, version);
    if (n > 0 && g_names_len + (size_t)n < WBG_MAX_NAMES) {
        g_names_len += (size_t)n;
    }
    if (strcmp(iface, ext_background_effect_manager_v1_interface.name) == 0) {
        g_manager = wl_registry_bind(reg, name, &ext_background_effect_manager_v1_interface, 1);
    }
}

static void registry_global_remove(void *data, struct wl_registry *reg, uint32_t name) {
    (void)data;
    (void)reg;
    (void)name;
}

static const struct wl_registry_listener registry_listener = {
    .global = registry_global,
    .global_remove = registry_global_remove,
};

static void manager_capabilities(void *data, struct ext_background_effect_manager_v1 *m,
                                 uint32_t flags) {
    (void)data;
    (void)m;
    g_caps = flags;
}

static const struct ext_background_effect_manager_v1_listener manager_listener = {
    .capabilities = manager_capabilities,
};

/*
 * Fills `out` with "<interface>\t<version>" lines.
 * Returns 1 if blur is available, 0 if not supported, -1 on error.
 * A negative return still fills `out` when the registry roundtrip worked.
 */
int wbg_scan(void *display, char *out, int outlen) {
    struct wl_display *dpy = (struct wl_display *)display;
    if (!dpy) {
        return -1;
    }
    g_names_len = 0;
    g_names[0] = '\0';

    struct wl_registry *reg = wl_display_get_registry(dpy);
    if (!reg) {
        return -1;
    }
    wl_registry_add_listener(reg, &registry_listener, NULL);
    if (wl_display_roundtrip(dpy) < 0) {
        return -1;
    }

    int rc = 0;
    if (g_manager) {
        ext_background_effect_manager_v1_add_listener(g_manager, &manager_listener, NULL);
        if (wl_display_roundtrip(dpy) < 0) {
            return -1;
        }
        rc = (g_caps & EXT_BACKGROUND_EFFECT_MANAGER_V1_CAPABILITY_BLUR) ? 1 : 0;
    }

    if (out && outlen > 0) {
        size_t n = g_names_len < (size_t)(outlen - 1) ? g_names_len : (size_t)(outlen - 1);
        memcpy(out, g_names, n);
        out[n] = '\0';
    }
    return rc;
}
