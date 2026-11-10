{
  pkgs,
  lib,
  modulesPath,
  runner,
  ...
}:
{
  imports = [ (modulesPath + "/virtualisation/qemu-vm.nix") ];
  networking.hostName = "cas-fleet-appliance";
  networking.hostId = "ca500001";
  networking.useDHCP = false;
  networking.usePredictableInterfaceNames = false;
  networking.firewall.enable = false;
  system.stateVersion = "26.05";
  documentation.enable = false;
  nix.enable = false;
  services.timesyncd.enable = false;
  boot.supportedFilesystems = [ "zfs" ];
  boot.zfs.forceImportRoot = false;
  boot.kernelModules = [ "virtio_net" ] ++ lib.optional pkgs.stdenv.hostPlatform.isx86_64 "kvm-amd";
  boot.extraModprobeConfig = "options kvm_amd nested=1";
  environment.systemPackages = [
    runner
    pkgs.zfs
    pkgs.xfsprogs
  ];
  services.openssh = {
    enable = true;
    hostKeys = [
      {
        type = "ed25519";
        path = "/etc/ssh/ssh_host_ed25519_key";
      }
    ];
    settings = {
      PermitRootLogin = "prohibit-password";
      PasswordAuthentication = false;
      KbdInteractiveAuthentication = false;
      AuthorizedKeysFile = "/results/authorized_keys";
    };
  };
  systemd.services.fleet-storage = {
    wantedBy = [ "multi-user.target" ];
    before = [ "sshd.service" ];
    after = [ "systemd-modules-load.service" ];
    unitConfig.RequiresMountsFor = [
      "/results"
      "/fleet"
    ];
    path = [
      pkgs.zfs
      pkgs.iproute2
      pkgs.coreutils
    ];
    script = ''
      ip link set dev eth0 up
      ip address replace 10.0.2.15/24 dev eth0
      ip route replace default via 10.0.2.2 dev eth0
      zpool create -o cachefile=none -O mountpoint=none cas-zfs /dev/disk/by-id/virtio-zfs
      zfs create -o mountpoint=none cas-zfs/runs
    '';
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
    };
  };
  systemd.services.sshd = {
    requires = [ "fleet-storage.service" ];
    after = [ "fleet-storage.service" ];
    postStart = "cat /etc/ssh/ssh_host_ed25519_key.pub > /results/host-key.pub";
  };
  virtualisation = {
    diskImage = null;
    memorySize = 8192;
    cores = 6;
    graphics = false;
    writableStore = false;
    useHostCerts = false;
    qemu = {
      package = pkgs.qemu_kvm;
      forceAccel = true;
      networkingOptions = lib.mkForce [
        ''-nic "user,model=virtio-net-pci,hostfwd=tcp:$CAS_BIND_ADDRESS:17422-:22,hostfwd=tcp:$CAS_BIND_ADDRESS:17487-:17487"''
      ];
      options = [
        "-cpu host"
        "-no-reboot"
        ''-drive "if=none,id=xfs,file=$CAS_XFS_DISK,format=raw,cache=none,aio=io_uring"''
        "-device virtio-blk-pci,drive=xfs,serial=xfs"
        ''-drive "if=none,id=zfs,file=$CAS_ZFS_DISK,format=raw,cache=none,aio=io_uring"''
        "-device virtio-blk-pci,drive=zfs,serial=zfs"
      ];
    };
    sharedDirectories.results = {
      source = ''"$CAS_RESULTS_DIR"'';
      target = "/results";
      securityModel = "none";
    };
  };
  virtualisation.fileSystems."/fleet" = {
    device = "/dev/disk/by-id/virtio-xfs";
    fsType = "xfs";
    autoFormat = true;
    options = [ "noatime" ];
  };
}
