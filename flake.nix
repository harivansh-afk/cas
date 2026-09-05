{
  description = "CAS research tools and reproducible Linux test environments";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
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
          pkgs = import nixpkgs { inherit system; };
          guest = nixpkgs.lib.nixosSystem {
            inherit system;
            modules = [ ./nix/guest.nix ];
          };
          vm = guest.config.system.build.vm;
          buildInfo = pkgs.writeText "cas-vm-build.json" (
            builtins.toJSON {
              inherit system;
              source_revision = self.rev or self.dirtyRev or null;
              source_path = toString self.outPath;
              nixpkgs_revision = nixpkgs.rev;
              vm = toString vm;
              qemu_version = pkgs.qemu_kvm.version;
              fio_version = pkgs.fio.version;
              guest_kernel = guest.config.boot.kernelPackages.kernel.version;
              guest_memory_mib = guest.config.virtualisation.memorySize;
              guest_vcpus = guest.config.virtualisation.cores;
            }
          );
          smoke = pkgs.writeShellApplication {
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
        in
        {
          inherit pkgs vm smoke;
          cas = pkgs.callPackage ./nix/package.nix { };
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
              cargo
              rustc
              rustfmt
              clippy
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
          imports = [ ./nix/host.nix ];
          environment.systemPackages = [ self.packages.${pkgs.stdenv.hostPlatform.system}.cas ];
        };
        bare-metal = {
          imports = [
            self.nixosModules.test-host
            disko.nixosModules.disko
            ./nix/disks.nix
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
          host-config = import ./nix/check-host.nix {
            inherit nixpkgs system;
            inherit (self) nixosModules;
            pkgs = env.pkgs;
          };
        }
      );
    };
}
