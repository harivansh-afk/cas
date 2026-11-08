{
  config,
  lib,
  pkgs,
  modulesPath,
  cas,
  pressure ? false,
  ...
}:
let
  # Build this before starting the daemon's recovery clock. A second 9p hop
  # for every guest executable/metadata lookup can consume that clock in boot.
  registration = pkgs.closureInfo { rootPaths = config.virtualisation.additionalPaths; };
  programs = pkgs.closureInfo {
    rootPaths = [
      config.system.build.toplevel
      registration
    ];
  };
  programImage =
    pkgs.runCommand "cas-guest-programs.erofs"
      {
        nativeBuildInputs = [
          pkgs.gnutar
          pkgs.erofs-utils
        ];
      }
      ''
        tar --create --absolute-names --verbatim-files-from \
          --transform 'flags=rSh;s|/nix/store/||' \
          --files-from ${programs}/store-paths | \
          mkfs.erofs --quiet --force-uid=0 --force-gid=0 \
            -L cas-programs -U 9cd4f673-0990-4ec0-a0b3-69ea967cdffb -T 0 \
            --hard-dereference --tar=f "$out"
      '';
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
      CAS_FIO = lib.optionalString pressure "${pkgs.fio}/bin/fio";
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
  # This fixed-workload guest never installs packages. Registration otherwise
  # tries to create /nix/store/.links on the immutable program image.
  nix.enable = false;
  services.timesyncd.enable = false;
  virtualisation = {
    diskImage = null;
    memorySize = 512;
    cores = 1;
    graphics = false;
    writableStore = false;
    mountHostNixStore = false;
    fileSystems."/nix/store" = {
      device = "/dev/disk/by-label/cas-programs";
      fsType = "erofs";
      neededForBoot = true;
      options = [ "ro" ];
    };
    useHostCerts = false;
    qemu = {
      forceAccel = false;
      networkingOptions = lib.mkForce [ "-nic none" ];
      enableSharedMemory = true;
      options = [
        "-machine accel=tcg"
        "-no-reboot"
        # SCSI keeps the vhost-user workload disk at /dev/vda.
        "-device virtio-scsi-pci,id=programs"
        "-drive if=none,id=programs,file=${programImage},format=raw,readonly=on"
        "-device scsi-hd,drive=programs,bus=programs.0"
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
  boot.initrd.availableKernelModules = [
    "virtio_scsi"
    "sd_mod"
  ];
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
      TimeoutStartSec = if pressure then 1000 else 150;
      ExecStart = lib.getExe workload;
      ExecStopPost = lib.getExe finish;
    };
  };
}
