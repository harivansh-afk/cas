# CAS inflight metadata v3

Encoding for [live recovery](storage-design.md#live-recovery).
The pinned `vhost-user-backend` extension routes GET/SET_INFLIGHT_FD to
backend implementations and brackets frontend state changes with default-no-op
lifecycle hooks. Default FD methods reject both requests; adding hooks does
not advertise the feature. The patch changes `backend.rs`, `handler.rs`
and the `lib.rs` re-export. `--backend local-async --restartable` enables
the integrated four-queue path. Version 3 adds discovered identities; see
[Version 3](#version-3-discovered-identities).

The pinned `vhost` request decoder already rejects unnegotiated inflight
requests and validates FD/message framing before dispatch. The extension reuses
that gate. A socket test bypasses the typed frontend's own negotiation check to
prove that unnegotiated requests never reach a backend hook.

## Layout and ownership

The memfd contains standard split-queue regions first. Each uses the pinned
protocol's 16-byte queue header and 16-byte descriptor records, rounded up to
64 bytes per queue. At most four queues of 256 entries are supported. The CAS
trailer begins at the next 4 KiB boundary. It contains one 4 KiB header and one
128-byte private slot for each queue head. The total mapping is rounded up to
4 KiB. No payload bytes are stored in it.

The fd is shared with QEMU and sealed against growth and shrinkage. SET validates
the fd size/seals, mmap offset, complete geometry, trailer version, store/image
identity and writer/attachment identity before enabling queues. Mapping bounds
come from validated geometry, never unchecked shared indices. The supported
native targets are little-endian ARM64 and x86_64 with native 64-bit atomics.

The largest mapping is 155,648 bytes (four 256-entry queues). Reconciliation
also needs bounded temporary metadata for at most 1,024 saved entries; both
allocations belong to the host metadata budget when the carrier is integrated.
The standalone carrier does not acquire storage locks. The serving adapter owns
locked inspection before accepting the retained FD and choosing live repair.

Standard queue fields follow
[the pinned protocol](https://github.com/qemu/qemu/blob/v10.2.4/docs/interop/vhost-user.rst#inflight-io-tracking).
CAS publishes one used head per protocol completion transaction. Reconciliation
therefore accepts a wrapping used-index difference of zero or one, and checks
the last head before clearing it. It does not infer consumption from used.idx.

## Trailer header

All mutable fields use native aligned atomics. Integers are little-endian on the
supported targets. Unlisted bytes are zero and reserved.

| Offset | Bytes | Field |
|---|---|---|
| 0 | 8 | `CASIFL03`, published last during initialization |
| 8 | 4 | Version, 3 |
| 12 | 4 | Header bytes, 4096 |
| 16 | 16 | Store identity |
| 32 | 16 | Image identity |
| 48 | 8 | Writer epoch |
| 56 | 8 | Attachment generation |
| 64 | 4 | Queue count |
| 68 | 4 | Queue size |
| 72 | 4 | Private slot bytes, 128 |
| 76 | 4 | HEALTHY = 0, FAILED = 1 |
| 80 | 8 | Highest issued operation serial |
| 88 | 8 | Highest issued mutation sequence |
| 96 | 8 | Published mutation prefix P |
| 104 | 16 | Four next-available cursors, each stored in a u32 |
| 120 | 8 | Highest issued discovery identity |

Counters store the highest issued value, avoiding an unrepresentable next value
after u64::MAX. Check addition before admission; never wrap. A fresh attachment
initializes the mutation counter and P from the recovered image prefix.

## Private slot

| Offset | Bytes | Field |
|---|---|---|
| 0 | 4 | EMPTY = 0, PREPARED = 1, ACTIVE = 2, DISCOVERED = 3 |
| 4 | 2 | Request kind |
| 6 | 2 | Queue |
| 8 | 2 | Descriptor head |
| 10 | 2 | Original available-ring position |
| 12 | 4 | Outcome flags: bit 0 = admission rejected; all other bits zero |
| 16 | 8 | Operation serial |
| 24 | 8 | Mutation sequence, or zero for a nonmutation |
| 32 | 8 | Captured read/FLUSH boundary |
| 40 | 8 | Logical byte offset |
| 48 | 8 | Logical byte length |
| 56 | 8 | Attachment generation |
| 64 | 8 | Discovery identity |
| 72 | 56 | Reserved |

Kinds distinguish READ, WRITE, ZERO, FLUSH and protocol-only completions.
GET_ID, unsupported requests and admission rejections also need queue cursor
bookkeeping, but consume no mutation sequence. Empty ZERO similarly consumes
no mutation sequence. A malformed chain without usable status fails the
attachment. A protocol-only completion never produces a staging mutation.

A rejected admission retains the original request identity with its rejected
flag. It reserves a serial and available cursor but no mutation, payload or
storage IO. Its completion is always IOERR and does not await P. Recovery must
validate its original descriptors and preserve that outcome. Trailer v1 used
this word as reserved; v2 is a distinct, validated format. A fresh attachment
is required when changing from the prototype v1 carrier.

Admission initializes every slot field, release-publishes PREPARED, records the
standard counter/inflight flag, publishes ACTIVE, then updates issued counters
and the available cursor. The image admission lock covers the whole transaction.
Execution starts afterward. Recovery acquire-loads slot state before metadata.
PREPARED reserves its serial and mutation even if shared counters lag it.

Only the globally newest interrupted admission can need cursor repair: the
sequencer finishes a cursor update before starting another admission. Repair
may advance that slot's queue cursor by one when it still equals the slot's
original available position. An older inflight slot must never advance a cursor
merely because its 16-bit position matches after wrap; the issued serial rules
out that ambiguity. Tests must cover a long-lived old slot across ring wrap.

Completion holds the image completion lock through the HEALTHY check, private
P publication where applicable, standard last-head linkage, guest status/used
publication, standard inflight clear and used-index update, then private EMPTY.
Retirement changes only slot state. No head can be admitted for reuse between
those steps. Storage failure publishes FAILED under the same lock before IOERR.
Shared metadata is a live-guest recovery aid; it establishes no power-loss
durability and does not replace the locked-file IO exclusion barrier.

Negotiated C4 zero operations preserve their exact wire meaning in the kind:
WRITE_ZEROES without UNMAP uses 3, DISCARD uses 6, and WRITE_ZEROES with UNMAP
uses 7. Existing 1/2/4/5 values and the byte layout are unchanged. All three ZERO
forms consume a mutation only for nonempty ranges; older readers reject the new
kind values. Original descriptor replay validates the kind as well as the range.

## Version 3: discovered identities

Version 3 supports the [read ordering](storage-design.md#writes-reads-and-flush): a request can
be retained in the carrier before admission assigns it any storage identity.
The mapping geometry, the standard QEMU regions and the disk formats are
unchanged. Version 2 was validated as a distinct format; a version-2 retained
attachment cannot reconnect to a version-3 binary and needs a fresh attachment.

The header gains one field at offset 120: the highest issued discovery
identity. The slot gains state DISCOVERED = 3 and a discovery identity at
offset 64; the reserved tail shrinks to 56 bytes. Every admitted slot carries a
nonzero discovery identity, so the field is never zero in a used slot.

Discovery requires a HEALTHY trailer, a validated request, standard queue
version 1, an available cursor equal to the request's original position, an
EMPTY slot and a clear standard inflight flag. It writes the kind, queue, head,
available position, range, attachment generation, zero flags and the next
discovery identity, release-publishes DISCOVERED, then publishes the header's
discovery identity and advances that queue's available cursor by one. It
assigns no operation serial and no mutation sequence.

Admission of an EMPTY slot discovers it first. Admission then requires the
slot DISCOVERED with the same request and a clear standard inflight flag,
writes the flags, serial, mutation and boundary, and publishes PREPARED. The
rest of the transaction is unchanged from version 2.

Recovery accepts DISCOVERED beside PREPARED and ACTIVE. A DISCOVERED slot must
name its own queue and head, carry the current attachment generation and a
nonzero discovery identity, and have a clear standard inflight flag; it owns
no serial, mutation or boundary. No slot's discovery identity may exceed the
header's by more than one, and duplicate discovery identities fail the
attachment. Discovery owns the available cursor: only the newest discovery at
or above the header's value may repair its queue cursor, and only by one.
Replay returns admitted mutations and lists uncompleted DISCOVERED requests in
discovery order for the frontend to resubmit to admission.
