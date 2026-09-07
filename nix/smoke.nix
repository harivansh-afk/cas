# `cas-vm-smoke`: boot the NixOS test guest under KVM and run its fio jobs
# against one block backend. One runner is built per backend.
#
# The runner is a thin wrapper around `cas-harness vm`. It carries the guest
# VM script and a JSON build record so a result directory can always be traced
# back to the exact guest, daemon, QEMU, and source revision that produced it.
{
  lib,
  writeShellApplication,
  writeText,
  util-linux,
  git,
  qemu_kvm,
  fio,
  cas,
  # "raw", "daemon", or "staging"; see nix/guest/default.nix.
  backend,
  # The evaluated NixOS guest for this backend (a `lib.nixosSystem` result).
  guest,
  # Flake-level facts the package set cannot see: source and nixpkgs revisions
  # and the lock file to copy into every result directory.
  provenance,
}:
let
  vm = guest.config.system.build.vm;

  # Read by `cas-harness vm` (crates/harnesses/src/evidence.rs) and copied into
  # each result directory as build.json. Keys are part of that contract.
  buildInfo = writeText "cas-vm-build.json" (
    builtins.toJSON {
      inherit backend;
      inherit (guest.config.nixpkgs.hostPlatform) system;
      inherit (provenance) source_revision source_path nixpkgs_revision;
      vm = toString vm;
      daemon = if backend == "raw" then null else lib.getExe' cas "cas-daemon";
      qemu_version = qemu_kvm.version;
      fio_version = fio.version;
      guest_kernel = guest.config.boot.kernelPackages.kernel.version;
      guest_memory_mib = guest.config.virtualisation.memorySize;
      guest_vcpus = guest.config.virtualisation.cores;
    }
  );
in
writeShellApplication {
  name = "cas-vm-smoke";
  runtimeInputs = [
    util-linux
    git
  ];
  text = ''
    exec ${lib.getExe' cas "cas-harness"} vm \
      --vm ${vm}/bin/run-cas-guest-vm \
      --build-info ${buildInfo} \
      --lock ${provenance.lock} \
      "$@"
  '';
  meta.description = "Run the KVM guest fio check against the ${backend} block backend";
}
