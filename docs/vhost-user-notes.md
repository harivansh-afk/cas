# Vhost-user block adapter notes

Primary-source check: 2026-09-05. Scope: one QEMU queue backed by an existing
raw file, with asynchronous `io_uring` completion. This is transport bring-up;
it does not establish staging durability or transparent backend recovery.

## Dependencies and API

`vhost-user-backend` 0.23.0 is published and uses `vhost` 0.17,
`virtio-queue` 0.18, `vm-memory` 0.18, `virtio-bindings` 0.2.7, and
`vmm-sys-util` 0.15. Keep these versions compatible to avoid distinct Rust
types from duplicate crate versions. `vm-memory` needs `backend-mmap` and
`backend-atomic`. See the [release manifest](https://github.com/rust-vmm/vhost/blob/vhost-user-backend-v0.23.0/vhost-user-backend/Cargo.toml)
and [workspace versions](https://github.com/rust-vmm/vhost/blob/vhost-user-backend-v0.23.0/Cargo.toml).

Implement `VhostUserBackendMut` behind `Arc<Mutex<_>>`, with `Bitmap = ()`
and `Vring = VringMutex`. `update_memory` receives
`GuestMemoryAtomic<GuestMemoryMmap<()>>`; retain a memory snapshot while
servicing each request. `exit_event` uses `(EventConsumer, EventNotifier)`,
not the older `EventFd` interface. The trait also requires queue limits,
features, configuration access, and `handle_event`.
[Backend trait](https://github.com/rust-vmm/vhost/blob/vhost-user-backend-v0.23.0/vhost-user-backend/src/backend.rs)

The framework consumes queue kick notifications before invoking
`handle_event`. Custom event tokens must exceed `num_queues`: with one
queue, token 0 is the queue, 1 is reserved for exit, and 2 can identify
`io_uring` completions. Register the completion eventfd through
`daemon.get_epoll_handlers()[0].register_listener(...)` while **not holding
the backend mutex**: registration calls back into `num_queues`.
[Event loop](https://github.com/rust-vmm/vhost/blob/vhost-user-backend-v0.23.0/vhost-user-backend/src/event_loop.rs)

## Requests and completion

Walk validated descriptor chains, check header/data/status permissions and
capacity bounds, then enqueue IO. Retain the request's buffers until its
CQE arrives. Copy read results into guest memory, write status, then publish
the used entry and notify QEMU. If enabling `EVENT_IDX`, use the
disable/drain/enable/recheck pattern to avoid missing kicks; it is simpler
to leave this optional feature disabled initially.
[Queue access](https://github.com/rust-vmm/vhost/blob/vhost-user-backend-v0.23.0/vhost-user-backend/src/vring.rs)

Register a completion eventfd with `io_uring` and drain CQEs on notification.
`Fsync` is not implicitly ordered against earlier IO: wait for preceding
completions or use `IO_DRAIN`. The latter also prevents later SQEs from
starting before the drained operation finishes. The implementation uses a bounded set of outstanding requests
and orders FLUSH with `IO_DRAIN`.
[Eventfd registration](https://docs.rs/io-uring/0.7.14/io_uring/struct.Submitter.html#method.register_eventfd),
[Fsync ordering](https://github.com/tokio-rs/io-uring/blob/master/src/opcode.rs),
[submission flags](https://github.com/tokio-rs/io-uring/blob/master/src/squeue.rs)

## QEMU contract

QEMU 10.2.4's vhost-user block frontend requires protocol `CONFIG` and reads
the virtio block configuration from the backend. Advertise
`VHOST_USER_F_PROTOCOL_FEATURES` and implement bounded configuration reads.
Protocol `MQ` is optional for one queue: without it QEMU assumes a maximum
of one; with it QEMU queries `GET_QUEUE_NUM`. This is separate from guest
feature `VIRTIO_BLK_F_MQ`. Set `num-queues=1` explicitly.
[QEMU protocol negotiation](https://github.com/qemu/qemu/blob/v10.2.4/hw/virtio/vhost-user.c),
[block frontend](https://github.com/qemu/qemu/blob/v10.2.4/hw/block/vhost-user-blk.c)

Use shared guest RAM (for example a `memory-backend-memfd` with
`share=on`) so the backend can map memory passed over the Unix socket.
Limit advertised features to implemented behavior: modern virtio, block
size, bounded segments and FLUSH are sufficient for the initial disk.
Do not advertise discard, write-zeroes, packed rings, migration, or inflight
recovery without implementations.
[Memory sharing and protocol features](https://www.qemu.org/docs/master/interop/vhost-user.html)

`VhostUserDaemon::serve` accepts one connection, waits for it to end, and
treats disconnect as a normal outcome. Restarting the process is not an IO
recovery strategy: retained/replayed inflight requests require a separate
design and tests. Keep QEMU reconnect disabled for this milestone.
[Daemon lifecycle](https://github.com/rust-vmm/vhost/blob/vhost-user-backend-v0.23.0/vhost-user-backend/src/lib.rs),
[inflight IO tracking](https://www.qemu.org/docs/master/interop/vhost-user.html#inflight-i-o-tracking)
