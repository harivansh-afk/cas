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

  workload = pkgs.writeShellApplication {
    name = "cas-shared-workload";
    runtimeInputs = with pkgs; [
      coreutils
      util-linux
      xfsprogs
    ];
    runtimeEnv = {
      CAS_HARNESS = "${cas}/bin/cas-harness";
      CAS_BUILD_INFO = "${build}";
    };
    text = builtins.readFile ./workload.sh;
  };
in
{
  imports = [ ../fixture/guest.nix ];
  virtualisation.memorySize = lib.mkForce 4096;
  systemd.services.cas-fixture = {
    serviceConfig = {
      TimeoutStartSec = lib.mkForce (if pressure then 1200 else 450);
      ExecStart = lib.mkForce (lib.getExe workload);
    };
  };
}
