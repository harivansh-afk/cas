# One source-bound native development suite; the Rust runner owns validation.
{
  lib,
  writeShellApplication,
  writeText,
  cas,
  git,
  nix,
  util-linux,
  iproute2,
  qemu_kvm,
  fio,
  wrappers,
  provenance,
}:
let
  runtimeInputs = [
    cas.toolchain
    git
    nix
    util-linux
    iproute2
    qemu_kvm
    fio
  ];
  buildInfo = writeText "cas-checkpoint-build.json" (
    builtins.toJSON {
      inherit (provenance) source_revision source_path;
      harness = lib.getExe' cas "cas-harness";
      package = toString cas;
      tools = map toString runtimeInputs;
      wrappers = lib.mapAttrs (_: value: toString value) wrappers;
    }
  );
in
writeShellApplication {
  name = "cas-checkpoints";
  inherit runtimeInputs;
  text = ''
    exec ${lib.getExe' cas "cas-harness"} suite --build-info ${buildInfo} "$@"
  '';
  meta.description = "Run the source-bound CAS reference checkpoint suite";
}
