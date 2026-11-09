{
  pkgs,
  lib,
  nixpkgs,
  provenance,
  acceleration ? "kvm",
}:
let
  guests = lib.genAttrs [ "raw" "cas" ] (
    backend:
    (lib.nixosSystem {
      specialArgs = {
        inherit backend;
        probeTools = false;
        guestCores = 2;
        guestMemoryMiB = 1024;
        guestAcceleration = acceleration;
        guestQueues = 1;
        guestQueueSize = 128;
      };
      modules = [
        ../lab/guest.nix
        {
          nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system;
          # The workload guest never evaluates or installs Nix packages.
          nix.enable = false;
          # Each guest has its own restricted QEMU user network; no DHCP timing
          # or external network is needed for the host-forwarded SSH control.
          networking.useDHCP = false;
          networking.usePredictableInterfaceNames = false;
          boot.kernelModules = [ "virtio_net" ];
          systemd.services.fleet-network = {
            wantedBy = [ "multi-user.target" ];
            after = [ "systemd-modules-load.service" ];
            before = [ "sshd.service" ];
            path = [ pkgs.iproute2 ];
            script = ''
              ip link set dev eth0 up
              ip address replace 10.0.2.15/24 dev eth0
              ip route replace default via 10.0.2.2 dev eth0
            '';
            serviceConfig = {
              Type = "oneshot";
              RemainAfterExit = true;
            };
          };
          systemd.services.sshd = {
            requires = [ "fleet-network.service" ];
            after = [ "fleet-network.service" ];
          };
          networking.firewall.enable = false;
          systemd.services."serial-getty@ttyAMA0".enable = false;
          systemd.services.fleet-boot-evidence = {
            wantedBy = [ "multi-user.target" ];
            unitConfig.RequiresMountsFor = [ "/results" ];
            serviceConfig = {
              ExecStart = "${pkgs.systemd}/bin/journalctl -b -f --no-pager";
              StandardOutput = "append:/results/journal.log";
            };
          };
        }
      ];
    }).config.system.build.vm
  );
  build = pkgs.writeText "cas-two-host-build.json" (
    builtins.toJSON {
      inherit (provenance) source_revision source_path;
      inherit acceleration;
      system = pkgs.stdenv.hostPlatform.system;
      host = "${pkgs.cas}/bin/cas-host";
      fleet = "${pkgs.cas}/bin/cas-fleet";
      rsync = lib.getExe pkgs.rsync;
      qemu = lib.getExe' pkgs.qemu_kvm "qemu-system-${
        if pkgs.stdenv.hostPlatform.isAarch64 then "aarch64" else "x86_64"
      }";
      guests = lib.mapAttrs (_: vm: "${vm}/bin/run-cas-lab-guest-vm") guests;
    }
  );
in
pkgs.writeShellApplication {
  name = "cas-two-host";
  runtimeInputs = with pkgs; [
    python3
    openssh
    rsync
    coreutils
    util-linux
    iproute2
  ];
  text = ''export PYTHONDONTWRITEBYTECODE=1; exec python3 ${../../experiments/two-host}/runner.py --build-info ${build} "$@"'';
}
