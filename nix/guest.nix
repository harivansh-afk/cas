{
  lib,
  pkgs,
  modulesPath,
  casBackend ? "raw",
  ...
}:
{
  imports = [ (modulesPath + "/virtualisation/qemu-vm.nix") ];

  networking.hostName = "cas-guest";
  system.stateVersion = "26.05";
  documentation.enable = false;
  services.openssh.enable = false;
  services.timesyncd.enable = false;

  virtualisation = {
    diskImage = null; # Ephemeral tmpfs root; only the experiment disk persists.
    memorySize = 1024;
    cores = 2;
    graphics = false;
    writableStore = false;
    useHostCerts = false;
    qemu = {
      forceAccel = true; # Refuse to turn an unavailable KVM into a TCG run.
      networkingOptions = lib.mkForce [ "-nic none" ];
      enableSharedMemory = casBackend == "daemon";
      options = [
        "-no-reboot"
      ]
      ++ (
        if casBackend == "daemon" then
          [
            ''-chardev "socket,id=cas,path=$CAS_VHOST_SOCKET"''
            "-device vhost-user-blk-pci,chardev=cas,num-queues=1,queue-size=128"
          ]
        else
          [
            ''-drive "if=none,id=experiment,file=$CAS_RAW_IMAGE,format=raw,cache=none,aio=io_uring,werror=report,rerror=report"''
            "-device virtio-blk-pci,drive=experiment,serial=cas-experiment,logical_block_size=4096,physical_block_size=4096,num-queues=1"
          ]
      );
    };
    sharedDirectories.results = {
      source = ''"$CAS_RESULTS_DIR"'';
      target = "/results";
      securityModel = "none";
    };
  };

  environment.etc."cas/smoke.fio".source = ../experiments/fio/smoke.fio;
  environment.etc."cas/queue.fio".source = ../experiments/fio/queue.fio;
  systemd.services.cas-smoke = {
    description = "Verify guest IO through the selected block backend";
    wantedBy = [ "multi-user.target" ];
    after = [
      "local-fs.target"
      "systemd-udev-settle.service"
    ];
    wants = [ "systemd-udev-settle.service" ];
    unitConfig.RequiresMountsFor = [ "/results" ];
    path = [
      pkgs.fio
      pkgs.util-linux
      pkgs.coreutils
    ];
    serviceConfig = {
      Type = "oneshot";
      TimeoutStartSec = 70;
    };
    script = ''
      disk=/dev/disk/by-id/virtio-cas-experiment
      test -b "$disk"
      test "$(blockdev --getss "$disk")" = 4096
      cp /etc/cas/smoke.fio /results/smoke.fio
      uname -a > /results/guest-kernel.txt
      fio --version > /results/guest-fio-version.txt
      lsblk --json --bytes --output NAME,TYPE,SIZE,LOG-SEC,PHY-SEC > /results/guest-disks.json
      fio --output-format=json+ --output=/results/fio.json /etc/cas/smoke.fio
      ${lib.optionalString (casBackend == "daemon") ''
        cp /etc/cas/queue.fio /results/queue.fio
        fio --output-format=json+ --output=/results/queue.json /etc/cas/queue.fio
      ''}
    '';
    postStop = ''
      printf '{"schema_version":1,"service_result":"%s","exit_code":"%s","exit_status":"%s"}\n' \
        "$SERVICE_RESULT" "''${EXIT_CODE:-unknown}" "''${EXIT_STATUS:-unknown}" > /results/completion.json
      ${pkgs.coreutils}/bin/sync
      # Root is disposable tmpfs. After syncing results, bypass systemd's
      # shutdown ramfs and its teardown of the shared host Nix store.
      ${pkgs.systemd}/bin/systemctl --force --force poweroff
    '';
  };
}
