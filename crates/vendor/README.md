# Pinned vhost-user-backend extension

`vhost-user-backend-0.23.0/` starts from the published crate archive, SHA-256
`555753b65bc33837bd011f981e2c6d52d8ddc330d92c2deb9aa3f0b378a58bcc`.
Its recorded upstream commit is `4cc2d89ce4897fd90c4bbaf920a5f89a4420547c`
in `rust-vmm/vhost`, subdirectory `vhost-user-backend`. Original source headers,
metadata and tests are retained. The package is licensed Apache-2.0.

The CAS change adds default-rejecting GET/SET_INFLIGHT_FD hooks to both backend
traits, forwards them through Arc/Mutex/RwLock, and routes negotiated handler
requests to those hooks. It changes no feature advertisement or queue processing.
CAS's trailer, ownership and replay logic belong in the daemon, not this fork.
See [the encoding decision](../../docs/inflight-format.md) and C3 validation.

The root workspace excludes the vendored package from its own member/lint
policy while selecting it through `[patch.crates-io]`. Preserve upstream style
and keep the patch narrow. Upgrade only with a new provenance record and rerun
the reference, protocol, live-recovery and multiqueue checks.
