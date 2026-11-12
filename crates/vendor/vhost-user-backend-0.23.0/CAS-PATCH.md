# CAS inflight-descriptor hook extension

Pinned published crate: `vhost-user-backend` 0.23.0.
Archive SHA-256: `555753b65bc33837bd011f981e2c6d52d8ddc330d92c2deb9aa3f0b378a58bcc`.
Recorded upstream commit: `4cc2d89ce4897fd90c4bbaf920a5f89a4420547c` in
`rust-vmm/vhost`, subdirectory `vhost-user-backend`. Original source headers,
metadata, tests and the Apache-2.0 license are retained.

The CAS change adds default-rejecting `GET_INFLIGHT_FD` and `SET_INFLIGHT_FD`
hooks to both backend traits (`VhostUserBackend` and `VhostUserBackendMut`),
forwards them through the `Arc`, `Mutex` and `RwLock` wrappers, and routes the
negotiated handler requests to those hooks. It changes no feature
advertisement and no queue processing.

CAS live recovery keeps a bounded inflight trailer in the memfd that QEMU
retains across daemon replacement. The trailer, its ownership rules and replay
belong to the daemon, not to this fork; the fork only exposes the protocol
messages. See [the inflight encoding](../../../docs/inflight-format.md).

Upgrade only with a new provenance record here and in
[`../README.md`](../README.md), and rerun the reference, protocol,
live-recovery and multiqueue checks named in `docs/validation.md`.
