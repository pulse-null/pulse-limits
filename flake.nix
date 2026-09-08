{
  description = "PulseLimits: your Claude plan limits as a retro patient monitor. On Linux a Waybar module and a command, no Swift.";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      # the version lives in the plugin header, where SwiftBar and `pulse-limits version` read it
      version = builtins.head (builtins.match ".*<swiftbar\\.version>v([0-9.]+)</swiftbar\\.version>.*"
        (builtins.readFile ./pulse-limits.1m.sh));
    in {
      packages = forAll (pkgs: {
        default = pkgs.stdenvNoCC.mkDerivation {
          pname = "pulse-limits";
          inherit version;
          src = ./.;
          nativeBuildInputs = [ pkgs.makeWrapper ];
          buildInputs = [ pkgs.python3 ];   # patchShebangs points bin/pulse-activity.py at it
          dontBuild = true;                 # scripts run as they are; the Swift helpers are macOS/Homebrew only
          installPhase = ''
            runHook preInstall
            install -Dm755 -t $out/libexec pulse-limits.1m.sh pulse-limits.5m.sh open-monitor.sh
            install -Dm644 -t $out/libexec panel.html
            install -Dm755 -t $out/libexec/bin bin/pulse-activity.py
            install -Dm755 -t $out/bin pulse-limits
            runHook postInstall
          '';
          # everything runs through the command, which hands its PATH down to the plugin
          postFixup = ''
            wrapProgram $out/bin/pulse-limits --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.jq pkgs.curl pkgs.python3 pkgs.procps pkgs.xdg-utils ]}
          '';
          meta = with pkgs.lib; {
            description = "Claude plan limits as a retro patient monitor: a Waybar module and a command";
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
