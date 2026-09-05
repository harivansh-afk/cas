{ config, lib, ... }:
let
  cfg = config.cas.testbed;
in
{
  options.cas.testbed = {
    osDisk = lib.mkOption {
      type = lib.types.str;
      description = "Stable /dev/disk/by-id path of the OS disk to provision.";
    };
    dataDisk = lib.mkOption {
      type = lib.types.str;
      description = "Stable /dev/disk/by-id path of the separate, expendable test disk.";
    };
    authorizedKeys = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = "SSH public keys permitted to administer this dedicated host.";
    };
  };

  config = {
    assertions = [
      {
        assertion = cfg.osDisk != cfg.dataDisk;
        message = "CAS testbed OS and experiment disks must be different devices.";
      }
      {
        assertion =
          builtins.all
            (disk: builtins.match "/dev/disk/by-id/[^/]+" disk != null && !(lib.hasInfix "REPLACE" disk))
            [
              cfg.osDisk
              cfg.dataDisk
            ];
        message = "Fill in real, stable OS and experiment disk IDs before building a bare-metal host.";
      }
      {
        assertion = cfg.authorizedKeys != [ ];
        message = "Configure an administrator SSH public key before provisioning a test host.";
      }
    ];

    boot.loader.systemd-boot.enable = true;
    boot.loader.efi.canTouchEfiVariables = false;
    services.openssh = {
      enable = true;
      settings = {
        PasswordAuthentication = false;
        KbdInteractiveAuthentication = false;
        PermitRootLogin = "prohibit-password";
      };
    };
    users.users.root.openssh.authorizedKeys.keys = cfg.authorizedKeys;
    disko.devices.disk = {
      os = {
        type = "disk";
        device = cfg.osDisk;
        content = {
          type = "gpt";
          partitions = {
            ESP = {
              size = "512M";
              type = "EF00";
              content = {
                type = "filesystem";
                format = "vfat";
                mountpoint = "/boot";
                mountOptions = [ "umask=0077" ];
              };
            };
            root = {
              size = "100%";
              content = {
                type = "filesystem";
                format = "ext4";
                mountpoint = "/";
              };
            };
          };
        };
      };
      experiment = {
        type = "disk";
        device = cfg.dataDisk;
        content = {
          type = "gpt";
          partitions.data = {
            size = "100%";
            content = {
              type = "filesystem";
              format = "xfs";
              mountpoint = "/srv/cas-testbed";
              mountOptions = [ "noatime" ];
            };
          };
        };
      };
    };
  };
}
