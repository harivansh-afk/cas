{
  lib,
  pkgs,
  modulesPath,
  cas,
  ...
}:
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
    path = [
      pkgs.coreutils
      pkgs.util-linux
      pkgs.e2fsprogs
    ];
    serviceConfig = {
      Type = "oneshot";
      TimeoutStartSec = 150;
    };
    script = ''
      exec > /results/workload.log 2>&1
      phase=$(cat /results/phase)
      image=$(cat /results/image)
      uname -a > /results/kernel.log
      lsblk --bytes --json > /results/disks.json
      test "$(blockdev --getsize64 /dev/vda)" = 536870912
      test "$(blockdev --getss /dev/vda)" = 4096
      case "$phase" in
        write) mkfs.ext4 -F -b 4096 -E nodiscard,lazy_itable_init=0,lazy_journal_init=0 /dev/vda ;;
        verify) e2fsck -fn /dev/vda ;;
        *) exit 1 ;;
      esac
      mkdir -p /mnt/cas
      mount -t ext4 -o data=ordered /dev/vda /mnt/cas
      findmnt --json /mnt/cas > /results/mount.json
      tune2fs -l /dev/vda > /results/ext4.log
      ${cas}/bin/cas-harness filesystem --root /mnt/cas --output /results/workload --phase "$phase" --image "$image" --sqlite ${pkgs.sqlite}/bin/sqlite3
      if test -f /results/live-recovery; then
        touch /results/ready
        while ! test -f /results/resume; do sleep 0.05; done
        ${cas}/bin/cas-harness filesystem --root /mnt/cas --output /results/resumed --phase resume --image "$image" --sqlite ${pkgs.sqlite}/bin/sqlite3
      fi
      if test "$phase" = write; then
        fstrim -v /mnt/cas > /results/trim.log
      fi
      umount /mnt/cas
      blockdev --flushbufs /dev/vda
    '';
    postStop = ''
      printf '{"schema_version":1,"service_result":"%s","exit_code":"%s","exit_status":"%s"}\n' "$SERVICE_RESULT" "$EXIT_CODE" "$EXIT_STATUS" > /results/completion.json
      ${pkgs.systemd}/bin/journalctl -u cas-filesystem.service --no-pager > /results/service.log
      ${pkgs.coreutils}/bin/sync
      ${pkgs.systemd}/bin/systemctl --force --force poweroff
    '';
  };
}
