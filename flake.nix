{
  description = "Super Simple Booru Bot - Rust Discord bot";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }: let
    systems = [ "x86_64-linux" ];
    forAllSystems = nixpkgs.lib.genAttrs systems;
  in {
    packages = forAllSystems (system: let
      pkgs = nixpkgs.legacyPackages.${system};
    in {
      default = pkgs.rustPlatform.buildRustPackage {
        pname = "super-simple-booru-bot";
        version = "26.1.1-alpha";
        src = pkgs.lib.fileset.toSource {
          root = ./.;
          fileset = pkgs.lib.fileset.unions [
            ./Cargo.toml
            ./Cargo.lock
            ./build.rs
            ./src
            ./locales
            ./LICENSE
          ];
        };
        cargoLock.lockFile = ./Cargo.lock;
        nativeBuildInputs = [ pkgs.pkg-config ];
        buildInputs = [ pkgs.openssl ];
        meta = {
          description = "Discord bot that returns random booru images from configurable tag presets";
          mainProgram = "super-simple-booru-bot";
          platforms = [ "x86_64-linux" ];
        };
      };
    });

    apps = forAllSystems (system: {
      default = {
        type = "app";
        program = "${self.packages.${system}.default}/bin/super-simple-booru-bot";
        meta.description = "Run Super Simple Booru Bot from a provisioned runtime state directory";
      };
    });

    devShells = forAllSystems (system: let pkgs = nixpkgs.legacyPackages.${system}; in {
      default = pkgs.mkShell {
        packages = with pkgs; [ cargo rustc rustfmt clippy cargo-audit pkg-config openssl ];
        RUST_BACKTRACE = "1";
      };
    });

    checks = forAllSystems (system: let
      pkgs = nixpkgs.legacyPackages.${system};
      testSystem = nixpkgs.lib.nixosSystem {
        modules = [ self.nixosModules.default {
          nixpkgs.hostPlatform = system;
          system.stateVersion = "26.05";
          services.superSimpleBooruBot = {
            enable = true;
            secretsFile = "/var/lib/super-simple-booru-bot/runtime-only.toml";
            dbPath = "audit.sqlite3";
          };
        } ];
      };
      service = testSystem.config.systemd.services.super-simple-booru-bot;
      source = self.packages.${system}.default.src;
    in {
      module-contract =
        assert service.environment.SUPER_SIMPLE_BOORU_BOT_SECRETS_FILE == "/var/lib/super-simple-booru-bot/runtime-only.toml";
        assert service.environment.SUPER_SIMPLE_BOORU_BOT_DB_PATH == "audit.sqlite3";
        assert service.serviceConfig.Restart == "on-failure";
        assert !(builtins.hasAttr "secretsText" testSystem.options.services.superSimpleBooruBot);
        assert !(pkgs.lib.hasInfix "printf" service.preStart);
        pkgs.runCommand "super-simple-booru-bot-module-contract" {} "touch $out";
      source-contract = pkgs.runCommand "super-simple-booru-bot-source-contract" { src = source; } ''
        test -f "$src/Cargo.toml"
        test -d "$src/src"
        test ! -e "$src/secrets.toml"
        test ! -e "$src/super-simple-booru-bot.sqlite3"
        test ! -e "$src/target"
        test ! -e "$src/.git"
        touch "$out"
      '';
    });

    nixosModules.default = { config, lib, pkgs, ... }: let
      cfg = config.services.superSimpleBooruBot;
    in {
      options.services.superSimpleBooruBot = {
        enable = lib.mkEnableOption "Super Simple Booru Bot Discord bot";
        package = lib.mkOption {
          type = lib.types.package;
          default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        };
        dataDir = lib.mkOption {
          type = lib.types.str;
          default = "/var/lib/super-simple-booru-bot";
        };
        secretsFile = lib.mkOption {
          type = lib.types.str;
          default = "${cfg.dataDir}/secrets.toml";
          description = "Absolute runtime path to a manually provisioned secrets file. Never put secret contents in Nix configuration.";
        };
        dbPath = lib.mkOption {
          type = lib.types.str;
          default = "super-simple-booru-bot.sqlite3";
        };
      };

      config = lib.mkIf cfg.enable {
        assertions = [
          { assertion = lib.hasPrefix "/" cfg.dataDir; message = "super-simple-booru-bot.dataDir must be absolute"; }
          { assertion = lib.hasPrefix "/" cfg.secretsFile && !(lib.hasPrefix builtins.storeDir cfg.secretsFile); message = "super-simple-booru-bot.secretsFile must be a runtime path outside the Nix store"; }
        ];
        users.users.super-simple-booru-bot = {
          isSystemUser = true;
          group = "super-simple-booru-bot";
          home = cfg.dataDir;
          createHome = true;
        };
        users.groups.super-simple-booru-bot = {};
        systemd.tmpfiles.rules = [ "d ${cfg.dataDir} 0700 super-simple-booru-bot super-simple-booru-bot -" ];

        systemd.services.super-simple-booru-bot = {
          description = "Super Simple Booru Bot Discord bot";
          after = [ "network-online.target" ];
          wants = [ "network-online.target" ];
          wantedBy = [ "multi-user.target" ];
          environment = {
            SUPER_SIMPLE_BOORU_BOT_SECRETS_FILE = cfg.secretsFile;
            SUPER_SIMPLE_BOORU_BOT_DB_PATH = cfg.dbPath;
            SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
          };
          serviceConfig = {
            Type = "simple";
            User = "super-simple-booru-bot";
            Group = "super-simple-booru-bot";
            WorkingDirectory = cfg.dataDir;
            ExecStart = "${cfg.package}/bin/super-simple-booru-bot";
            Restart = "on-failure";
            RestartSec = "5s";
            UMask = "0077";
            NoNewPrivileges = true;
            PrivateTmp = true;
            ProtectSystem = "strict";
            ProtectHome = true;
            ReadWritePaths = [ cfg.dataDir ];
            TimeoutStopSec = "60s";
          };
          preStart = ''
            if ! test -r ${lib.escapeShellArg cfg.secretsFile}; then
              echo ${lib.escapeShellArg ("super-simple-booru-bot: runtime secrets file is missing or unreadable: " + cfg.secretsFile)} >&2
              exit 1
            fi
          '';
        };
      };
    };
  };
}
