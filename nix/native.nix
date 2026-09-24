# Run storage on the physical host and boot only the workload guests under KVM.
{
  pkgs,
  lib,
  nixpkgs,
  provenance,
  lock,
}:
let
  guests = lib.genAttrs [ "raw" "daemon" "cas" ] (
    backend:
    (lib.nixosSystem {
      specialArgs = {
        inherit backend;
        probeTools = false;
        guestCores = 2;
        guestMemoryMiB = 2048;
        guestAcceleration = "kvm";
        guestQueues = 1;
        guestQueueSize = 128;
      };
      modules = [
        ./lab/guest.nix
        { nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system; }
      ];
    }).config.system.build.vm
  );
  build = pkgs.writeText "cas-native-build.json" (
    builtins.toJSON {
      inherit (provenance) source_revision source_path;
      inherit lock;
      system = pkgs.stdenv.hostPlatform.system;
      host = "${pkgs.cas}/bin/cas-host";
      daemon = "${pkgs.cas}/bin/cas-daemon";
      guests = lib.mapAttrs (_: vm: "${vm}/bin/run-cas-lab-guest-vm") guests;
      guest_vcpus = 2;
      guest_memory_mib = 2048;
      guest_queues = 1;
      guest_queue_size = 128;
      acceleration = "kvm";
      qemu_version = pkgs.qemu_kvm.version;
      fio_version = pkgs.fio.version;
    }
  );
in
pkgs.writeShellApplication {
  name = "cas-native-run";
  runtimeInputs = with pkgs; [
    openssh
    coreutils
    util-linux
    git
    iproute2
    jq
  ];
  text = ''exec ${pkgs.cas}/bin/cas-harness native --build-info ${build} "$@"'';
  meta.description = "Owned native KVM experiment with raw, passthrough and shared CAS arms";
}
