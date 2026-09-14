{
  pkgs,
  lib,
  nixpkgs,
  provenance,
  guestCores ? 1,
  traceReads ? false,
}:
let
  guests = lib.genAttrs [ "cas" "raw" "daemon" ] (
    backend:
    lib.nixosSystem {
      specialArgs = { inherit backend guestCores; };
      modules = [
        ./guest.nix
        { nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system; }
      ];
    }
  );
  host = lib.nixosSystem {
    specialArgs = {
      cas = pkgs.cas;
      inherit guests traceReads;
    };
    modules = [
      ./host.nix
      { nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system; }
    ];
  };
  build = pkgs.writeText "cas-lab-build.json" (
    builtins.toJSON {
      vm = "${host.config.system.build.vm}/bin/run-cas-lab-host-vm";
      inherit (provenance) source_revision source_path;
      kernel = host.config.boot.kernelPackages.kernel.version;
      system = pkgs.stdenv.hostPlatform.system;
    }
  );
in
pkgs.writeShellApplication {
  name = "casctl";
  runtimeInputs = with pkgs; [
    openssh
    systemd
    coreutils
    nix
  ];
  runtimeEnv.CAS_LAB_BUILD = "${build}";
  text = ''exec ${pkgs.cas}/bin/casctl "$@"'';
  meta.description = "Named CAS development VMs, SSH and measurements";
}
