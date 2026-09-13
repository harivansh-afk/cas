{
  lib,
  pkgs,
  cas,
  guests,
  ...
}:
let
  build = pkgs.writeText "cas-lab-host.json" (
    builtins.toJSON {
      host = "${cas}/bin/cas-host";
      daemon = "${cas}/bin/cas-daemon";
      guest = "${guests.cas.config.system.build.vm}/bin/run-cas-lab-guest-vm";
      daemon_guest = "${guests.daemon.config.system.build.vm}/bin/run-cas-lab-guest-vm";
      raw_guest = "${guests.raw.config.system.build.vm}/bin/run-cas-lab-guest-vm";
    }
  );
  finish = pkgs.writeShellApplication {
    name = "cas-lab-finish";
    runtimeInputs = with pkgs; [
      coreutils
      systemd
    ];
    text = ''
      journalctl -u cas-fixture --no-pager > /results/service.log
      sync -f /fixture
      sync -f /results
      systemctl --force --force poweroff
    '';
  };
in
{
  imports = [ ../fixture/guest.nix ];
  networking.hostName = lib.mkForce "cas-lab-host";
  networking.useDHCP = lib.mkForce true;
  virtualisation.memorySize = lib.mkForce 4096;
  virtualisation.qemu.networkingOptions = lib.mkForce [
    ''-nic "user,model=virtio-net-pci,restrict=on,hostfwd=tcp:127.0.0.1:$CAS_SSH_PORT-:22"''
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
      AllowTcpForwarding = "local";
    };
  };
  systemd.services.sshd = {
    unitConfig.RequiresMountsFor = [ "/results" ];
    preStart = builtins.readFile ../guest/sshd-pre-start.sh;
    postStart = builtins.readFile ../guest/sshd-post-start.sh;
  };
  systemd.services.cas-fixture = {
    path = with pkgs; [
      coreutils
      util-linux
      xfsprogs
    ];
    serviceConfig = {
      Type = lib.mkForce "simple";
      TimeoutStartSec = lib.mkForce "infinity";
      ExecStart = lib.mkForce "${cas}/bin/casctl lab-host ${build}";
      ExecStopPost = lib.mkForce (lib.getExe finish);
    };
  };
}
