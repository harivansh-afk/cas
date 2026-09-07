# The Rust workspace: casctl, cas-daemon, and cas-harness.
#
# The toolchain comes from rust-toolchain.toml so Nix and `cargo` agree on the
# compiler. Only the Cargo manifest, lock file, and crates enter the build
# source, so edits to docs or Nix never trigger a rebuild.
{
  lib,
  rust-bin,
  makeRustPlatform,
}:
let
  toolchain = rust-bin.fromRustupToolchainFile ../rust-toolchain.toml;
  rustPlatform = makeRustPlatform {
    cargo = toolchain;
    rustc = toolchain;
  };
  workspace = (builtins.fromTOML (builtins.readFile ../Cargo.toml)).workspace.package;
in
rustPlatform.buildRustPackage {
  pname = "cas-research";
  inherit (workspace) version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../crates
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;

  # Exposed for the development shell, which needs the same rustc and cargo.
  passthru = {
    inherit toolchain;
  };

  meta = {
    description = "Research storage primitives and command-line checks";
    mainProgram = "casctl";
    platforms = lib.platforms.linux;
  };
}
