# A physical UEFI host provisioned with nixos-anywhere.
#
# Adds the disko layout from disks.nix and systemd-boot on top of test-host.nix,
# and refuses to build without an administrator key, since the installed system
# has no other way in.
{ config, lib, ... }:
{
  imports = [
    ./test-host.nix
    ./disks.nix
  ];

  boot.loader.systemd-boot.enable = true;
  boot.loader.efi.canTouchEfiVariables = false;

  assertions = [
    {
      assertion = config.cas.testbed.authorizedKeys != [ ];
      message = "Configure an administrator SSH public key before provisioning a test host.";
    }
  ];
}
