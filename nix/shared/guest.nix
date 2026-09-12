{
  lib,
  pkgs,
  modulesPath,
  cas,
  ...
}:
let
  workload = pkgs.writeShellApplication {
    name = "cas-filesystem-workload";
    runtimeInputs = with pkgs; [
      coreutils
      util-linux
      e2fsprogs
    ];
    runtimeEnv = {
      CAS_HARNESS = "${cas}/bin/cas-harness";
      CAS_SQLITE = "${pkgs.sqlite}/bin/sqlite3";
    };
    text = builtins.readFile ./filesystem.sh;
  };

  finish = pkgs.writeShellApplication {
    name = "cas-filesystem-finish";
    runtimeInputs = with pkgs; [
      coreutils
      systemd
    ];
    text = builtins.readFile ./finish.sh;
  };
in
{
  imports = [ (modulesPath + "/virtualisation/qemu-vm.nix") ];
  networking.hostName = "cas-filesystem";
  networking.useDHCP = false;
  system.stateVersion = "26.05";
  documentation.enable = false;
  services.timesyncd.enable = false;
  virtualisation = {
    diskImage = null;
    memorySize = 512;
    cores = 1;
    graphics = false;
    writableStore = false;
    useHostCerts = false;
    qemu = {
      forceAccel = false;
      networkingOptions = lib.mkForce [ "-nic none" ];
      enableSharedMemory = true;
      options = [
        "-machine accel=tcg"
        "-no-reboot"
        ''-chardev "socket,id=cas,path=$CAS_VHOST_SOCKET,reconnect-ms=100"''
        "-device vhost-user-blk-pci,chardev=cas,num-queues=4,queue-size=256"
      ];
    };
    sharedDirectories.results = {
      source = ''"$CAS_RESULTS_DIR"'';
      target = "/results";
      securityModel = "none";
    };
  };
  systemd.services.cas-filesystem = {
    wantedBy = [ "multi-user.target" ];
    after = [
      "local-fs.target"
      "systemd-udev-settle.service"
    ];
    wants = [ "systemd-udev-settle.service" ];
    unitConfig.RequiresMountsFor = [ "/results" ];
    serviceConfig = {
      Type = "oneshot";
      TimeoutStartSec = 150;
      ExecStart = lib.getExe workload;
      ExecStopPost = lib.getExe finish;
    };
  };
}
