# Evaluate the bare-metal host module against fixture values and assert the
# layout it produces. Nothing is built or deployed; this catches module
# regressions in `nix flake check`.
{
  nixpkgs,
  nixosModules,
  pkgs,
}:
let
  system = pkgs.stdenv.hostPlatform.system;

  host = nixpkgs.lib.nixosSystem {
    modules = [
      nixosModules.bare-metal
      {
        nixpkgs.hostPlatform = system;
        networking.hostName = "cas-config-check";
        system.stateVersion = "26.05";
        cas.testbed = {
          osDisk = "/dev/disk/by-id/test-fixture-os";
          dataDisk = "/dev/disk/by-id/test-fixture-data";
          # Evaluation fixture only; this configuration is never deployed.
          authorizedKeys = [
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA fixture"
          ];
        };
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
