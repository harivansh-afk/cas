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
  outputs = [
    "out"
    "tests"
  ];

  # Preserve the executable tested by this derivation for filesystem fixtures.
  # Require exactly one match; never silently substitute a stale binary.
  postCheck = ''
    coreTests=()
    for candidate in target/*/release/deps/cas_core-* target/release/deps/cas_core-*; do
      if [[ -f "$candidate" && -x "$candidate" ]]; then
        coreTests+=("$candidate")
      fi
    done
    test "''${#coreTests[@]}" -eq 1
    install -Dm755 "''${coreTests[0]}" "$tests/bin/cas-core-tests"
    daemonTests=()
    for candidate in target/*/release/deps/cas_daemon-* target/release/deps/cas_daemon-*; do
      if [[ -f "$candidate" && -x "$candidate" ]]; then
        if "$candidate" --list > "$TMPDIR/daemon-test-inventory" 2> "$TMPDIR/daemon-test-probe-error" &&
          grep -Fxq 'local::host::tests::multiple_reactors_compact_private_images_and_reopen_shared_chunks: test' "$TMPDIR/daemon-test-inventory"; then
          daemonTests+=("$candidate")
        fi
      fi
    done
    test "''${#daemonTests[@]}" -eq 1
    install -Dm755 "''${daemonTests[0]}" "$tests/bin/cas-daemon-tests"
  '';

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
