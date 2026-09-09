{
  writeShellApplication,
  cas,
  coreutils,
  curl,
  qemu_kvm,
  util-linux,
  e2fsprogs,
  jq,
  gawk,
  cloud-utils,
  OVMF,
}:
let
  normalize = writeShellApplication {
    name = "cas-normalize-root";
    runtimeInputs = [
      coreutils
      util-linux
      e2fsprogs
      jq
      gawk
    ];
    text = builtins.readFile ../experiments/normalize-root.sh;
  };
in
{
  inherit normalize;
  pilot = writeShellApplication {
    name = "cas-census-pilot";
    runtimeInputs = [
      cas
      coreutils
      curl
      qemu_kvm
      util-linux
      e2fsprogs
      jq
      gawk
      normalize
    ];
    text = builtins.readFile ../experiments/census-pilot.sh;
  };
  fleet = writeShellApplication {
    name = "cas-census-fleet";
    runtimeInputs = [
      cas
      coreutils
      qemu_kvm
      cloud-utils
      normalize
    ];
    text = ''
      exec cas-harness fleet \
        --firmware ${OVMF.fd}/FV/AAVMF_CODE.fd \
        --firmware-vars ${OVMF.fd}/FV/AAVMF_VARS.fd \
        --workload ${../experiments/update-guest.sh} "$@"
    '';
  };
}
