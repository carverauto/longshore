# Milestone 33 — IPsec packet offload on the ConnectX-7, against TLS and plain TCP

Measured 2026-11-26 between the two DGX Sparks (roberto 10.100.32.2 sends,
donbot 10.100.32.1 receives), the first interface of the 200 GbE port
(`enp1s0f0np0`, MTU 9000, about 110 Gbit/s), Linux `7.0.0-1019-nvidia`,
ConnectX-7 firmware 28.45.4028 with `CRYPTO_POLICY UNRESTRICTED`. Each end
one thread pinned to CPU 19 (Cortex-X925). GB = 10^9 bytes. Raw runs:
`m33-ipsec-packet-offload-dgx-spark-pair.jsonl`.

The question, from the cache design (`openspec/notes/s3-cache/`, 3.8): can
the card encrypt the wire, so that a server keeps sending zero-copy from
plaintext memory at the cost of plain TCP, instead of sealing every byte in
TLS?

## The three modes

- **plain**: TCP in the clear.
- **ipsec**: the same TCP, with IPsec ESP in transport mode, AES-GCM-128
  (`rfc4106(gcm(aes))`), one SA each way, and **packet offload** on both
  SAs and both policies: the card encrypts and decrypts, adds and strips
  the ESP headers. Only traffic between 10.100.32.1 and .2 is covered.
  Keys set by hand with `ip xfrm` from iproute2 6.19 (the hosts' 6.1 has
  no `offload packet`), built in the test user's home. Removed after the
  runs; both hosts were left with no SA and no policy.
- **tls**: TLS 1.3 through longshore's record layer, AWS-LC sealing.
  `tcp_uring --tls` (commit `59238de`) and `http_uring --tls-cert`. Sealed
  records are never sent zero-copy.

## The card does the work

- The driver accepted packet offload on every SA and policy (`crypto
  offload parameters: dev enp1s0f0np0 dir out mode packet`).
- The card's own counters: 5 pings were 5 packets encrypted on one card
  and 5 decrypted on the other. Over the short set, roberto's card
  encrypted 68.1 GB and donbot's decrypted 67.9 GB (and 20 GB the other
  way, the GETs); over both sets about 440 GB. `ipsec_rx_drop_*` and
  `ipsec_tx_drop_pkts` stayed at zero throughout.
- With packet offload the host sees plaintext on both sides (the card is
  past the stack), so `tcpdump` cannot show the ESP on the wire; the
  counters, and a connection that works only if the peer's card decrypts,
  are the evidence.

## Results

### Bulk TCP, 1 MiB frames, 4 connections (short set, 2 runs of 2 s)

| Mode | Sends | GB/s | Sender CPU-s a GB | Receiver CPU-s a GB |
|---|---|---|---|---|
| plain | zero-copy | 7.09, 7.16 | 0.068 | 0.140 |
| ipsec | zero-copy | 6.99, 6.99 | 0.082 | 0.142 |
| tls | sealed | 4.15, 4.07 | 0.172 | 0.243 |

CPU is each pinned thread's own time; the kernel's work on that thread is
in it, softirq on other cores is not. The receiver is the limit in every
row (its thread is 100% busy).

### Bulk TCP, the full matrix (3 rounds of 3 s, plain and ipsec only, older bench build)

Medians of three.

| Frame | Conns | Sends | plain GB/s | ipsec GB/s | plain send CPU-s/GB | ipsec send CPU-s/GB | plain recv CPU-s/GB | ipsec recv CPU-s/GB |
|---|---|---|---|---|---|---|---|---|
| 64 KiB | 1 | copied | 4.65 | 4.30 | 0.099 | 0.092 | 0.211 | 0.229 |
| 64 KiB | 1 | zero-copy | 4.54 | 4.35 | 0.076 | 0.083 | 0.214 | 0.227 |
| 64 KiB | 4 | copied | 4.95 | 4.99 | 0.081 | 0.079 | 0.201 | 0.200 |
| 64 KiB | 4 | zero-copy | 5.04 | 4.91 | 0.068 | 0.082 | 0.198 | 0.201 |
| 1 MiB | 1 | copied | 4.97 | 4.40 | 0.193 | 0.216 | 0.183 | 0.205 |
| 1 MiB | 1 | zero-copy | 5.09 | 5.74 | 0.102 | 0.109 | 0.179 | 0.170 |
| 1 MiB | 4 | copied | 5.42 | 4.74 | 0.184 | 0.211 | 0.165 | 0.172 |
| 1 MiB | 4 | zero-copy | 7.13 | 6.98 | 0.068 | 0.082 | 0.140 | 0.143 |

### HTTP, 8 MiB objects, 8 connections (`http_uring` against `http_uring sink`, one run of 3 s each)

| Mode | Request | GB/s | Client thread | GB a thread-CPU-second | ms of CPU a GB |
|---|---|---|---|---|---|
| plain | PUT | 13.31 | 0.39 | 34.2 | 29 |
| ipsec | PUT | 13.02 | 0.51 | 25.4 | 39 |
| tls | PUT | 2.92 | 0.38 | 7.7 | 130 |
| plain | GET | 6.20 | 0.97 | 6.4 | 157 |
| ipsec | GET | 6.55 | 0.97 | 6.7 | 148 |
| tls | GET | 3.87 | 0.97 | 4.0 | 252 |

PUT is the client sending (the cache serving a hit is this side); GET is
the client receiving. The TLS sink is rustls on the server side; the
client is longshore's record layer either way.

### Request and response, one connection, 4 KiB body (one run of 2 s each)

| Mode | Requests/s | Mean µs (from the rate) | p50 µs | p99 µs |
|---|---|---|---|---|
| plain | 22,408 | 45 | 36.9 | 164 |
| ipsec | 14,474 | 69 | 36.9 | 623 |
| tls | 9,680 | 103 | 32.8 | 754 |

## Reading it

- **IPsec packet offload works on these cards, and keeps zero-copy
  sends.** Encrypted on the wire, it carries about what plain TCP
  carries: 96 to 113% of plain's rate in the matrix's zero-copy cells and
  98% in the short set's, 98% on HTTP PUT with the link full at 13 GB/s.
  With copied sends it is 87 to 101%, lowest on 1 MiB frames (87 and
  89%). The card adds nothing on the receiving side of the zero-copy
  1 MiB cell (0.142 against 0.140 CPU-s a GB); in the matrix's other cells
  the receiver's cost moves by -5 to +12%.
- **It is not free on the sender: about 10 to 14 ms of CPU a GB more**
  (0.082 against 0.068 CPU-s a GB on zero-copy bulk; 39 against 29 ms a GB
  on HTTP PUT). The kernel still looks up the policy and builds the ESP
  framing for every segment, on the sending thread. Not profiled.
- **Against TLS it is 3 to 4.5 times cheaper and faster.** On the sending
  side TLS costs 130 ms of CPU a GB on HTTP PUT (milestone 12 measured the
  same) and holds a core to 2.9 GB/s; IPsec offload costs 39 ms and fills
  the link. On bulk TCP, TLS carries 58% of plain's rate at 2.5 times the
  sender's CPU a GB.
- **For receiving, the gain is smaller, as the receive copy dominates:**
  HTTP GET 6.55 GB/s over IPsec, 6.2 in the clear, 3.9 over TLS. The
  kernel's copy into user memory stays (`add-tcp-zero-copy-receive`).
- **Latency of small requests: not settled by these runs.** The medians
  are the same (37 µs) and the tails single 2-second runs. The rate says a
  request took 45 µs on average in the clear, 69 over IPsec and 103 over
  TLS, all with one connection that sleeps between requests; repeated runs
  are needed before reading anything into the 24 µs.

For the cache: a hit sent over IPsec packet offload costs about 39 ms of
CPU a GB, against 29 in the clear and 130 over TLS, and its bytes go
zero-copy from the chunk. Case 1 of the design's 3.8 holds on these cards;
the per-application end-to-end case (sealed at fill) is still the answer
where the network cannot be trusted to do it.

## Caveats

- Short runs: the short set is 2 runs of 2 s for bulk and one run each for
  HTTP and request/response, at the user's request; the matrix is 3 rounds
  of 3 s, plain and IPsec only, from an older copy of the bench whose two
  hosts' sources differed (the short set ran one build of `59238de` on
  both). The 1 MiB, 4-connection zero-copy cell agrees between the two
  sets (7.13 and 7.09 to 7.16 GB/s in the clear; 6.98 and 6.99 over IPsec).
- The modes ran in blocks (plain, then TLS, then IPsec), not interleaved.
  Both hosts are working k3s nodes; their background load is in the
  whole-machine figures of the raw file, which are not used above.
- One link only, so the second interface's capacity is not in these
  figures, and 13 GB/s is that link's limit for PUT.
- Keys by hand, no rekeying: in service an IKE daemon (strongSwan supports
  packet offload) would set them and rotate them. IPsec authenticates
  hosts, not applications.
- TLS 1.3 with AWS-LC only; ring and rustls were checked to run (loopback)
  but not measured here.

## Reproducing

```bash
# both hosts, as the test user: iproute2 with `offload packet`
curl -sO https://mirrors.edge.kernel.org/pub/linux/utils/net/iproute2/iproute2-6.19.0.tar.xz
tar xf iproute2-6.19.0.tar.xz && cd iproute2-6.19.0 && ./configure && make SUBDIRS="lib ip"

# on the sender (L=10.100.32.2, P=10.100.32.1), mirrored on the receiver
ip xfrm state add src $L dst $P proto esp spi 0x1001 reqid 0x1001 mode transport \
  aead 'rfc4106(gcm(aes))' $KEY_OUT 128 offload packet dev enp1s0f0np0 dir out
ip xfrm state add src $P dst $L proto esp spi 0x1002 reqid 0x1002 mode transport \
  aead 'rfc4106(gcm(aes))' $KEY_IN 128 offload packet dev enp1s0f0np0 dir in
ip xfrm policy add src $L/32 dst $P/32 dir out tmpl src $L dst $P proto esp \
  reqid 0x1001 mode transport offload packet dev enp1s0f0np0
ip xfrm policy add src $P/32 dst $L/32 dir in tmpl src $P dst $L proto esp \
  reqid 0x1002 mode transport offload packet dev enp1s0f0np0

# the runs (peer first, on donbot; the load on roberto)
taskset -c 19 tcp_uring bulk --listen 10.100.32.1:PORT --conns 4 --frame 1048576 --zc-sends 4 --secs 2
taskset -c 19 tcp_uring bulk --connect 10.100.32.1:PORT --conns 4 --frame 1048576 --zc-sends 4 --secs 2
# TLS: add --tls ~/ls-cert to both (made on donbot, copied to roberto)
http_uring sink --port PORT --size 8388608 [--tls-cert cert.der]
http_uring put|get --addr 10.100.32.1:PORT --conns 8 --size 8388608 --secs 3 --cpu 19 [--tls-cert cert.der]
ethtool -S enp1s0f0np0 | grep ipsec_
```
