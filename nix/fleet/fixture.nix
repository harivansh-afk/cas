{
  pkgs,
  lib,
  modulesPath,
  runner,
  ...
}:
let
  config = pkgs.writeText "cas-two-host-loopback.json" (
    builtins.toJSON {
      # Legacy TCG fixture has XFS only; the KVM appliance validates ZFS.
      # The multi-guest, burst and resume arms are not in the TCG default
      # list: the daemon's fixed 60-second attach-to-driver window is not met
      # by concurrently booting TCG guests. Request them with --cases on KVM.
      cases = [
        "flows-k1"
        "flows-k2"
        "reads-memory"
        "reads-storage"
        "raw-rsync"
        "independent-stores"
      ];
      settings = {
        seconds = 5;
        repeats = 1;
        depths = [
          1
          4
        ];
        image_bytes = 128 * 1024 * 1024;
        data_bytes = 1024 * 1024;
        burst = {
          images = 4;
          base_bytes = 4 * 1024 * 1024;
          unique_bytes = 1024 * 1024;
          # Two direct reads of distinct unique-region chunks, then the shared base.
          working_set = [
            "dd if=/mnt/cas/unique bs=4096 count=64 iflag=direct status=none | sha256sum"
            "dd if=/mnt/cas/unique bs=4096 count=64 skip=128 iflag=direct status=none | sha256sum"
            "sha256sum /mnt/cas/base"
          ];
        };
        resume = {
          setup_bytes = 2 * 1024 * 1024;
          working_set = [
            "dd if=/mnt/cas/data bs=4096 count=64 iflag=direct status=none | sha256sum"
            "dd if=/mnt/cas/data bs=4096 count=64 skip=256 iflag=direct status=none | sha256sum"
            "sha256sum /mnt/cas/data"
          ];
        };
      };
      nodes = [
        {
          storage = "/fleet-a/run";
          output = "/results/host-a";
          get = "127.0.0.1:17483";
          bulk = "127.0.0.1:17485";
        }
        {
          storage = "/fleet-b/run";
          output = "/results/host-b";
          get = "127.0.0.1:17484";
          bulk = "127.0.0.1:17486";
        }
      ];
    }
  );
  workload = pkgs.writeShellApplication {
    name = "cas-two-host-fixture-workload";
    runtimeInputs = [ pkgs.coreutils ];
    text = ''
      exec > /results/workload.log 2>&1
      args=()
      if test -f /results/cases; then
        args+=(--cases "$(cat /results/cases)")
      fi
      if test -f /results/overrides; then
        while IFS= read -r line; do
          test -n "$line" && args+=(--set "$line")
        done < /results/overrides
      fi
      ${lib.getExe runner} --config ${config} --output /results/run "''${args[@]}"
    '';
  };
  finish = pkgs.writeShellApplication {
    name = "cas-two-host-fixture-finish";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.systemd
    ];
    text = builtins.readFile ../fixture/finish.sh;
  };
in
{
  imports = [ (modulesPath + "/virtualisation/qemu-vm.nix") ];
  networking.hostName = "cas-two-host-fixture";
  networking.useDHCP = false;
  system.stateVersion = "26.05";
  documentation.enable = false;
  services.timesyncd.enable = false;
  virtualisation = {
    diskImage = null;
    memorySize = 6144;
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
        ''-drive "if=none,id=a,file=$CAS_FLEET_A,format=raw,cache=none,aio=io_uring"''
        "-device virtio-blk-pci,drive=a,serial=fleet-a"
        ''-drive "if=none,id=b,file=$CAS_FLEET_B,format=raw,cache=none,aio=io_uring"''
        "-device virtio-blk-pci,drive=b,serial=fleet-b"
      ];
    };
    sharedDirectories.results = {
      source = ''"$CAS_RESULTS_DIR"'';
      target = "/results";
      securityModel = "none";
    };
  };
  virtualisation.fileSystems = lib.genAttrs [ "/fleet-a" "/fleet-b" ] (mount: {
    device = "/dev/disk/by-id/virtio-${builtins.baseNameOf mount}";
    fsType = "xfs";
    autoFormat = true;
    options = [ "noatime" ];
  });
  systemd.services.cas-fixture = {
    wantedBy = [ "multi-user.target" ];
    after = [ "local-fs.target" ];
    unitConfig.RequiresMountsFor = [
      "/results"
      "/fleet-a"
      "/fleet-b"
    ];
    serviceConfig = {
      Type = "oneshot";
      TimeoutStartSec = 14400;
      ExecStart = lib.getExe workload;
      ExecStopPost = lib.getExe finish;
    };
  };
}
