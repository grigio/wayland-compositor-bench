use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let xml = "protocols/ext-background-effect-v1.xml";

    // makepkg's default CFLAGS carry -flto=auto. gcc then writes *slim* LTO
    // objects into libwbg_blur.a, which lld — what rustc links with — cannot
    // read: the link fails with "undefined symbol: wbg_scan" even though the
    // archive is right there. Drop LTO before cc reads CFLAGS; the rest of the
    // distro hardening flags are kept.
    if let Ok(cflags) = std::env::var("CFLAGS") {
        let kept: Vec<&str> = cflags
            .split_whitespace()
            .filter(|f| !f.starts_with("-flto"))
            .collect();
        std::env::set_var("CFLAGS", kept.join(" "));
    }
    println!("cargo:rerun-if-env-changed=CFLAGS");

    // Generate the client bindings for ext-background-effect-v1 at build time,
    // so we do not depend on a wayland-protocols crate version that ships it.
    for (mode, name) in [
        ("client-header", "ext-bg-effect.h"),
        ("private-code", "ext-bg-effect.c"),
    ] {
        let dest = out.join(name);
        let status = Command::new("wayland-scanner")
            .args([mode, xml, dest.to_str().unwrap()])
            .status()
            .expect("wayland-scanner not found (install `wayland` + `wayland-protocols`)");
        assert!(status.success(), "wayland-scanner failed for {mode}");
    }

    cc::Build::new()
        .file("src/blur.c")
        .file(out.join("ext-bg-effect.c"))
        .include(&out)
        .warnings(false)
        .compile("wbg_blur");

    println!("cargo:rustc-link-lib=wayland-client");
    println!("cargo:rerun-if-changed=src/blur.c");
    println!("cargo:rerun-if-changed={xml}");
}
