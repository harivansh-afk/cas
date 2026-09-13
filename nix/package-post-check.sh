# Preserve the exact test executables used by filesystem fixtures.
# Require exactly one match; never silently substitute a stale binary.

coreTests=()
for candidate in target/*/release/deps/cas_core-* target/release/deps/cas_core-*; do
  if [[ -f "$candidate" && -x "$candidate" ]]; then
    coreTests+=("$candidate")
  fi
done
test "${#coreTests[@]}" -eq 1
install -Dm755 "${coreTests[0]}" "$tests/bin/cas-core-tests"
daemonTests=()
for candidate in target/*/release/deps/cas_daemon-* target/release/deps/cas_daemon-*; do
  if [[ -f "$candidate" && -x "$candidate" ]]; then
    if "$candidate" --list > "$TMPDIR/daemon-test-inventory" 2> "$TMPDIR/daemon-test-probe-error" &&
      grep -Fxq 'local::host::tests::multiple_reactors_compact_private_images_and_reopen_shared_chunks: test' "$TMPDIR/daemon-test-inventory"; then
      daemonTests+=("$candidate")
    fi
  fi
done
test "${#daemonTests[@]}" -eq 1
install -Dm755 "${daemonTests[0]}" "$tests/bin/cas-daemon-tests"
