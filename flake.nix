{
  description = "liquid — self-hosted AI agent and personal software factory";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system:
        f (import nixpkgs {
          inherit system;
          # claude-code is unfree in nixpkgs; allow exactly it, nothing else.
          config.allowUnfreePredicate = pkg:
            builtins.elem (nixpkgs.lib.getName pkg) [ "claude-code" ];
        }));
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
            # Polyglot app backends (ADR 0002): the whiteboard proving case
            # runs `mix phx.server`.
            beamPackages.elixir
          ];
          # Hex without a network fetch: mix loads the nix-built archive from
          # MIX_PATH (the same trick nixpkgs' mixRelease uses), so vendored
          # hex deps compile offline.
          MIX_PATH = "${pkgs.beamPackages.hex}/lib/erlang/lib/hex/ebin";
        };
      });

      packages = forAllSystems (pkgs: rec {
        liquid = pkgs.rustPlatform.buildRustPackage {
          pname = "liquid";
          version = "0.1.0";
          src = cleanedSource ./.;
          cargoLock.lockFile = ./Cargo.lock;
          # The deploy-pipeline tests shell out to git (worktree, commit, diff);
          # git must be on PATH during the check phase, and HOME writable so
          # git doesn't reject a missing config dir.
          nativeCheckInputs = [ pkgs.git ];
          preCheck = "export HOME=$TMPDIR";
          meta = {
            description = "Self-hosted AI agent and personal software factory";
            license = pkgs.lib.licenses.mit;
            mainProgram = "liquid";
          };
        };
        default = liquid;

        # The agent harness, store-packaged for the NixOS module: sources plus
        # a symlink to the vendored node_modules.
        liquid-agent = pkgs.stdenvNoCC.mkDerivation {
          name = "liquid-agent";
          src = cleanedSource ./workspace/agent;
          dontBuild = true;
          installPhase = ''
            mkdir -p $out/share/liquid-agent
            cp *.ts protocol.json package.json tsconfig.json bun.lock $out/share/liquid-agent/
            ln -s ${agent-deps} $out/share/liquid-agent/node_modules
          '';
        };

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

      nixosModules = rec {
        liquidagent = import ./nix/module.nix self;
        default = liquidagent;
      };

      # A real NixOS VM test: boots the module (with the offline fake agent so
      # no credentials are needed) and verifies the service starts, binds its
      # port, and serves /api/health with the expected fields. Validates the
      # whole packaging + systemd + paths chain.  `nix build .#checks.x86_64-linux.module-boots`
      checks = forAllSystems (pkgs:
        nixpkgs.lib.optionalAttrs (pkgs.stdenv.hostPlatform.system == "x86_64-linux") {
          module-boots = pkgs.testers.runNixOSTest {
            name = "liquidagent-boots";
            # The test nodes inherit `pkgs` (which already allows claude-code
            # via the forAllSystems import), so no nixpkgs.config here.
            nodes.machine = { lib, ... }: {
              imports = [ self.nixosModules.liquidagent ];
              services.liquidagent.enable = true;
              # Run the offline fake harness — proves the service boots and
              # serves without needing Claude credentials in CI.
              systemd.services.liquidagent.environment.LIQUID_AGENT_CMD =
                lib.mkForce "${pkgs.bun}/bin/bun run ${(self.packages.${pkgs.stdenv.hostPlatform.system}.liquid-agent)}/share/liquid-agent/fake-harness.ts";
              virtualisation.memorySize = 2048;
            };
            testScript = ''
              machine.wait_for_unit("liquidagent.service")
              machine.wait_for_open_port(3000)
              machine.succeed("curl -sf http://localhost:3000/api/health | grep -q '\"status\":\"ok\"'")
              machine.succeed("curl -sf http://localhost:3000/api/health | grep -q pipeline_mode")
              # first-boot workspace + deployed worktree were created under the state dir
              machine.succeed("test -d /var/lib/liquidagent/workspace/.git")
              machine.succeed("test -d /var/lib/liquidagent/data/pipeline/deployed")
            '';
          };
        });

      # Eval-only smoke check for the module:
      #   nix eval .#nixosConfigurations.smoke.config.systemd.services.liquidagent.serviceConfig.ExecStart
      nixosConfigurations.smoke = nixpkgs.lib.nixosSystem {
        system = "x86_64-linux";
        modules = [
          self.nixosModules.liquidagent
          ({ lib, ... }: {
            nixpkgs.config.allowUnfreePredicate = pkg:
              builtins.elem (lib.getName pkg) [ "claude-code" ];
            services.liquidagent.enable = true;
            fileSystems."/" = { device = "/dev/null"; fsType = "ext4"; };
            boot.loader.grub.enable = false;
            system.stateVersion = "25.11";
          })
        ];
      };
    };
}
