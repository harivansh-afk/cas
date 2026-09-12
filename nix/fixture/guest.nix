# Dedicated XFS scratch filesystem; the launcher supplies only fresh paths.
{
  lib,
  pkgs,
  modulesPath,
  cas,
  ...
}:
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
    path = [
      pkgs.coreutils
      pkgs.util-linux
      pkgs.xfsprogs
    ];
    serviceConfig = {
      Type = "oneshot";
      TimeoutStartSec = 100;
    };
    script = ''
      export CAS_CORE_TESTS=${cas.tests}/bin/cas-core-tests
      ${builtins.readFile ./workload.sh}
    '';
    postStop = ''
      printf '{"schema_version":1,"service_result":"%s","exit_code":"%s","exit_status":"%s"}\n' \
        "$SERVICE_RESULT" "$EXIT_CODE" "$EXIT_STATUS" > /results/completion.json
      ${pkgs.systemd}/bin/journalctl -u cas-fixture.service --no-pager > /results/service.log
      ${pkgs.systemd}/bin/systemctl --force --force poweroff
    '';
  };
}
