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
      settings = {
        seconds = 1;
        repeats = 1;
        depths = [
          1
          4
        ];
        image_bytes = 128 * 1024 * 1024;
        data_bytes = 1024 * 1024;
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
      cases=()
      if test -f /results/cases; then
        cases=(--cases "$(cat /results/cases)")
      fi
      ${lib.getExe runner} --config ${config} --output /results/run "''${cases[@]}"
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
      TimeoutStartSec = 7200;
      ExecStart = lib.getExe workload;
      ExecStopPost = lib.getExe finish;
    };
  };
}
