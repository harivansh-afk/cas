{
  description = "CAS research tools and reproducible Linux test environments";

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

  outputs =
    {
      self,
      nixpkgs,
      disko,
      rust-overlay,
      ...
    }:
    let
      systems = [
        "aarch64-linux"
        "x86_64-linux"
      ];
      forSystems = nixpkgs.lib.genAttrs systems;
      environments = forSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };
          toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          rustPlatform = pkgs.makeRustPlatform {
            cargo = toolchain;
            rustc = toolchain;
          };
          cas = pkgs.callPackage ./nix/package.nix { inherit rustPlatform; };
          guestFor =
            backend:
            nixpkgs.lib.nixosSystem {
              inherit system;
              modules = [
                ./nix/tests/guest.nix
                { _module.args.casBackend = backend; }
              ];
            };
          smokeFor =
            backend:
            let
              guest = guestFor backend;
              vm = guest.config.system.build.vm;
              buildInfo = pkgs.writeText "cas-vm-build.json" (
                builtins.toJSON {
                  inherit system backend;
                  source_revision = self.rev or self.dirtyRev or null;
                  source_path = toString self.outPath;
                  nixpkgs_revision = nixpkgs.rev;
                  vm = toString vm;
                  daemon = if backend == "daemon" then "${cas}/bin/cas-daemon" else null;
                  qemu_version = pkgs.qemu_kvm.version;
                  fio_version = pkgs.fio.version;
                  guest_kernel = guest.config.boot.kernelPackages.kernel.version;
                  guest_memory_mib = guest.config.virtualisation.memorySize;
                  guest_vcpus = guest.config.virtualisation.cores;
                }
              );
            in
            pkgs.writeShellApplication {
              name = "cas-vm-smoke";
              runtimeInputs = [
                pkgs.python3
                pkgs.util-linux
                pkgs.git
              ];
              text = ''
                exec python3 ${./experiments/run-vm.py} \
                  --vm ${vm}/bin/run-cas-guest-vm \
                  --build-info ${buildInfo} \
                  --lock ${./flake.lock} "$@"
              '';
            };
          vm = (guestFor "raw").config.system.build.vm;
          smoke = smokeFor "raw";
          daemonSmoke = smokeFor "daemon";
        in
        {
          inherit
            pkgs
            vm
            smoke
            cas
            daemonSmoke
            toolchain
            ;
        }
      );
    in
    {
      packages = forSystems (
        system:
        let
          env = environments.${system};
        in
        {
          default = env.cas;
          cas = env.cas;
          vm-smoke = env.smoke;
          daemon-smoke = env.daemonSmoke;
          test-guest = env.vm;
        }
      );

      apps = forSystems (system: {
        vm-smoke = {
          type = "app";
          program = "${environments.${system}.smoke}/bin/cas-vm-smoke";
          meta.description = "Run a KVM guest write/readback check on a new raw disk";
        };
      });

      devShells = forSystems (
        system:
        let
          pkgs = environments.${system}.pkgs;
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              environments.${system}.toolchain
              just
              uv
              qemu_kvm
              fio
              xfsprogs
              util-linux
              nixfmt
              nixos-rebuild
              nixos-anywhere
            ];
          };
        }
      );

      formatter = forSystems (system: environments.${system}.pkgs.nixfmt);

      nixosModules = {
        test-host = { pkgs, ... }: {
          imports = [ ./nix/modules/host.nix ];
          environment.systemPackages = [ self.packages.${pkgs.stdenv.hostPlatform.system}.cas ];
        };
        bare-metal = { config, ... }: {
          imports = [
            self.nixosModules.test-host
            disko.nixosModules.disko
            ./nix/modules/disks.nix
          ];
          boot.loader.systemd-boot.enable = true;
          boot.loader.efi.canTouchEfiVariables = false;
          assertions = [
            {
              assertion = config.cas.testbed.authorizedKeys != [ ];
              message = "Configure an administrator SSH public key before provisioning a test host.";
            }
          ];
        };
      };

      templates.test-host = {
        path = ./templates/test-host;
        description = "UEFI bare-metal CAS research host (fill disk IDs and SSH keys)";
      };

      checks = forSystems (
        system:
        let
          env = environments.${system};
        in
        {
          cas = env.cas;
          runner = env.pkgs.runCommand "cas-runner-tests" { nativeBuildInputs = [ env.pkgs.python3 ]; } ''
            cp -r ${./experiments} experiments
            chmod -R u+w experiments
            python3 -m unittest discover -s experiments/tests -v
            touch "$out"
          '';
          host-config = import ./nix/tests/host-config.nix {
            inherit nixpkgs system;
            inherit (self) nixosModules;
            pkgs = env.pkgs;
          };
        }
      );
    };
}
