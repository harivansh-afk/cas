{
  lib,
  writeShellApplication,
  writeText,
  cas,
  git,
  nix,
  util-linux,
  guest,
  provenance,
}:
let
  vm = guest.config.system.build.vm;
  buildInfo = writeText "cas-fixture-build.json" (
    builtins.toJSON {
      inherit (provenance) source_revision source_path;
      vm = "${vm}/bin/run-cas-fixture-vm";
      harness = lib.getExe' cas "cas-harness";
      tests = "${cas.tests}/bin/cas-core-tests";
      daemon_tests = "${cas.tests}/bin/cas-daemon-tests";
      qemu = lib.getExe' guest.config.virtualisation.qemu.package "qemu-system-${
        if guest.config.nixpkgs.hostPlatform.isAarch64 then "aarch64" else "x86_64"
      }";
      qemu_executable = "${guest.config.virtualisation.qemu.package}/bin/.qemu-system-${
        if guest.config.nixpkgs.hostPlatform.isAarch64 then "aarch64" else "x86_64"
      }-wrapped";
      guest_kernel = guest.config.boot.kernelPackages.kernel.version;
    }
  );
in
writeShellApplication {
  name = "cas-xfs-fixture";
  runtimeInputs = [
    git
    nix
    util-linux
  ];
  text = ''
    exec ${lib.getExe' cas "cas-harness"} fixture --build-info ${buildInfo} "$@"
  '';
  meta.description = "Run source-bound native core IO and reflink controls on fresh XFS";
}
