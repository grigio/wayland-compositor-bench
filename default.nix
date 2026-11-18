# Nix derivation for wayland-compositor-bench.
#
# Usable with callPackage, from a flake (see flake.nix) or directly:
#
#   nix-build -E 'with import <nixpkgs> {}; callPackage ./default.nix {}'
#
# Cargo.toml is the single source of truth for the version; this file reads it
# at evaluation time so the two cannot drift apart.

{ lib
, rustPlatform
, pkg-config
, desktop-file-utils
, gtk4
, wayland
}:

let
  pname = "wayland-compositor-bench";
  manifest = builtins.readFile ./Cargo.toml;
  versionLine = lib.head (lib.filter (l: lib.hasPrefix "version = \"" l) (lib.splitString "\n" manifest));
  versionMatch = lib.match "version = \"([0-9]+\\.[0-9]+\\.[0-9]+)\".*" versionLine;
  version = if versionMatch == null
    then throw "wayland-compositor-bench: cannot read version from Cargo.toml"
    else builtins.head versionMatch;

  # The checkout also contains build output (target/), packaged trees (pkg/),
  # release artifacts (dist/), recorded results and a local Cargo cache. None
  # of that belongs in the Nix source, and copying it would bloat every build.
  root = ./.;
  rootStr = toString root;
  ignoredTopLevel = [ ".cargo-home" ".git" "dist" "pkg" "results" "target" ];
  src = lib.cleanSourceWith {
    src = root;
    filter = path: type:
      let
        relative = lib.strings.removePrefix (rootStr + "/") (toString path);
        topLevel = lib.head (lib.splitString "/" relative);
      in
      ! (lib.elem topLevel ignoredTopLevel) && lib.cleanSourceFilter path type;
  };
in
rustPlatform.buildRustPackage {
  inherit pname version src;

  cargoLock.lockFile = ./Cargo.lock;

  nativeBuildInputs = [ pkg-config desktop-file-utils ];
  buildInputs = [ gtk4 wayland ];

  # The test suite is headless (see README "Development"); keep cargo test in
  # the build so a broken release cannot be packaged.
  doCheck = true;

  postInstall = ''
    install -Dm644 ${src}/packaging/wayland-compositor-bench.desktop \
      $out/share/applications/wayland-compositor-bench.desktop
    install -Dm644 ${src}/packaging/wayland-compositor-bench.svg \
      $out/share/icons/hicolor/scalable/apps/wayland-compositor-bench.svg

    install -Dm644 ${src}/LICENSE $out/share/licenses/${pname}/LICENSE
    for doc in README.md DESIGN.md CHANGELOG.md; do
      install -Dm644 ${src}/$doc $out/share/doc/${pname}/$doc
    done
  '';

  meta = with lib; {
    description = "Measure a Wayland session: memory, cost per frame and idle power";
    homepage = "https://github.com/grigio/wayland-compositor-bench";
    license = licenses.gpl3Only;
    mainProgram = "wayland-compositor-bench";
    platforms = platforms.linux;
    # No maintainer listed upstream; keep the field absent rather than guessing.
  };
}
