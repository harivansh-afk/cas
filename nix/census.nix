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
}:
writeShellApplication {
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
  ];
  text = builtins.readFile ../experiments/census-pilot.sh;
}
