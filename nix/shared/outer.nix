{
  lib,
  pkgs,
  cas,
  inner,
  liveRecovery ? false,
  pressure ? false,
  ...
}:
let
  build = pkgs.writeText "cas-shared-build.json" (
    builtins.toJSON {
      host = "${cas}/bin/cas-host";
      guest = "${inner.config.system.build.vm}/bin/run-cas-filesystem-vm";
      qemu = lib.getExe' inner.config.virtualisation.qemu.package "qemu-system-${
        if pkgs.stdenv.hostPlatform.isAarch64 then "aarch64" else "x86_64"
      }";
      kernel = inner.config.boot.kernelPackages.kernel.version;
      live_recovery = liveRecovery;
      inherit pressure;
      guest_ram_bytes = inner.config.virtualisation.memorySize * 1024 * 1024;
    }
  );
in
{
  imports = [ ../fixture/guest.nix ];
  virtualisation.memorySize = lib.mkForce 4096;
  systemd.services.cas-fixture = {
    serviceConfig.TimeoutStartSec = lib.mkForce (if pressure then 1200 else 450);
    script = lib.mkForce ''
      exec > /results/workload.log 2>&1
      findmnt --json /fixture > /results/mount.json
      xfs_info /fixture > /results/xfs-info.log
      uname -a > /results/kernel.log
      df -B1 /fixture > /results/space-before.log
      ${cas}/bin/cas-harness shared --root /fixture/store --output /results/shared --build-info ${build} --scenario /results/scenario.json
      df -B1 /fixture > /results/space-after.log
      sync -f /fixture
    '';
  };
}
