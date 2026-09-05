{ lib, rustPlatform }:
rustPlatform.buildRustPackage {
  pname = "cas-research";
  version = "0.1.0";
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../crates
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;
  meta = {
    description = "Research storage primitives and command-line checks";
    mainProgram = "casctl";
    platforms = lib.platforms.linux;
  };
}
