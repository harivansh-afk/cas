{
  description = "CAS storage and reproducible Linux test environments";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    disko = {
      url = "github:nix-community/disko";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  # Layout:
  #   nix/package.nix      the Rust workspace, exposed as pkgs.cas via the overlay
  #   nix/smoke.nix        cas-vm-smoke, one runner per block backend (dev-vm adds SSH)
  #   nix/guest/           the NixOS guest those runners boot
  #   nix/fixture/         the XFS KVM fixture; also wraps the shared and pressure guests
  #   nix/shared/          the outer host and inner filesystem guests those fixtures nest
  #   nix/lab/             casctl, named local VM labs with their host and guest systems
  #   nix/checkpoints.nix  the source-bound checkpoint suite wrapper
  #   nix/census.nix       the census pilot and the dated ARM64 clone fleet
  #   nix/modules/         NixOS modules for dedicated test hosts
  #   nix/checks/          evaluation-only checks for `nix flake check`
  outputs =
    {
      self,
      nixpkgs,
      disko,
      rust-overlay,
      ...
    }:
    let
      inherit (nixpkgs) lib;

      systems = [
        "aarch64-linux"
        "x86_64-linux"
      ];

      # Call `f` once per supported system with a package set carrying this
      # flake's overlay, so every output can refer to `pkgs.cas`.
      eachSystem =
        f:
        lib.genAttrs systems (
          system:
          f (
            import nixpkgs {
              inherit system;
              overlays = [ self.overlays.default ];
            }
          )
        );

      # Flake-level facts the package set cannot see. `fallback` is the
      # source_revision recorded when neither self.rev nor self.dirtyRev exists.
      provenanceWith = fallback: {
        source_revision = self.rev or self.dirtyRev or fallback;
        source_path = toString self.outPath;
      };
      provenance = provenanceWith null;
      # casctl's build record spells that case out instead of leaving it null.
      labProvenance = provenanceWith "unversioned";

      # The guest as a NixOS system, configured for one block backend.
      guestFor =
        pkgs: backend: interactive:
        lib.nixosSystem {
          modules = [
            ./nix/guest
            {
              nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system;
              cas.guest = { inherit backend interactive; };
            }
          ];
        };

      # The cas-vm-smoke runner for one backend.
      smokeFor =
        pkgs: backend: interactive:
        pkgs.callPackage ./nix/smoke.nix {
          inherit backend;
          guest = guestFor pkgs backend interactive;
          provenance = provenance // {
            nixpkgs_revision = nixpkgs.rev;
            lock = ./flake.lock;
          };
        };
      sharedFor =
        pkgs:
        {
          liveRecovery ? false,
          pressure ? false,
        }:
        pkgs.callPackage ./nix/fixture {
          name =
            if pressure then
              "cas-pressure-fixture"
            else if liveRecovery then
              "cas-shared-recovery-fixture"
            else
              "cas-shared-fixture";
          workload = if pressure then "pressure" else "shared";
          inherit liveRecovery;
          guest = lib.nixosSystem {
            specialArgs = {
              cas = pkgs.cas;
              inherit liveRecovery pressure;
              inner = lib.nixosSystem {
                specialArgs = {
                  cas = pkgs.cas;
                  inherit pressure;
                };
                modules = [
                  ./nix/shared/guest.nix
                  { nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system; }
                ];
              };
            };
            modules = [
              ./nix/shared/outer.nix
              { nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system; }
            ];
          };
          inherit provenance;
        };
    in
    {
      # Adds `cas` (and the rust-bin toolchain it is built with) to a nixpkgs.
      overlays.default = lib.composeManyExtensions [
        rust-overlay.overlays.default
        (final: _prev: { cas = final.callPackage ./nix/package.nix { }; })
      ];

      packages = eachSystem (
        pkgs:
        let
          raw = smokeFor pkgs "raw" false;
          daemon = smokeFor pkgs "daemon" false;
          staging = smokeFor pkgs "staging" false;
          local = smokeFor pkgs "local" false;
          async = smokeFor pkgs "local-async" false;
          xfs = pkgs.callPackage ./nix/fixture {
            guest = lib.nixosSystem {
              specialArgs = {
                cas = pkgs.cas;
              };
              modules = [
                ./nix/fixture/guest.nix
                { nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system; }
              ];
            };
            inherit provenance;
          };
          sharedRecovery = sharedFor pkgs { liveRecovery = true; };
          pressure = sharedFor pkgs { pressure = true; };
        in
        {
          default = pkgs.cas;
          inherit (pkgs) cas;
          native = import ./nix/native.nix {
            inherit pkgs lib nixpkgs;
            provenance = labProvenance;
            lock = ./flake.lock;
          };
          casctl = import ./nix/lab {
            inherit pkgs lib nixpkgs;
            provenance = labProvenance;
          };
          casctl-read-probe = import ./nix/lab {
            inherit pkgs lib nixpkgs;
            guestCores = 2;
            traceReads = true;
            provenance = labProvenance;
          };
          casctl-read-control = import ./nix/lab {
            inherit pkgs lib nixpkgs;
            guestCores = 2;
            probeTools = true;
            provenance = labProvenance;
          };
          vm-smoke = raw;
          daemon-smoke = daemon;
          staging-smoke = staging;
          local-smoke = local;
          async-smoke = async;
          checkpoints = pkgs.callPackage ./nix/checkpoints.nix {
            fixtures = {
              inherit xfs pressure;
              shared = sharedRecovery;
            };
            wrappers = {
              inherit
                raw
                daemon
                staging
                local
                async
                ;
            };
            inherit provenance;
          };
          xfs-fixture = xfs;
          shared-fixture = sharedFor pkgs { };
          pressure-fixture = pressure;
          shared-recovery-fixture = sharedRecovery;
          dev-vm = smokeFor pkgs "staging" true;
          census-pilot = (pkgs.callPackage ./nix/census.nix { }).pilot;
          census-fleet = (pkgs.callPackage ./nix/census.nix { }).fleet;
          test-guest = (guestFor pkgs "raw" false).config.system.build.vm;
        }
      );

      apps = eachSystem (pkgs: {
        vm-smoke = {
          type = "app";
          program = lib.getExe (smokeFor pkgs "raw" false);
          meta.description = "Run a KVM guest write/readback check on a new raw disk";
        };
      });

      devShells = eachSystem (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            cas.toolchain
            just
            qemu_kvm
            fio
            openssh
            iproute2
            jq
            xfsprogs
            e2fsprogs
            util-linux
            nixfmt
            nixos-rebuild
            nixos-anywhere
          ];
        };
      });

      # treefmt wrapper around nixfmt; `nix fmt` formats the tree, `nix fmt -- --ci` checks it.
      formatter = eachSystem (pkgs: pkgs.nixfmt-tree);

      nixosModules = {
        # Tools, SSH, and measurement defaults for any dedicated test host.
        test-host = {
          imports = [ ./nix/modules/test-host.nix ];
          nixpkgs.overlays = [ self.overlays.default ];
        };
        # test-host plus a disko disk layout and UEFI boot for nixos-anywhere.
        bare-metal = {
          imports = [
            self.nixosModules.test-host
            disko.nixosModules.disko
            ./nix/modules/bare-metal.nix
          ];
        };
      };

      templates.test-host = {
        path = ./templates/test-host;
        description = "UEFI bare-metal CAS research host (fill disk IDs and SSH keys)";
      };

      checks = eachSystem (pkgs: {
        inherit (pkgs) cas;
        census-pilot = self.packages.${pkgs.stdenv.hostPlatform.system}.census-pilot;
        census-fleet = self.packages.${pkgs.stdenv.hostPlatform.system}.census-fleet;
        host-config = import ./nix/checks/host-config.nix {
          inherit nixpkgs pkgs;
          inherit (self) nixosModules;
        };
      });
    };
}
