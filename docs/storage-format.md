# V2 storage encoding

Encoding used by the [local storage implementation](storage-design.md). All integers
are little-endian. Every IO address, length and offset is 4 KiB aligned. These
tables describe the byte representation checked by the encoder and recovery parser.
CRC32 uses the same algorithm as the v1 reference. A checksum is not authentication.

## Segment header

Each segment starts with this 4 KiB block. Unlisted bytes are zero. CRC32 covers
the full block with its checksum field zeroed.

| Offset | Bytes | Field |
|---|---|---|
| 0 | 8 | `CASSEG02` |
| 8 | 4 | Version, 2 |
| 12 | 4 | Header bytes, 4096 |
| 16 | 16 | Store identity |
| 32 | 16 | Image identity |
| 48 | 8 | Writer epoch |
| 56 | 8 | Segment number |
| 64 | 8 | Segment capacity, including header |
| 72 | 8 | Logical image bytes |
| 80 | 8 | Mutation prefix preceding this segment |
| 4092 | 4 | Header CRC32 |

Segment numbers come from a durable monotonic allocator; removed segments do
not make their numbers available again. A segment header and its directory
entry are synchronized before its first data submission. Rollover drains and
synchronizes the preceding segment.

The allocator retains the highest numbered segment, even when it contains no
live payload. It may retire that header only after a higher numbered successor
header and its directory entry are durable. Segment filenames are fixed-width
decimal numbers and must match their headers. Recovery also considers retained
rejected segment names when finding the high-water number. This lets the durable
header serve as the allocation record without a second mutable counter file.
The same retained header bounds the next cold-attachment writer epoch. Neither
GC nor recovery may remove the final allocation record. Creating the next
header is the reservation; data cannot be issued before it becomes durable.
The new cold attachment uses its new segment ticket as its epoch. Recovery
synchronizes the retained segments, publishes that new header and a recovery
FENCE, then synchronizes the fence before enabling guest IO. This ordering
keeps recovery writable even if the old segment's final fence slot is full.

The synchronous implementation reserves a final 4 KiB fence slot in each
segment. Rollover fences and synchronizes the old segment before making the new
one writable. Segment space is preallocated with `FALLOC_FL_KEEP_SIZE`, so
recovery can distinguish the written file length from reserved physical space.

## Batch header

A batch contains a 4 KiB header followed by at most 1 MiB of whole-block
payload. The first 64 bytes are its envelope; up to 63 descriptors follow.
Unused descriptor slots and reserved fields are zero. Header CRC32 covers all
4096 bytes with bytes 56–59 zeroed. Payload checksums are per WRITE descriptor.

| Offset | Bytes | Field |
|---|---|---|
| 0 | 8 | `CASBAT02` |
| 8 | 2 | Version, 2 |
| 10 | 2 | Kind: DATA = 1, FENCE = 2 |
| 12 | 2 | Descriptor count |
| 14 | 2 | Reserved |
| 16 | 8 | Segment number |
| 24 | 8 | Batch number within the segment |
| 32 | 4 | Total IO bytes |
| 36 | 4 | Payload bytes |
| 40 | 8 | First mutation sequence; FENCE uses its captured boundary |
| 48 | 8 | Last mutation sequence; FENCE uses its captured boundary |
| 56 | 4 | Header CRC32 |
| 60 | 4 | Reserved |

DATA has at least one descriptor, consecutive nonzero mutation sequences and
no payload gaps or overlaps. Each nonempty WRITE or ZERO consumes one sequence.
FENCE has no descriptors or payload and does not consume a sequence. Batch
numbers increase across DATA and FENCE within their segment.

## Mutation descriptor

Offsets below are relative to the descriptor's 64-byte slot.

| Offset | Bytes | Field |
|---|---|---|
| 0 | 8 | Operation serial |
| 8 | 8 | Mutation sequence |
| 16 | 8 | Attachment generation |
| 24 | 8 | Logical byte offset |
| 32 | 8 | Logical byte length |
| 40 | 4 | Offset within the batch payload |
| 44 | 4 | Payload length |
| 48 | 4 | Payload CRC32 |
| 52 | 2 | Kind: WRITE = 1, ZERO = 2 |
| 54 | 2 | Queue number |
| 56 | 2 | Descriptor head |
| 58 | 6 | Reserved |

WRITE has equal logical and payload lengths. ZERO has nonzero logical length
and zero payload offset, length and CRC. Both logical offsets and lengths are
whole blocks and must fit the segment header's image capacity. Empty ranges
are handled before encoding and reserve no mutation sequence.

The encoder gathers payload into the final allocation, then fills sequence and
framing metadata without relocating that payload. Normal admission charges the
allocation's full capacity, including unused packing space. IO uses only the
sealed prefix. Moving an owned buffer cannot change its allocation address.

Recovery validates the header before using its length, then checks descriptors,
range arithmetic, dense mutation order and payload coverage. It advances only
to the boundary supplied by that validated header; it never searches payload
for framing. An invalid terminal batch and its suffix are retained before
repair. Live recovery additionally requires coverage of P. Reclaimed payload
below durable D is skipped only through intact, validated headers, as specified
in C0. V1 and unsupported version opens are rejected, never converted implicitly.

## Chunk segments and batches

Segment header retains the existing staging header positions but has a distinct
`CASCHS02` magic. Version u32 2 at 8, header length at 12, store ID at 16..32,
segment number u64 at 56, capacity u64 at 64, CRC32 at 4092. Other bytes zero.
No image identity or image mutation epoch belongs in the shared chunk segment.
Headers and directories are durable before data. The global durable-header
allocator includes all chunk/WAL segments and archives; highest tickets survive
GC until a higher durable header exists. No mutable allocation ledger.

Chunk batch magic `CASCHB02`; otherwise envelope positions mirror WAL v2:
version u16 at 8, DATA kind 1 at 10, descriptor count at 12, segment u64 at 16,
batch u64 at 24, total bytes u32 at 32, payload bytes u32 at 36, first/last
physical record ordinals at 40/48, CRC32 at 56. Physical ordinals are dense
within the segment and are not image mutation sequences. Store batches do not
need a FENCE record: data-sync completion gates usable index publication.

Each 64-byte chunk descriptor is:

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 32 | BLAKE3 of the full 4096-byte chunk |
| 32 | 4 | Payload offset relative to batch payload |
| 36 | 4 | Payload length, exactly 4096 |
| 40 | 4 | Payload CRC32 |
| 44 | 20 | Zero reserved bytes |

Up to 63 descriptors follow the 64-byte envelope in the 4 KiB header. Payloads
are consecutive with no gaps, so a full chunk batch is 256 KiB including its
header. Zero content becomes a manifest hole, never a store payload. A real
all-zero hash value remains a valid hash; it is not an empty-index sentinel.

A 64-bit store address contains the segment ticket in its high 48 bits and
its payload block index in its low 16 bits. Tickets range from 1 through
2^48 - 1; block offsets range from 1 through 65535 (4 KiB units). Segment
capacity is at most 256 MiB, with the normal 64 MiB geometry unchanged. Address
construction checks alignment and both limits; exhaustion fails before segment
creation. Shared-store WAL and chunk allocation uses this common ticket bound.
No truncating cast or wrap is permitted. Zero is never a usable address.

## Manifest pages

One 4 KiB file header precedes append-only pages, reserving offset zero as the
empty-root sentinel. Common 64-byte page header:

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 8 | `CASMAN02` |
| 8 | 2 | Version 2 |
| 10 | 2 | Kind: LEAF 1, BRANCH 2, COMMIT 3, FILE 4 |
| 12 | 2 | Tree level; COMMIT uses root height; FILE uses zero |
| 14 | 2 | Entry count; zero for COMMIT/FILE |
| 16 | 8 | This page's file-local offset |
| 24 | 32 | Zero reserved bytes |
| 56 | 4 | Whole-page CRC32 |
| 60 | 4 | Zero reserved bytes |

Leaf level is zero; root height counts levels including the leaf and is <= 8.
Every child offset is aligned, nonzero, below its parent, and its level is one
less. Branch entries are (minimum logical block u64, child offset u64); capacity
252. Leaf entries use logical block units, with sorted nonoverlapping extents:
(start block u64, end block exclusive u64, hash[32], chunk byte offset u32,
kind u16, reserved[10]); capacity 63. With fixed chunks, hash extents contain one block and their chunk byte offset
is zero; ZERO may span blocks. The editor can represent ZERO by removing mappings into holes.

FILE payload: store ID at 64..80 and image bytes u64 at 80; remaining bytes
zero. COMMIT payload: store ID 64..80, image ID 80..96, generation u64 at 96,
root offset u64 at 104, D u64 at 112, image bytes u64 at 120; remaining bytes
zero. Empty root has offset/height zero. Nonempty root precedes COMMIT and has
its declared height. Generation is nonzero and checked against overflow.

Image identity stays out of tree pages. FICLONE keeps file-local offsets and
page CRCs; a writable clone appends a COMMIT for its new image and D=0 while
sharing the old tree. Catalog publication remains temp/fsync/rename/dirsync.
A complete selected root referencing missing/corrupt supposedly durable chunks
fails recovery; torn/incomplete metadata transactions leave the prior root.

The COW range editor detaches covered subtrees, copies boundary paths and
splits bounded nodes. It enforces the C0 8H+8 per-mapping bound before emitting pages. Reserve transaction memory and disk extents before writing; no
large ZERO operation may rebuild or walk the full map merely to erase it.
