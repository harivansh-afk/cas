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
    runtimeEnv = {
      CAS_FIRMWARE = "${OVMF.fd}/FV/AAVMF_CODE.fd";
      CAS_FIRMWARE_VARS = "${OVMF.fd}/FV/AAVMF_VARS.fd";
      CAS_WORKLOAD = "${../experiments/update-guest.sh}";
    };
    text = builtins.readFile ./run-fleet.sh;
  };
}
