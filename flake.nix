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

  # Layout:
  #   nix/package.nix   the Rust workspace, exposed as pkgs.cas via the overlay
  #   nix/smoke.nix     cas-vm-smoke, one runner per block backend
  #   nix/guest/        the NixOS guest those runners boot
  #   nix/modules/      NixOS modules for dedicated test hosts
  #   nix/checks/       evaluation-only checks for `nix flake check`
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
          provenance = {
            source_revision = self.rev or self.dirtyRev or null;
            source_path = toString self.outPath;
            nixpkgs_revision = nixpkgs.rev;
            lock = ./flake.lock;
          };
        };
    in
    {
      # Adds `cas` (and the rust-bin toolchain it is built with) to a nixpkgs.
      overlays.default = lib.composeManyExtensions [
        rust-overlay.overlays.default
        (final: _prev: { cas = final.callPackage ./nix/package.nix { }; })
      ];

      packages = eachSystem (pkgs: {
        default = pkgs.cas;
        inherit (pkgs) cas;
        vm-smoke = smokeFor pkgs "raw" false;
        daemon-smoke = smokeFor pkgs "daemon" false;
        staging-smoke = smokeFor pkgs "staging" false;
        dev-vm = smokeFor pkgs "staging" true;
        census-pilot = pkgs.callPackage ./nix/census.nix { };
        test-guest = (guestFor pkgs "raw" false).config.system.build.vm;
      });

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
        host-config = import ./nix/checks/host-config.nix {
          inherit nixpkgs pkgs;
          inherit (self) nixosModules;
        };
      });
    };
}
