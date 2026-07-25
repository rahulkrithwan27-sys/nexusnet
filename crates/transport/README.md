# nexusnet-transport

Transport-layer connectivity for NexusNet: carrying protocol frames over real
sockets.

This is where `nexusnet-protocol`'s incremental decoder finally does its job. A
TCP read returns whatever bytes happen to have arrived — half a frame, or three
frames and a fragment — and `Connection` turns that back into whole frames.

## What's here

- **`Connection<S>`** — a framed connection generic over any async stream. TCP
  today; TLS and QUIC attach to the same type later, and tests drive it over an
  in-memory pipe with no sockets at all.
- **`TcpListener` / `tcp::connect`** — the stream transport, with a bounded
  connect timeout.
- **`UdpEndpoint`** — a datagram transport, one frame per datagram.
- **`ConnectionPool`** — reusable connections with idle expiry and automatic
  removal of connections left desynchronized by a failure.
- **`ReconnectPolicy` / `connect_with_retry`** — exponential backoff with jitter.
- **`TransportConfig`** — payload limits, buffer sizes, timeouts, socket options.

## Usage

```rust
use bytes::Bytes;
use nexusnet_protocol::{Frame, FrameType};
use nexusnet_transport::{tcp, TcpListener, TransportConfig};

let config = TransportConfig::default();
let listener = TcpListener::bind("127.0.0.1:0", config).await?;
let address = listener.local_addr()?;

tokio::spawn(async move {
    let (mut connection, _peer) = listener.accept().await?;
    while let Some(frame) = connection.recv().await? {
        connection.send(&frame).await?;   // echo
    }
    Ok::<_, nexusnet_transport::Error>(())
});

let mut client = tcp::connect(address, config).await?;
client.send(&Frame::new(FrameType::Data, 1, Bytes::from_static(b"hello"))?).await?;
let echoed = client.recv().await?.expect("echo");
# Ok::<(), nexusnet_transport::Error>(())
```

Binding to port `0` asks the OS for a free port, which `local_addr()` reports —
useful in tests.

## Closing semantics

A clean close at a frame boundary is `Ok(None)` from `recv()`. A close
**mid-frame** is `Error::UnexpectedEof` carrying how many bytes were stranded.
Conflating the two would turn silent data loss into an ordinary end-of-stream,
so they stay distinct.

`Error::is_fatal()` reports whether a connection survives an error. Protocol
errors and unexpected EOF desynchronize a stream and require closing it;
timeouts and oversized datagrams leave the endpoint usable.

## Stream vs datagram

The two transports differ in ways the API makes explicit:

| | TCP | UDP |
| --- | --- | --- |
| Framing | Reassembled across reads | One frame per datagram |
| Oversized input | Deferred until complete | `DatagramTooLarge` error |
| Ordering | Guaranteed | Not guaranteed |
| Close | Clean EOF detectable | No connection to close |

A datagram cannot be reassembled from parts, so an oversized one is an error
rather than something to wait on. The receive buffer is deliberately one byte
larger than the limit so truncation is detected rather than silently accepted.

## Nagle's algorithm

`nodelay` defaults to **true** — that is, Nagle is disabled. NexusNet sends
discrete frames, and Nagle delays small writes hoping to coalesce them, adding
latency for no benefit. Use `send_all` to batch deliberately instead.

## Connection pooling

Establishing a TCP connection costs a round trip before any data moves — more
once TLS is layered on. Pooling amortizes that. Two details make a pool correct
rather than merely fast:

- **Idle connections go stale.** A peer, load balancer, or NAT device drops idle
  connections without telling anyone. Reusing one that silently died turns a
  fresh request into a mysterious failure, so connections idle beyond
  `max_idle_time` are discarded rather than handed out.
- **A failed connection must not go back.** After a protocol error or truncated
  read the stream is desynchronized; returning it would spread that failure to
  an unrelated caller. `PooledConnection` detects fatal errors automatically and
  drops such connections — including on a clean peer close, since a closed
  connection cannot serve anyone else.

```rust
let pool = ConnectionPool::new(address, PoolConfig::default());

let mut connection = pool.get().await?;   // dials, or reuses an idle connection
connection.send(&frame).await?;
connection.recv().await?;
// Returned to the pool on drop, unless it broke.

println!("{:.0}% reused", pool.stats().reuse_ratio() * 100.0);
# Ok::<(), nexusnet_transport::Error>(())
```

`PoolStats` tracks connections created, reused, discarded, and expired.

## Reconnection

`ReconnectPolicy` is exponential backoff **with jitter**, which is the part
people leave out. Backoff alone still lets every client retry in lockstep and
hammer a service exactly as it tries to recover; jitter decorrelates them. Each
delay is uniform in `[d/2, d]` — still backing off, no longer synchronized.

Only connection *establishment* is retried. A mid-session failure surfaces to
the caller rather than being silently reconnected, since a transparent
reconnect would discard stream state the caller may care about.

## Testing

```bash
cargo test -p nexusnet-transport
```

Framing is tested over in-memory pipes (fast, deterministic, including a 7-byte
read buffer that forces reassembly) *and* over real loopback sockets, where 500
variable-length frames must arrive in order and multiple concurrent clients are
served. Pooling tests cover reuse, idle capacity, expiry, and the refusal to
reuse a broken connection; reconnection tests cover backoff bounds, jitter
distribution, and recovery once a server appears.

## Status

Implemented in **Phase 3**. QUIC, WebSocket, HTTP/2, HTTP/3, connection pooling,
and automatic reconnect are still to come. See
[`docs/roadmap.md`](../../docs/roadmap.md).

## License

Licensed under the MIT license. See [`LICENSE`](../../LICENSE).
