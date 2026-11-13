# Vendored crates

Each directory is a published crate archive with one narrow CAS change. Every
crate carries a `CAS-PATCH.md` with the archive SHA-256, the upstream commit,
the exact change, why CAS needs it and the upgrade rule. Original headers,
metadata, tests and licenses are retained; the root workspace excludes these
packages from its own member and lint policy and selects them through
`[patch.crates-io]`. Preserve upstream style and keep each patch narrow.

| Crate | Change | Record |
|---|---|---|
| `vhost-user-backend-0.23.0` | Default-rejecting `GET/SET_INFLIGHT_FD` hooks on both backend traits, forwarded through the wrappers and routed from the handler | [CAS-PATCH.md](vhost-user-backend-0.23.0/CAS-PATCH.md) |
| `virtio-queue-0.18.0` | `DescriptorChain::new` made public so live recovery can walk a saved descriptor head with the ordinary parser | [CAS-PATCH.md](virtio-queue-0.18.0/CAS-PATCH.md) |

Upgrading either crate needs a new provenance record and a rerun of the
reference, protocol, live-recovery and multiqueue checks.
