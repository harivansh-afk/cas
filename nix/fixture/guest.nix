# Dedicated XFS scratch filesystem; the launcher supplies only fresh paths.
{
  lib,
  pkgs,
  modulesPath,
  cas,
  ...
}:
let
  workload = pkgs.writeShellApplication {
    name = "cas-fixture-workload";
    runtimeInputs = with pkgs; [
      coreutils
      util-linux
      xfsprogs
    ];
    runtimeEnv = {
      CAS_CORE_TESTS = "${cas.tests}/bin/cas-core-tests";
      CAS_DAEMON_TESTS = "${cas.tests}/bin/cas-daemon-tests";
    };
    text = builtins.readFile ./workload.sh;
  };

  finish = pkgs.writeShellApplication {
    name = "cas-fixture-finish";
    runtimeInputs = with pkgs; [
      coreutils
      systemd
    ];
    text = builtins.readFile ./finish.sh;
  };
in
{
  imports = [ (modulesPath + "/virtualisation/qemu-vm.nix") ];
  networking.hostName = "cas-fixture";
  networking.useDHCP = false;
  system.stateVersion = "26.05";
  documentation.enable = false;
  services.timesyncd.enable = false;
  virtualisation = {
    diskImage = null;
    memorySize = 2048;
    cores = 4;
    graphics = false;
    writableStore = false;
    useHostCerts = false;
    qemu = {
      package = pkgs.qemu_kvm;
      forceAccel = true;
      networkingOptions = lib.mkForce [ "-nic none" ];
      options = [
        "-no-reboot"
        ''-drive "if=none,id=fixture,file=$CAS_XFS_IMAGE,format=raw,cache=none,aio=io_uring"''
        "-device virtio-blk-pci,drive=fixture,serial=cas-fixture"
      ];
    };
    sharedDirectories.results = {
      source = ''"$CAS_RESULTS_DIR"'';
      target = "/results";
      securityModel = "none";
    };
    fileSystems."/fixture" = {
      device = "/dev/disk/by-id/virtio-cas-fixture";
      fsType = "xfs";
      autoFormat = true;
      options = [ "noatime" ];
    };
  };
  systemd.services.cas-fixture = {
    wantedBy = [ "multi-user.target" ];
    after = [ "local-fs.target" ];
    unitConfig.RequiresMountsFor = [
      "/results"
      "/fixture"
    ];
    serviceConfig = {
      Type = "oneshot";
      TimeoutStartSec = 240;
      ExecStart = lib.getExe workload;
      ExecStopPost = lib.getExe finish;
    };
  };
}
