# Dedicated test host

This template defines one **UEFI** host with an ext4 OS disk and a separate XFS
experiment disk at `/srv/cas-testbed`. Both disks are formatted by the initial
installation. It is for dedicated experiment machines.

Before installation, edit `flake.nix`:

1. Set the hostname and architecture. The example is an AMD x86_64 host.
2. Replace both disk paths with the actual `/dev/disk/by-id/` paths. Verify they
   identify two different physical devices reserved for this host.
3. Add your administrator SSH public key. Password login is disabled.
4. Check the boot mode, storage drivers, networking, and KVM module against the
   target's hardware. CloudLab's installation/boot procedure is not yet verified.

Lock the inputs and inspect the configuration before deploying:

```sh
nix flake lock
nix build .#nixosConfigurations.node-a.config.system.build.toplevel
```

From the CAS repository's `nix develop` shell, install using the absolute path
to this host flake and the target's SSH address:

```sh
nixos-anywhere --flake /path/to/host-flake#node-a root@TEST_HOST
```

This installation **erases both declared disks**. Ordinary updates use
`nixos-rebuild`, which does not rerun the disk formatter:

```sh
nixos-rebuild switch --flake /path/to/host-flake#node-a --target-host root@TEST_HOST
```

Reboot after changing the kernel before running an experiment. Confirm the
running versions and disk/cache state for every measurement. Add a second
`nixosConfigurations.node-b` with its own identity and disk IDs for the peer.

The template installs experiment tools and `casctl`. It does not start workloads,
create a ZFS comparator, or automatically start the CAS daemon. Result archives
belong on the OS disk; raw test images belong on the experiment disk.
