{
  lib,
  pkgs,
  modulesPath,
  backend ? "cas",
  guestCores ? 1,
  probeTools ? false,
  ...
}:
{
  imports = [ (modulesPath + "/virtualisation/qemu-vm.nix") ];
  networking.hostName = "cas-lab-guest";
  system.stateVersion = "26.05";
  documentation.enable = false;
  services.timesyncd.enable = false;
  environment.systemPackages =
    with pkgs;
    [
      fio
      sqlite
      util-linux
      e2fsprogs
    ]
    ++ lib.optionals probeTools [ bpftrace ];
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
    };
  };
  systemd.services.sshd = {
    unitConfig.RequiresMountsFor = [ "/results" ];
    preStart = builtins.readFile ../guest/sshd-pre-start.sh;
    postStart = builtins.readFile ../guest/sshd-post-start.sh;
  };
  virtualisation = {
    diskImage = null;
    memorySize = if probeTools then 1024 else 512;
    cores = guestCores;
    graphics = false;
    writableStore = false;
    useHostCerts = false;
    qemu = {
      forceAccel = false;
      enableSharedMemory = true;
      networkingOptions = lib.mkForce [
        ''-nic "user,model=virtio-net-pci,restrict=on,hostfwd=tcp:127.0.0.1:$CAS_SSH_PORT-:22"''
      ];
      options = [
        "-machine accel=tcg"
        "-no-reboot"
      ]
      ++ (
        if backend == "raw" then
          [
            ''-drive "if=none,id=data,file=$CAS_RAW_IMAGE,format=raw,cache=none,aio=io_uring,werror=report,rerror=report"''
            "-device virtio-blk-pci,drive=data,logical_block_size=4096,physical_block_size=4096,num-queues=1"
          ]
        else
          [
            ''-chardev "socket,id=cas,path=$CAS_VHOST_SOCKET"''
            "-device vhost-user-blk-pci,chardev=cas,num-queues=${
              if backend == "daemon" then "1,queue-size=128" else "4,queue-size=256"
            }"
          ]
      );
    };
    sharedDirectories.results = {
      source = ''"$CAS_RESULTS_DIR"'';
      target = "/results";
      securityModel = "none";
    };
  };
  systemd.services.cas-lab-data = {
    wantedBy = [ "multi-user.target" ];
    after = [
      "local-fs.target"
      "systemd-udev-settle.service"
    ];
    wants = [ "systemd-udev-settle.service" ];
    unitConfig.RequiresMountsFor = [ "/results" ];
    path = with pkgs; [
      coreutils
      util-linux
      e2fsprogs
      systemd
    ];
    script = builtins.readFile ./guest.sh;
    serviceConfig = {
      Type = "simple";
      TimeoutStopSec = 30;
    };
  };
}
