{
  description = "Dedicated CAS research host";
  inputs.cas.url = "git+https://git.harivan.sh/harivansh-afk/cas.git";
  inputs.nixpkgs.follows = "cas/nixpkgs";

  outputs = { cas, nixpkgs, ... }: {
    nixosConfigurations.node-a = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        cas.nixosModules.bare-metal
        {
          networking.hostName = "node-a";
          networking.useDHCP = true;
          boot.initrd.availableKernelModules = [
            "nvme"
            "ahci"
            "xhci_pci"
          ];
          boot.kernelModules = [ "kvm-amd" ];
          cas.testbed = {
            osDisk = "/dev/disk/by-id/REPLACE_OS_DISK";
            dataDisk = "/dev/disk/by-id/REPLACE_TEST_DISK";
            authorizedKeys = [ ];
          };
          system.stateVersion = "26.05";
        }
      ];
    };
  };
}
