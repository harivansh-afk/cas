# Common settings for a dedicated experiment host.
#
# Importing this module installs the pinned tools and key-only SSH. It does not
# start a workload, format a disk, or run a CAS daemon. `pkgs.cas` comes from
# this flake's overlay, which the exported `nixosModules.test-host` applies.
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.cas.testbed;
in
{
  options.cas.testbed.authorizedKeys = lib.mkOption {
    type = lib.types.listOf lib.types.str;
    default = [ ];
    description = "SSH public keys permitted to administer this dedicated host.";
  };

  config = {
    # SSH only appears once there is a key to log in with.
    services.openssh = lib.mkIf (cfg.authorizedKeys != [ ]) {
      enable = true;
      settings = {
        PasswordAuthentication = false;
        KbdInteractiveAuthentication = false;
        PermitRootLogin = "prohibit-password";
      };
    };
    users.users.root.openssh.authorizedKeys.keys = cfg.authorizedKeys;

    nix.settings.experimental-features = [
      "nix-command"
      "flakes"
    ];

    # Measurement hygiene: a fixed governor and clock, and a kernel the host
    # flake can still override.
    boot.kernelPackages = lib.mkDefault pkgs.linuxPackages;
    powerManagement.cpuFreqGovernor = lib.mkDefault "performance";
    time.timeZone = "UTC";
    services.timesyncd.enable = true;
    networking.firewall.enable = true;

    environment.systemPackages = with pkgs; [
      cas
      qemu_kvm
      fio
      xfsprogs
      nvme-cli
      pciutils
      ethtool
      iproute2
      util-linux
      git
      just
    ];
  };
}
