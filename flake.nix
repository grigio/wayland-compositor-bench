{
  description = "Measure a Wayland session: memory, cost per frame and idle power";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];

      forAllSystems = f:
        nixpkgs.lib.genAttrs systems (system: f {
          inherit system;
          pkgs = import nixpkgs { inherit system; };
        });
    in
    {
      packages = forAllSystems ({ system, pkgs, ... }: {
        default = pkgs.callPackage ./default.nix { };
        wayland-compositor-bench = self.packages.${system}.default;
      });

      apps = forAllSystems ({ system, ... }: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/wayland-compositor-bench";
          meta = {
            description = "Measure a Wayland session: memory, cost per frame and idle power";
          };
        };
      });

      devShells = forAllSystems ({ system, pkgs }: {
        default = pkgs.mkShell {
          # Pull in the package's native build inputs (GTK, pkg-config, …)
          # so `cargo build` works inside the shell as it does in the build.
          inputsFrom = [ self.packages.${system}.default ];
          packages = with pkgs; [ cargo clippy rustfmt ];
        };
      });

      # `nixpkgs.overlays = [ inputs.wcb.overlays.default ];`
      overlays.default = final: _prev: {
        wayland-compositor-bench = final.callPackage ./default.nix { };
      };

      # `programs.wayland-compositor-bench.enable = true;` in configuration.nix.
      nixosModules.default =
        { config, lib, pkgs, ... }:
        let
          cfg = config.programs.wayland-compositor-bench;
        in
        {
          options.programs.wayland-compositor-bench = {
            enable = lib.mkEnableOption "wayland-compositor-bench, a Wayland session benchmark";

            package = lib.mkOption {
              type = lib.types.package;
              # Prefer the package from the overlay (or nixpkgs, eventually);
              # fall back to this flake's derivation so the module also works
              # without adding the overlay.
              default = pkgs.wayland-compositor-bench or (pkgs.callPackage ./default.nix { });
              defaultText = lib.literalExpression "pkgs.wayland-compositor-bench";
              description = "The wayland-compositor-bench package to install.";
            };
          };

          config = lib.mkIf cfg.enable {
            environment.systemPackages = [ cfg.package ];
          };
        };
    };
}
