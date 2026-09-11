# The NixOS guest booted by cas-vm-smoke.
#
# A minimal KVM guest with a tmpfs root and one experiment disk.
# At boot the cas-smoke service runs the fio jobs from crates/harnesses/fio
# against that disk and writes results to a host-shared directory. Smoke guests
# have no network and power off; interactive guests stay available over SSH.
#
# `cas.guest.backend` chooses how the experiment disk reaches the guest:
#   raw      QEMU's own virtio-blk on a raw image file ($CAS_RAW_IMAGE)
#   daemon   cas-daemon over vhost-user, raw-file storage ($CAS_VHOST_SOCKET)
#   staging  cas-daemon over vhost-user, staging-log storage
# The environment variables are set by `cas-harness vm` when it starts QEMU.
{
  config,
  lib,
  pkgs,
  modulesPath,
  ...
}:
let
  cfg = config.cas.guest;
  vhostUser = cfg.backend != "raw";
  concurrent = cfg.backend == "local-async";

  # QEMU flags that attach the experiment disk. The guest sees a 4 KiB-sector
  # virtio-blk device either way; only what stands behind it changes.
  experimentDisk =
    if vhostUser then
      [
        ''-chardev "socket,id=cas,path=$CAS_VHOST_SOCKET,reconnect-ms=''${CAS_RECONNECT_MS:-0}"''
        "-device vhost-user-blk-pci,chardev=cas,num-queues=${
          if concurrent then "4,queue-size=256" else "1,queue-size=128"
        }"
      ]
    else
      [
        ''-drive "if=none,id=experiment,file=$CAS_RAW_IMAGE,format=raw,cache=none,aio=io_uring,werror=report,rerror=report"''
        "-device virtio-blk-pci,drive=experiment,serial=cas-experiment,logical_block_size=4096,physical_block_size=4096,num-queues=1"
      ];

  smoke = pkgs.writeShellApplication {
    name = "cas-guest-smoke";
    runtimeInputs = with pkgs; [
      fio
      util-linux
      coreutils
      systemd
      gnused
    ];
    text = builtins.readFile ./smoke.sh;
  };

  finish = pkgs.writeShellApplication {
    name = "cas-guest-finish";
    runtimeInputs = with pkgs; [
      coreutils
      systemd
    ];
    text = builtins.readFile ./finish.sh;
  };
in
{
  imports = [ (modulesPath + "/virtualisation/qemu-vm.nix") ];

  options.cas.guest.interactive = lib.mkEnableOption "SSH access after the guest IO checks";

  options.cas.guest.backend = lib.mkOption {
    type = lib.types.enum [
      "raw"
      "daemon"
      "staging"
      "local"
      "local-async"
    ];
    default = "raw";
    description = "Block backend that serves the experiment disk.";
  };

  config = {
    networking.hostName = "cas-guest";
    system.stateVersion = "26.05";
    documentation.enable = false;
    services.openssh = lib.mkIf cfg.interactive {
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
    systemd.services.sshd = lib.mkIf cfg.interactive {
      unitConfig.RequiresMountsFor = [ "/results" ];
      preStart = ''
        install -Dm600 /results/authorized_keys /etc/ssh/authorized_keys.d/root
      '';
      postStart = ''
        cp /etc/ssh/ssh_host_ed25519_key.pub /results/host-key.tmp
        mv /results/host-key.tmp /results/host-key.pub
      '';
    };
    environment.systemPackages = lib.optionals cfg.interactive [
      pkgs.fio
      (pkgs.writeShellApplication {
        name = "cas-poweroff";
        runtimeInputs = with pkgs; [
          util-linux
          coreutils
          systemd
        ];
        text = ''
          blockdev --flushbufs /dev/disk/by-id/virtio-cas-experiment
          sync
          systemctl --force --force poweroff
        '';
      })
    ];
    services.timesyncd.enable = false;

    virtualisation = {
      diskImage = null; # Ephemeral tmpfs root; only the experiment disk persists.
      memorySize = 1024;
      cores = if concurrent then 4 else 2;
      graphics = false;
      writableStore = false;
      useHostCerts = false;

      qemu = {
        forceAccel = true; # Refuse to turn an unavailable KVM into a TCG run.
        networkingOptions = lib.mkForce (
          if cfg.interactive then
            [
              ''-nic "user,model=virtio-net-pci,restrict=on,hostfwd=tcp:127.0.0.1:$CAS_SSH_PORT-:22"''
            ]
          else
            [ "-nic none" ]
        );
        # vhost-user needs guest memory in a file QEMU can share with the daemon.
        enableSharedMemory = vhostUser;
        options = [ "-no-reboot" ] ++ experimentDisk;
      };

      sharedDirectories.results = {
        source = ''"$CAS_RESULTS_DIR"'';
        target = "/results";
        securityModel = "none";
      };
    };

    # fio job files, read by smoke.sh as /etc/cas/<job>.fio.
    environment.etc = lib.mapAttrs' (
      name: _:
      lib.nameValuePair "cas/${name}" {
        source =
          ../../crates/harnesses/fio
          + "/${
            if concurrent && name == "queue.fio" then
              "queue-mq.fio"
            else if concurrent && name == "live.fio" then
              "live-mq.fio"
            else
              name
          }";
      }
    ) (builtins.readDir ../../crates/harnesses/fio);

    systemd.services.cas-smoke = {
      description = "Verify guest IO through the ${cfg.backend} block backend";
      wantedBy = [ "multi-user.target" ];
      after = [
        "local-fs.target"
        "systemd-udev-settle.service"
      ];
      wants = [ "systemd-udev-settle.service" ];
      unitConfig.RequiresMountsFor = [ "/results" ];
      serviceConfig = {
        Type = "oneshot";
        TimeoutStartSec = 70;
        ExecStart = "${lib.getExe smoke} ${cfg.backend}";
        ExecStopPost = "${lib.getExe finish} ${if cfg.interactive then "interactive" else "smoke"}";
      };
    };
  };
}
