{
  nixpkgs,
  nixosModules,
  pkgs,
  system,
}:
let
  host = nixpkgs.lib.nixosSystem {
    inherit system;
    modules = [
      nixosModules.bare-metal
      {
        networking.hostName = "cas-config-check";
        cas.testbed = {
          osDisk = "/dev/disk/by-id/test-fixture-os";
          dataDisk = "/dev/disk/by-id/test-fixture-data";
          # Evaluation fixture only; this configuration is never deployed.
          authorizedKeys = [
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA fixture"
          ];
        };
        system.stateVersion = "26.05";
      }
    ];
  };
  cfg = host.config;
in
assert builtins.all (item: item.assertion) cfg.assertions;
assert cfg.fileSystems."/".fsType == "ext4";
assert cfg.fileSystems."/srv/cas-testbed".fsType == "xfs";
assert cfg.services.openssh.settings.PasswordAuthentication == false;
assert cfg.disko.devices.disk.os.device != cfg.disko.devices.disk.experiment.device;
pkgs.writeText "cas-host-config-check.json" (
  builtins.toJSON {
    inherit system;
    root = cfg.fileSystems."/".fsType;
    experiment = cfg.fileSystems."/srv/cas-testbed".fsType;
    kernel = cfg.boot.kernelPackages.kernel.version;
  }
)
