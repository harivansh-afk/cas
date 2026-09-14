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
  openssh,
  qemu_kvm,
  fio,
  cas,
  # "raw", "daemon", "staging", "local" or "local-async"; see nix/guest/default.nix.
  backend,
  # The evaluated NixOS guest for this backend (a `lib.nixosSystem` result).
  guest,
  # Flake-level facts the package set cannot see: source and nixpkgs revisions
  # and the lock file to copy into every result directory.
  provenance,
}:
let
  vm = guest.config.system.build.vm;
  inherit (guest.config.cas.guest) interactive;

  # Read by `cas-harness vm` (crates/harnesses/src/evidence.rs) and copied into
  # each result directory as build.json. Keys are part of that contract.
  buildInfo = writeText "cas-vm-build.json" (
    builtins.toJSON {
      inherit backend interactive;
      inherit (guest.config.nixpkgs.hostPlatform) system;
      inherit (provenance) source_revision source_path nixpkgs_revision;
      vm = toString vm;
      harness = lib.getExe' cas "cas-harness";
      qemu = lib.getExe' qemu_kvm "qemu-system-${
        if guest.config.nixpkgs.hostPlatform.isAarch64 then "aarch64" else "x86_64"
      }";
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
  name = if interactive then "cas-dev-vm" else "cas-vm-smoke";
  runtimeInputs = [
    util-linux
    git
  ]
  ++ lib.optional interactive openssh;
  runtimeEnv = {
    CAS_HARNESS = lib.getExe' cas "cas-harness";
    CAS_VM = "${vm}/bin/run-cas-guest-vm";
    CAS_BUILD_INFO = "${buildInfo}";
    CAS_LOCK = "${provenance.lock}";
  };
  text = builtins.readFile ./run-vm.sh;
  derivationArgs = {
    CAS_BUILD_INFO = "${buildInfo}";
    postCheck = builtins.readFile ./install-build-info.sh;
  };
  meta.description =
    if interactive then
      "Boot a CAS development guest with SSH"
    else
      "Run the KVM guest fio check against the ${backend} block backend";
}
