{
  description = "PulseLimits: your Claude and Codex plan limits as a retro patient monitor. One Rust binary; on Linux a Waybar module and a command.";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
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
            description = "Claude and Codex plan limits as a retro patient monitor: a Waybar module, a terminal UI and a command";
            homepage = "https://github.com/dnacenta/pulse-limits";
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
    };
}
