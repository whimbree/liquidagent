{
  description = "liquid — self-hosted AI agent and personal software factory";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      # Keep build junk and local state out of every derivation's src.
      excludedNames = [ "target" "node_modules" "dev-workspace" "result" ".git" ];
      sourceFilter = path: type: !(builtins.elem (baseNameOf path) excludedNames);
      cleanedSource = src: nixpkgs.lib.cleanSourceWith { inherit src; filter = sourceFilter; };
    in
    {
      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            clippy
            rustfmt
            rust-analyzer
            bun
            git
            # The Agent SDK's bundled CLI is dynamically linked and won't run
            # on NixOS; the harness resolves `claude` from PATH instead.
            claude-code
          ];
        };
      });

      packages = forAllSystems (pkgs: rec {
        liquid = pkgs.rustPlatform.buildRustPackage {
          pname = "liquid";
          version = "0.1.0";
          src = cleanedSource ./.;
          cargoLock.lockFile = ./Cargo.lock;
          meta = {
            description = "Self-hosted AI agent and personal software factory";
            license = pkgs.lib.licenses.mit;
            mainProgram = "liquid";
          };
        };
        default = liquid;

        # Spike 2 — Bun dependency vendoring as a fixed-output derivation,
        # pinned to bun.lock. First build fails with a hash mismatch: copy the
        # "got:" hash into outputHash. If the hash flaps across machines,
        # switch to bun2nix (see spikes/README.md).
        agent-deps = pkgs.stdenvNoCC.mkDerivation {
          name = "liquid-agent-deps";
          src = cleanedSource ./workspace/agent;
          nativeBuildInputs = [ pkgs.bun ];
          dontConfigure = true;
          buildPhase = ''
            export HOME=$TMPDIR
            bun install --frozen-lockfile --ignore-scripts --no-progress
          '';
          installPhase = "cp -r node_modules $out";
          dontFixup = true;
          outputHashMode = "recursive";
          outputHashAlgo = "sha256";
          outputHash = "sha256-QMECHQrJdH75Gigz//pm325Sm654fIQ72m0WPq+LrQ4=";
        };
      });
    };
}
