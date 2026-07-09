# NixOS module for liquid. Imported via the flake:
#   imports = [ liquidagent.nixosModules.liquidagent ];
#   services.liquidagent.enable = true;
#
# The supervisor binds localhost only — put your reverse proxy (with SSO) in
# front. Claude credentials live under ${dataDir}/.claude (the service's
# HOME); either log in once as the service user or provide ANTHROPIC_API_KEY
# via services.liquidagent.environmentFile.
self: { config, lib, pkgs, ... }:

let
  cfg = config.services.liquidagent;
  packages = self.packages.${pkgs.stdenv.hostPlatform.system};
  au = cfg.autoUpdate;
  # When self-update is on the service runs from mutable symlinks the updater
  # repoints (seeded from the pinned packages on first boot); otherwise straight
  # from the pinned flake packages.
  selfDir = "${cfg.dataDir}/self-update";
  liquidExe = if au.enable then "${selfDir}/liquid/bin/liquid" else lib.getExe packages.liquid;
  agentBase = if au.enable then "${selfDir}/agent" else "${packages.liquid-agent}";
in
{
  options.services.liquidagent = {
    enable = lib.mkEnableOption "liquid AI agent";

    port = lib.mkOption {
      type = lib.types.port;
      default = 3000;
      description = "Port the supervisor listens on.";
    };

    host = lib.mkOption {
      type = lib.types.str;
      default = "127.0.0.1";
      example = "0.0.0.0";
      description = ''
        Address the supervisor binds. Localhost by default. Only widen this
        (e.g. 0.0.0.0) when a trusted reverse proxy fronts it — the supervisor
        has no transport security or SSO of its own.
      '';
    };

    initialPassword = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = ''
        If set and no password exists yet, seed this as the initial login
        password on first boot. Never overrides a password already set (so a
        later change from the UI persists). NOTE: a literal string here lands
        world-readable in the nix store — prefer LIQUID_INITIAL_PASSWORD in
        `environmentFile` for anything sensitive, and change it after first login.
      '';
    };

    dataDir = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/liquidagent";
      description = "State directory: agent workspace, Claude credentials.";
    };

    user = lib.mkOption {
      type = lib.types.str;
      default = "liquidagent";
    };

    group = lib.mkOption {
      type = lib.types.str;
      default = "liquidagent";
    };

    claudePackage = lib.mkOption {
      type = lib.types.package;
      default = pkgs.claude-code;
      defaultText = lib.literalExpression "pkgs.claude-code";
      description = ''
        Claude Code package providing the `claude` binary the agent harness
        drives. Unfree — the host must allow it, e.g.:
        nixpkgs.config.allowUnfreePredicate = pkg:
          builtins.elem (lib.getName pkg) [ "claude-code" ];
      '';
    };

    pipelineMode = lib.mkOption {
      type = lib.types.enum [ "vibe" "reviewed" ];
      default = "vibe";
      description = ''
        Default deploy pipeline mode. vibe = commit ships immediately;
        reviewed = a reviewer subagent gates app changes before they go live.
        Changeable at runtime from the shell; this sets the initial value.
      '';
    };

    environmentFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        Optional EnvironmentFile (e.g. containing ANTHROPIC_API_KEY=...).
        Keep it out of the nix store — use agenix/sops-nix or a root-owned file.
      '';
    };

    autoUpdate = {
      enable = lib.mkEnableOption ''
        periodic self-update: a timer builds the latest liquid from `flake` and
        restarts the service when it changed. Needs a working nix inside the host
        (a writable store — e.g. microvm.writableStoreOverlay — + the daemon)'';
      flake = lib.mkOption {
        type = lib.types.str;
        default = "github:whimbree/liquidagent";
        description = "Flake ref to track; its #liquid and #liquid-agent outputs are built.";
      };
      interval = lib.mkOption {
        type = lib.types.str;
        default = "5min";
        example = "1h";
        description = "systemd time between update checks (OnUnitActiveSec).";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    users.users.${cfg.user} = {
      isSystemUser = true;
      group = cfg.group;
      home = cfg.dataDir;
      # The agent runs shell commands (Claude Code's Bash tool) as this account;
      # a nologin shell breaks that. Give it a real shell.
      shell = pkgs.bashInteractive;
    };
    users.groups.${cfg.group} = { };

    systemd.services.liquidagent = {
      description = "liquid AI agent supervisor";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];

      # The agent's Bash tool and the harness need a sane PATH.
      path = [
        pkgs.bash
        pkgs.coreutils
        pkgs.git
        pkgs.bun
        cfg.claudePackage
      ];

      environment = {
        HOME = cfg.dataDir;
        # Pin Claude Code's config/credential dir so it never depends on HOME
        # being set right — an interactive `sudo -u liquidagent` without -H would
        # otherwise send it to the invoking user's home. `claude login` should be
        # run with this same dir (or with -H) so creds land where the service reads.
        CLAUDE_CONFIG_DIR = "${cfg.dataDir}/.claude";
        LIQUID_PORT = toString cfg.port;
        LIQUID_HOST = cfg.host;
        LIQUID_WORKSPACE_DIR = "${cfg.dataDir}/workspace";
        LIQUID_DATA_DIR = "${cfg.dataDir}/data";
        LIQUID_AGENT_CMD =
          "${pkgs.bun}/bin/bun run ${agentBase}/share/liquid-agent/harness.ts";
        LIQUID_REVIEW_CMD =
          "${pkgs.bun}/bin/bun run ${agentBase}/share/liquid-agent/review.ts";
        LIQUID_CLAUDE_BIN = "${cfg.claudePackage}/bin/claude";
        LIQUID_PIPELINE_MODE = cfg.pipelineMode;
      };

      serviceConfig = {
        ExecStart = liquidExe;
        # Self-update mode: seed the mutable symlinks from the pinned packages on
        # first boot so the service always has a working version, even before the
        # updater has run (or if it's offline). The updater repoints them later.
        ExecStartPre = lib.mkIf au.enable [
          "${pkgs.bash}/bin/bash -c '${pkgs.coreutils}/bin/mkdir -p ${selfDir}; [ -e ${selfDir}/liquid ] || ${pkgs.coreutils}/bin/ln -sfn ${packages.liquid} ${selfDir}/liquid; [ -e ${selfDir}/agent ] || ${pkgs.coreutils}/bin/ln -sfn ${packages.liquid-agent} ${selfDir}/agent'"
        ];
        User = cfg.user;
        Group = cfg.group;
        WorkingDirectory = cfg.dataDir;
        StateDirectory = "liquidagent";
        Restart = "on-failure";
        RestartSec = "5s";
        EnvironmentFile = lib.mkIf (cfg.environmentFile != null) cfg.environmentFile;

        # Hardening (design doc §12). The agent runs arbitrary code by
        # design; these scope the blast radius to dataDir.
        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        ReadWritePaths = [ cfg.dataDir ];
        CapabilityBoundingSet = "";
        RestrictSUIDSGID = true;
        LockPersonality = true;
        SystemCallFilter = [ "@system-service" ];
      };
    };

    # Self-updater: build the latest liquid from the flake and restart the service
    # only when the store paths actually change. Runs as root so it can use the
    # nix daemon and restart the unit; --refresh bypasses the flake-registry cache
    # so a fresh commit on the branch is picked up on the next tick.
    systemd.services.liquidagent-update = lib.mkIf au.enable {
      description = "Build the latest liquid from ${au.flake} and restart if changed";
      path = [ pkgs.nix pkgs.coreutils pkgs.systemd ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      serviceConfig.Type = "oneshot";
      script = ''
        set -uo pipefail
        dir=${selfDir}
        mkdir -p "$dir"
        feats="nix-command flakes"  # quoted as one value; word-splitting it breaks nix
        old="$(readlink -f "$dir/liquid" 2>/dev/null || true)|$(readlink -f "$dir/agent" 2>/dev/null || true)"
        if nix build --extra-experimental-features "$feats" --refresh '${au.flake}#liquid' --out-link "$dir/.gc-liquid" \
           && nix build --extra-experimental-features "$feats" '${au.flake}#liquid-agent' --out-link "$dir/.gc-agent"; then
          ln -sfn "$(readlink -f "$dir/.gc-liquid")" "$dir/liquid"
          ln -sfn "$(readlink -f "$dir/.gc-agent")" "$dir/agent"
          chown -h ${cfg.user}:${cfg.group} "$dir/liquid" "$dir/agent" 2>/dev/null || true
          new="$(readlink -f "$dir/liquid")|$(readlink -f "$dir/agent")"
          if [ "$old" != "$new" ]; then
            echo "liquid changed → restarting liquidagent"
            systemctl restart liquidagent.service
          else
            echo "liquid already current"
          fi
        else
          echo "liquid build failed; keeping the running version" >&2
        fi
      '';
    };
    systemd.timers.liquidagent-update = lib.mkIf au.enable {
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnBootSec = "3min";
        OnUnitActiveSec = au.interval;
        RandomizedDelaySec = "20s";
      };
    };
  };
}
