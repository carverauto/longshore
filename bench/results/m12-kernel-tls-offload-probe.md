# Milestone 12 — Can the network card do the TLS encryption?

A feasibility probe, run 2026-11-13 on the DGX Spark pair (host A to host B, Linux 6.14, Mellanox ConnectX-7, one link). The question: if the kernel is given the TLS keys and the card encrypts (`tls-hw-tx-offload`), how much cheaper is an HTTPS upload for the client than encrypting in our own record layer?

The probe is `crates/osc-bench/examples/ktls_probe.rs`. It does the handshake with rustls, hands the sending keys to the kernel with `TLS_TX`, and sends 8 MiB HTTP PUT bodies as plaintext on one blocking connection pinned to CPU 19, for 5 s. It is not the client: one connection, blocking sockets, no io_uring. It measures what the kernel path costs.

## Results

| Path | TLS | Who encrypts | GB per client CPU-second |
|---|---|---|---|
| Our record layer, default AWS-LC build (milestone 10) | 1.3 | client CPU | 5.9 |
| Our record layer, 8-way AES-GCM build (milestone 11) | 1.3 | client CPU | 7.6 |
| Kernel TLS, `send` | 1.3 | kernel, in software | 4.1 |
| Kernel TLS, `sendfile` | 1.3 | kernel, in software | 3.5 |
| Kernel TLS, `send` | 1.2 | **the card** | 6.8 |
| Kernel TLS, `sendfile` with `TLS_TX_ZEROCOPY_RO` | 1.2 | **the card** | **10.1** |
| Plain HTTP, zero-copy send (milestone 10) | none | nobody | 38 |

Throughput in the kernel TLS rows was 2.6 to 3.4 GB/s, limited by the single-threaded peer on one connection, so only the CPU cost per byte is meaningful there. One run each.

## What it shows

- **With TLS 1.3 the card does not take the work.** The kernel's own counter (`TlsTxDevice` in `/proc/net/tls_stat`) stayed at zero and the kernel encrypted in software, which costs the client more than our record layer does. On this kernel, device offload is offered for TLS 1.2 only.
- **With TLS 1.2 the card does encrypt**, the counter goes up, and the peer decrypts the stream without error.
- **The gain is about a third, not threefold.** The best case, `sendfile` with no copy, costs the client 99 ms of CPU per GB: better than our 130 ms, and nowhere near plain HTTP's 26 ms. Even with the cipher gone, the kernel still builds and tracks a 16 KiB record at a time, and that path is much heavier than a plain zero-copy send.
- **It does not fit the client as built.** The zero-copy form needs the data to come from a file, where the client sends from registered memory buffers; the copying form (6.8) is slower than our own record layer with the fast cipher (7.6).

## Conclusion

Not worth adopting now. It would mean speaking TLS 1.2, depending on particular network cards, and reworking how bodies reach the socket, for roughly a 30% lower CPU cost on uploads and nothing on downloads. The in-tree record layer stays. Worth another look when kernels offload TLS 1.3 to the card.

## Caveats

- One connection, one run per row, peer-limited throughput.
- The TLS 1.2 peer was `openssl s_server`; the TLS 1.3 peer was our own sink.
- Receive-side offload was not tried (`tls-hw-rx-offload` is off on these cards).
