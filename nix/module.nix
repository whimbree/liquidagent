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
          "${pkgs.bun}/bin/bun run ${packages.liquid-agent}/share/liquid-agent/harness.ts";
        LIQUID_REVIEW_CMD =
          "${pkgs.bun}/bin/bun run ${packages.liquid-agent}/share/liquid-agent/review.ts";
        LIQUID_CLAUDE_BIN = "${cfg.claudePackage}/bin/claude";
        LIQUID_PIPELINE_MODE = cfg.pipelineMode;
      };

      serviceConfig = {
        ExecStart = lib.getExe packages.liquid;
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
  };
}
