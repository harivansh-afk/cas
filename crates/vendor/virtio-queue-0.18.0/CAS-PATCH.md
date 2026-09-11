# CAS descriptor-head replay extension

Pinned published crate: `virtio-queue` 0.18.0.
Archive SHA-256: `f631bfd09362a9b17f0cfd5ca3b0a1b179d153fa276f9ce86919d560891e61d6`.
Original archive retained in `results/c3-replay-2026-09-11/`.

The sole upstream code change makes `DescriptorChain::new` public in
`src/chain.rs`. Constructor behavior and the bounded direct/indirect descriptor
walker are unchanged. Both original license files are retained.

CAS live recovery has a saved descriptor head whose original available-ring
slot may already have wrapped. It must walk that head directly, using the same
parser as ordinary admission. No guest payload is stored in shared inflight
metadata. The caller retains the accepted guest memory and validates queue/head
geometry before constructing a chain.
