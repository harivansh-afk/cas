check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --locked -- -D warnings
    cargo test --workspace --locked
    git diff --check

test:
    cargo test --workspace --locked

nix-check:
    nix fmt -- --check flake.nix nix/*.nix templates/test-host/flake.nix
    nix flake check --no-build --all-systems
    nix flake check

vm-build:
    nix build .#vm-smoke --out-link result-vm

vm-smoke output:
    ./result-vm/bin/cas-vm-smoke --output {{quote(output)}}
