{ lib, rustPlatform }:
rustPlatform.buildRustPackage {
  pname = "cas-research";
  version = (builtins.fromTOML (builtins.readFile ../Cargo.toml)).workspace.package.version;
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
