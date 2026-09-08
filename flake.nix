{
  description = "PulseLimits: your Claude, Codex and Grok plan limits as a retro patient monitor. One Rust binary; on Linux a Waybar module and a command.";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
      packageOption = pkgs: nixpkgs.lib.mkOption {
        type = nixpkgs.lib.types.package;
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        defaultText = nixpkgs.lib.literalExpression "pulse-limits.packages.\${system}.default";
        description = "The pulse-limits package.";
      };
    in {
      packages = forAll (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "pulse-limits";
          inherit version;
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = [ pkgs.makeWrapper ];
          # The binary finds panel.html and the SwiftBar shims one folder up from itself, so it
          # lives in libexec/pulse-limits/bin with them, and bin/ carries a symlink; pgrep and
          # xdg-open are what it shells out to on Linux. The Swift helpers are macOS/Homebrew only.
          postInstall = ''
            mkdir -p $out/libexec/pulse-limits/bin
            mv $out/bin/pulse-limits $out/libexec/pulse-limits/bin/pulse-limits
            install -Dm644 -t $out/libexec/pulse-limits panel.html
            install -Dm755 -t $out/libexec/pulse-limits pulse-limits.1m.sh pulse-limits.5m.sh
            ln -s ../libexec/pulse-limits/bin/pulse-limits $out/bin/pulse-limits
            wrapProgram $out/libexec/pulse-limits/bin/pulse-limits --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.procps pkgs.xdg-utils ]}
          '';
          doCheck = false; # the unit tests bind local sockets; run `cargo test` in a checkout
          meta = with pkgs.lib; {
            description = "Claude, Codex and Grok plan limits as a retro patient monitor: a Waybar module, a terminal UI and a command";
            homepage = "https://github.com/pulse-null/pulse-limits";
            license = licenses.agpl3Plus;
            mainProgram = "pulse-limits";
            platforms = platforms.unix;
          };
        };
      });
      apps = forAll (pkgs: {
        default = {
          type = "app";
          program = "${self.packages.${pkgs.stdenv.hostPlatform.system}.default}/bin/pulse-limits";
          meta.description = "the pulse-limits command";
        };
      });

      # Home Manager: the package, the two settings files and the Waybar wiring that
      # `pulse-limits bar on` cannot do when the Waybar config is generated from Nix.
      homeManagerModules = rec {
        default = pulse-limits;
        pulse-limits = { config, lib, pkgs, ... }:
          let
            cfg = config.programs.pulse-limits;
            inherit (lib) mkOption mkEnableOption mkIf mkAfter literalExpression types;
            # What `pulse-limits bar on` writes and prints on Linux (src/bar.rs); keep them equal.
            module = {
              exec = "${cfg.package}/bin/pulse-limits waybar";
              return-type = "json";
              interval = 60;
              format = "{text} {icon}";
              format-icons = [ "󰪞" "󰪟" "󰪠" "󰪡" "󰪢" "󰪣" "󰪤" "󰪥" ];
              on-click = "${cfg.package}/bin/pulse-limits open";
              signal = 8;
              tooltip = true;
            };
            style = ''
              #custom-pulse-limits { min-width: 12px; margin: 0 7.5px; }
              #custom-pulse-limits.warn  { color: #ffb000; }
              #custom-pulse-limits.crit  { color: #ff5c5c; }
              #custom-pulse-limits.stale { color: #ffb000; }
              #custom-pulse-limits.dead  { color: #8c8c8c; }
            '';
          in {
            options.programs.pulse-limits = {
              enable = mkEnableOption "PulseLimits, the plan-limits ring in the bar";
              package = packageOption pkgs;
              providers = mkOption {
                type = types.listOf (types.enum [ "grok" "claude" "codex" ]); # KNOWN in src/providers/mod.rs
                default = [ ];
                example = [ "grok" "claude" ];
                description = ''
                  The enabled providers; the first is what the bar shows while none of their CLIs
                  runs. Empty leaves it to the binary: the first run enables the CLIs that have a
                  login on the machine and `pulse-limits provider NAME` toggles them. Non-empty
                  writes `~/.config/pulse-limits/providers`; a runtime toggle then replaces that
                  file until the next switch puts this list back.
                '';
              };
              theme = mkOption {
                type = types.nullOr (types.enum [ "crt" "modern" "cyber" "synth" "analog" ]); # THEMES in src/util.rs
                default = null;
                example = "synth";
                description = ''
                  The monitor theme, written to `~/.config/pulse-limits/theme`. Null leaves it to
                  `pulse-limits theme NAME` (crt until then); set, a runtime change lasts until
                  the next switch.
                '';
              };
              waybar = {
                enable = mkOption {
                  type = types.bool;
                  default = config.programs.waybar.enable;
                  defaultText = literalExpression "config.programs.waybar.enable";
                  description = ''
                    Wire the module into `programs.waybar`: `custom/pulse-limits` goes into
                    `settings.<bar>` and the end of `modules-<side>`, and the tones are appended
                    to `programs.waybar.style`. That option must then be text, not a path:
                    `builtins.readFile ./style.css` rather than `./style.css`.
                  '';
                };
                bar = mkOption {
                  type = types.str;
                  default = "mainBar";
                  description = "The key of `programs.waybar.settings` (the attribute-set form) that gets the module.";
                };
                side = mkOption {
                  type = types.enum [ "left" "center" "right" ];
                  default = "right";
                  description = "Which of `modules-left|center|right` the module is appended to.";
                };
              };
            };

            config = mkIf cfg.enable {
              home.packages = [ cfg.package ];
              assertions = [{
                assertion = cfg.waybar.enable -> config.programs.waybar.enable;
                message = "programs.pulse-limits.waybar.enable needs programs.waybar.enable";
              }];
              # `pulse-limits provider|theme` write through a rename, which swaps Home Manager's
              # link for a plain file; force puts the declared file back instead of stopping there.
              xdg.configFile."pulse-limits/providers" = mkIf (cfg.providers != [ ]) {
                text = lib.concatMapStrings (p: p + "\n") cfg.providers;
                force = true;
              };
              xdg.configFile."pulse-limits/theme" = mkIf (cfg.theme != null) {
                text = cfg.theme + "\n";
                force = true;
              };
              programs.waybar = mkIf cfg.waybar.enable {
                settings.${cfg.waybar.bar} = {
                  "custom/pulse-limits" = module;
                  "modules-${cfg.waybar.side}" = mkAfter [ "custom/pulse-limits" ];
                };
                style = mkAfter style;
              };
            };
          };
      };

      # NixOS: the command for every user. The bar item is per user and lives in the Home
      # Manager module.
      nixosModules = rec {
        default = pulse-limits;
        pulse-limits = { config, lib, pkgs, ... }:
          let cfg = config.programs.pulse-limits;
          in {
            options.programs.pulse-limits = {
              enable = lib.mkEnableOption "the pulse-limits command system-wide";
              package = packageOption pkgs;
            };
            config = lib.mkIf cfg.enable { environment.systemPackages = [ cfg.package ]; };
          };
      };
    };
}
