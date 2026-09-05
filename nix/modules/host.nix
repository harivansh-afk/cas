{
  config,
  lib,
  pkgs,
  ...
}:
{
  options.cas.testbed.authorizedKeys = lib.mkOption {
    type = lib.types.listOf lib.types.str;
    default = [ ];
    description = "SSH public keys permitted to administer this dedicated host.";
  };

  config = {
    services.openssh = lib.mkIf (config.cas.testbed.authorizedKeys != [ ]) {
      enable = true;
      settings = {
        PasswordAuthentication = false;
        KbdInteractiveAuthentication = false;
        PermitRootLogin = "prohibit-password";
      };
    };
    users.users.root.openssh.authorizedKeys.keys = config.cas.testbed.authorizedKeys;
    # Common settings for dedicated experiment hosts. Importing this module does
    # not start a workload, format a disk, or install a CAS daemon.
    nix.settings.experimental-features = [
      "nix-command"
      "flakes"
    ];
    boot.kernelPackages = lib.mkDefault pkgs.linuxPackages;
    powerManagement.cpuFreqGovernor = lib.mkDefault "performance";
    time.timeZone = "UTC";
    services.timesyncd.enable = true;
    networking.firewall.enable = true;
    environment.systemPackages = with pkgs; [
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
      uv
    ];
  };
}
