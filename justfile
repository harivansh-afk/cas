check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --locked -- -D warnings
    cargo test --workspace --locked
    git diff --check

test:
    cargo test --workspace --locked

nix-check:
    nix fmt -- --ci
    nix flake check --no-build --all-systems
    nix flake check

vm-build:
    nix build .#vm-smoke --out-link result-vm

vm-smoke output:
    ./result-vm/bin/cas-vm-smoke --output {{quote(output)}}

daemon-build:
    nix build .#daemon-smoke --out-link result-daemon

daemon-smoke output:
    ./result-daemon/bin/cas-vm-smoke --output {{quote(output)}}

staging-build:
    nix build .#staging-smoke --out-link result-staging

staging-smoke output:
    ./result-staging/bin/cas-vm-smoke --output {{quote(output)}}

staging-recovery output:
    ./result-staging/bin/cas-vm-smoke --recovery --output {{quote(output)}}
