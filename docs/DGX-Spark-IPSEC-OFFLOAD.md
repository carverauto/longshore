# Encryption on the network card of a DGX Spark: IPsec packet offload

The ConnectX-7 in a DGX Spark can encrypt and decrypt IPsec traffic itself.
With **packet offload** the card does all of ESP: it encrypts, adds and
strips the headers, checks the tag, and counts. The host's TCP stack, and a
program on it, sees plaintext on both ends, so a sender keeps sending
zero-copy from its own memory while the wire carries ciphertext.

This page says how to turn it on between two hosts, check that the card is
doing it, and turn it off again. Everything here was run between the two
Sparks on 2026-11-26 (`bench/results/m33-ipsec-packet-offload-dgx-spark-pair.md`).
What it costs there:

| Mode | HTTP PUT, 8 MiB | Sender's CPU a GB |
|---|---|---|
| plain TCP | 13.3 GB/s | 29 ms |
| IPsec packet offload | 13.0 GB/s | 39 ms |
| TLS 1.3, longshore's record layer | 2.9 GB/s | 130 ms |

TLS offload on the same card is covered at the end: it exists, but not for
TLS 1.3, which is why IPsec is the way to put encryption on the card.

## What it gives and what it does not

- Every packet between the two addresses an SA covers is encrypted on the
  wire with AES-GCM, whatever program sent it. Nothing in the program
  changes: no TLS, no certificates, no handshake per connection.
- It authenticates **hosts** (whoever holds the key), not applications or
  users. A program that needs to know who its peer is still needs its own
  authentication (SigV4, a token, mTLS).
- With keys set by hand, as below, nothing rekeys and nothing survives a
  reboot. For service, an IKE daemon sets and rotates the keys ("In
  service", below).

## 1. Check that the card and the kernel can do it

All read-only. On each host:

```bash
# The card: a ConnectX-7 with crypto enabled in its firmware.
lspci | grep -i mellanox                      # MT2910 Family [ConnectX-7]
sudo mstconfig -d 0000:01:00.0 query | grep CRYPTO_POLICY
#        CRYPTO_POLICY                               UNRESTRICTED(1)

# Driver, firmware, and the offloads the interface offers.
ethtool -i enp1s0f0np0 | grep -E 'driver|firmware'
#   driver: mlx5_core
#   firmware-version: 28.45.4028 (NVD0000000087)
ethtool -k enp1s0f0np0 | grep -E 'esp|tls|macsec'
#   tx-esp-segmentation: on
#   esp-hw-offload: on [fixed]
#   esp-tx-csum-hw-offload: on [fixed]
#   tls-hw-tx-offload: on
#   macsec-hw-offload: on

# The kernel: IPsec offload in XFRM and in mlx5.
grep -E 'CONFIG_XFRM_OFFLOAD|CONFIG_INET_ESP_OFFLOAD|CONFIG_MLX5_EN_IPSEC' /boot/config-$(uname -r)
#   CONFIG_XFRM_OFFLOAD=y
#   CONFIG_INET_ESP_OFFLOAD=m
#   CONFIG_MLX5_EN_IPSEC=y
sudo dmesg | grep -i 'IPSec ESP acceleration'
#   mlx5_core 0000:01:00.0: mlx5e: IPSec ESP acceleration enabled

# The card's e-switch is in legacy mode (packet offload was accepted so;
# switchdev mode was not tried), and the steering mode it runs.
sudo devlink dev eswitch show pci/0000:01:00.0     # mode legacy ...
sudo devlink dev param show pci/0000:01:00.0 name flow_steering_mode   # value hmfs

# The card's IPsec counters exist (they stay at zero until an SA is used).
ethtool -S enp1s0f0np0 | grep ipsec_
```

`esp-hw-offload: on` says the card can do IPsec **crypto** offload. It does
not prove **packet** offload; only installing an SA with it does (step 4).
The Sparks ran Linux `7.0.0-1019-nvidia`.

Each Spark shows its one 200 GbE port as two interfaces, each about
110 Gbit/s: `enp1s0f0np0` (PCI `0000:01:00.0`) and `enP2p1s0f0np0`
(`0002:01:00.0`). An SA belongs to one interface. To encrypt both links,
give each interface's addresses SAs of their own (step 3, once per link).

## 2. An `ip` that can ask for packet offload

The Sparks' iproute2 is 6.1, whose `ip xfrm` knows only
`offload [dev DEV] dir DIR` (crypto offload). Asking for packet offload
needs iproute2 6.2 or later, where the syntax is
`offload [crypto|packet] dev DEV dir DIR`. Build one in your home directory
without touching the system's:

```bash
mkdir -p ~/ipr && cd ~/ipr
V=iproute2-6.19.0
curl -sO https://mirrors.edge.kernel.org/pub/linux/utils/net/iproute2/$V.tar.xz
curl -s https://mirrors.edge.kernel.org/pub/linux/utils/net/iproute2/sha256sums.asc \
  | grep " $V.tar.xz\$" | sha256sum -c -
tar xf $V.tar.xz && cd $V
./configure
make -j8 SUBDIRS="lib ip"        # make -C lib alone fails: the top-level makefile sets the include path
IP=~/ipr/$V/ip/ip
$IP -V                            # ip utility, iproute2-6.19.0
$IP xfrm state help 2>&1 | grep offload
#   [ offload [ crypto | packet ] dev DEV dir DIR ]
```

It needs `gcc`, `make`, `bison`, `flex` and `pkg-config`, which the Sparks
have. The examples below use `$IP` for this binary; `sudo` runs it by its
full path.

## 3. Install the SAs and policies

IPsec here is ESP in **transport mode**: the packet keeps its own IP
header, and its payload is encrypted. Each direction has its own **SA**
(security association: the key, the algorithm and an SPI, the number that
names it on the wire), and each host has a **policy** each way that says
which traffic must go through which SA.

### Choose the keys and the SPIs

The algorithm is `rfc4106(gcm(aes))`: AES-GCM as ESP uses it. Its key is
20 bytes, a 16-byte AES-128 key followed by a 4-byte salt, written as 40 hex
digits after `0x`; the tag is 128 bits. One key each way:

```bash
KEY_AB=0x$(openssl rand -hex 20)   # from A to B
KEY_BA=0x$(openssl rand -hex 20)   # from B to A
```

Both hosts need both keys. Move them over a channel you trust (ssh), keep
them out of shell history and logs, and never reuse a key with a new SA.
For AES-256-GCM the key is 36 bytes (`openssl rand -hex 36`); only
AES-128 was tried on these cards.

On the Sparks, for the first link (the addresses below are documentation
addresses, RFC 5737; put the link's own in their place):

| | host A, the sender | host B |
|---|---|---|
| host | roberto | donbot |
| address | 192.0.2.2 | 192.0.2.1 |
| interface | `enp1s0f0np0` | `enp1s0f0np0` |
| SPI of its outgoing SA | `0x1001` | `0x1002` |

### On host A (192.0.2.2)

```bash
A=192.0.2.2; B=192.0.2.1; DEV=enp1s0f0np0

# The SA for what A sends: encrypted on A's card.
sudo $IP xfrm state add src $A dst $B proto esp spi 0x1001 reqid 0x1001 \
  mode transport aead 'rfc4106(gcm(aes))' $KEY_AB 128 \
  offload packet dev $DEV dir out

# The SA for what A receives: decrypted on A's card.
sudo $IP xfrm state add src $B dst $A proto esp spi 0x1002 reqid 0x1002 \
  mode transport aead 'rfc4106(gcm(aes))' $KEY_BA 128 \
  offload packet dev $DEV dir in

# Everything from A to B goes out through the SA of reqid 0x1001 ...
sudo $IP xfrm policy add src $A/32 dst $B/32 dir out \
  tmpl src $A dst $B proto esp reqid 0x1001 mode transport \
  offload packet dev $DEV

# ... and everything from B to A must have come in through 0x1002.
sudo $IP xfrm policy add src $B/32 dst $A/32 dir in \
  tmpl src $B dst $A proto esp reqid 0x1002 mode transport \
  offload packet dev $DEV
```

### On host B (192.0.2.1): the mirror image

The same two SAs with the same SPIs and keys; only the directions turn
around.

```bash
A=192.0.2.2; B=192.0.2.1; DEV=enp1s0f0np0

sudo $IP xfrm state add src $B dst $A proto esp spi 0x1002 reqid 0x1002 \
  mode transport aead 'rfc4106(gcm(aes))' $KEY_BA 128 \
  offload packet dev $DEV dir out
sudo $IP xfrm state add src $A dst $B proto esp spi 0x1001 reqid 0x1001 \
  mode transport aead 'rfc4106(gcm(aes))' $KEY_AB 128 \
  offload packet dev $DEV dir in
sudo $IP xfrm policy add src $B/32 dst $A/32 dir out \
  tmpl src $B dst $A proto esp reqid 0x1002 mode transport \
  offload packet dev $DEV
sudo $IP xfrm policy add src $A/32 dst $B/32 dir in \
  tmpl src $A dst $B proto esp reqid 0x1001 mode transport \
  offload packet dev $DEV
```

What each part means:

- `src`/`dst` on a state: the outer addresses of the packets the SA
  carries. On a policy: the selector, here one address to one address
  (`/32`), every protocol and port. Only this traffic is touched; ssh and
  Kubernetes traffic on other addresses is not.
- `reqid`: ties a policy's template to the SA that serves it.
- `offload packet dev DEV dir out|in`: the card does the whole of ESP for
  this SA, on this interface, in this direction. Policies take
  `offload packet dev DEV` too: with packet offload the card also holds
  the policy. Without `packet` (or with an old `ip`) it would be crypto
  offload, where the kernel still builds the ESP packets.
- No `replay-window` and no `flag esn` were set in the runs above, so the
  receiver keeps no anti-replay window and the sequence number is 32 bits.
  Hand-set SAs like these are for measuring and testing; see "In service".

Between the two hosts' first commands and their last, traffic between the
two addresses is dropped: one side encrypts what the other does not yet
expect. Install both, then use the link.

## 4. Check that the card is doing it

The driver accepted packet offload if every SA and policy says
`mode packet`:

```bash
sudo $IP -s xfrm state | grep -E '^src|offload'
#   src 192.0.2.2 dst 192.0.2.1
#           crypto offload parameters: dev enp1s0f0np0 dir out mode packet
#   src 192.0.2.1 dst 192.0.2.2
#           crypto offload parameters: dev enp1s0f0np0 dir in mode packet
sudo $IP xfrm policy | grep -E '^src|dir|offload'
#           crypto offload parameters: dev enp1s0f0np0 mode packet
```

If the card or driver refused packet offload, `ip` prints the reason the
kernel gives and adds nothing (`dmesg` may have more).

Then send something and watch the card's counters move on both hosts:

```bash
ethtool -S enp1s0f0np0 | grep -E 'ipsec_(rx|tx)_(pkts|bytes)|ipsec_.*drop'
ping -c 5 192.0.2.1                       # from A
ethtool -S enp1s0f0np0 | grep -E 'ipsec_(rx|tx)_(pkts|bytes)|ipsec_.*drop'
```

On the Sparks, five pings were five packets in `ipsec_tx_pkts` on the
sender and five in `ipsec_rx_pkts` on the receiver, both ways. What the
counters mean:

| Counter | Counts |
|---|---|
| `ipsec_tx_pkts`, `ipsec_tx_bytes` | packets the card encrypted (bytes with ESP) |
| `ipsec_rx_pkts`, `ipsec_rx_bytes` | packets the card decrypted and passed up |
| `ipsec_tx_drop_pkts`, `ipsec_rx_drop_pkts` | packets the card dropped (a bad tag on receive, for one) |
| `ipsec_rx_drop_sadb_miss` | ESP packets for an SPI the card has no SA for |
| `ipsec_rx_drop_mismatch_sa_sel` | decrypted packets that did not match the SA's selector |
| `ipsec_rx_drop_sp_alloc` | packets dropped for want of state to hand up with them |

The counters keep counting since the driver loaded; they do not reset when
SAs are removed. Compare before and after.

`tcpdump` on either host shows plaintext TCP, not ESP: the host sees each
packet before the card encrypts it, and after the card decrypts it. That is
the point of packet offload, and it means the counters, and a connection
that only works if the other card decrypts, are the evidence. To see ESP
itself, capture on the wire between the two (a mirrored switch port), not on
either host.

## 5. Using it

Nothing in a program changes. Any TCP or UDP between the two addresses is
encrypted. longshore's zero-copy sends (`Op::SendZc`) keep working: the
card encrypts the bytes as it reads them from the sender's memory.

ESP adds 34 to 37 bytes a packet (8 of ESP header, 8 of IV, 16 of tag, 2
of trailer, up to 3 of padding); the kernel takes it out of the path MTU,
so TCP's segments shrink to fit. At MTU 9000 that is under half a percent.

What was measured with it on the Sparks (milestone 33):

- `tcp_uring bulk`, 1 MiB frames, 4 connections, zero-copy: 6.99 GB/s
  against 7.1 in the clear, the receiving thread the limit either way;
  the sender's thread 0.082 against 0.068 CPU-s a GB.
- `http_uring put`, 8 MiB, 8 connections: 13.0 GB/s against 13.3 (the
  link), 39 against 29 ms of CPU a GB.
- `http_uring get`: 6.55 against 6.20 GB/s; receiving is held by the
  kernel's copy, not by the encryption.
- The sender's extra 10 to 14 ms a GB is the kernel's own per-packet IPsec
  work (policy lookup, ESP framing) on the sending thread; not profiled.

## 6. Turn it off

Delete exactly what was added, on each host (host A shown; on host B swap
`$A` and `$B` and the SPIs):

```bash
sudo $IP xfrm policy delete src $A/32 dst $B/32 dir out
sudo $IP xfrm policy delete src $B/32 dst $A/32 dir in
sudo $IP xfrm state delete src $A dst $B proto esp spi 0x1001
sudo $IP xfrm state delete src $B dst $A proto esp spi 0x1002
sudo ip xfrm state | grep -c '^src'    # 0 if nothing else uses IPsec
sudo ip xfrm policy | grep -c '^src'
```

Do not use `ip xfrm state flush` or `ip xfrm policy flush` on a host where
anything else might use IPsec: they remove every SA and policy, not only
these. Deleting one host's SAs before the other's drops traffic between the
two until both are gone. A reboot removes everything set by hand.

## In service

Keys set by hand do not rotate, protect against no replay, and are lost
at reboot. For anything beyond a measurement:

- Let an IKE daemon negotiate and rekey the SAs. strongSwan can ask for
  packet offload (`hw_offload = packet` in the child SA's section of
  `swanctl.conf`), in recent versions; check the version on the host and
  that the SAs it installs show `mode packet` as in step 4.
- Turn on anti-replay and 64-bit sequence numbers (IKE does by default;
  by hand, `replay-window` and `flag esn`). Neither was tried with packet
  offload on these cards.
- The Sparks are Kubernetes nodes (k3s, Calico). Calico's own encryption
  option is WireGuard, which is done in software, not on the card. Keep SAs
  to the addresses they are meant for, and agree on changes to the cards
  first.

## TLS offload on the same card

The card can also encrypt TLS records itself (kernel TLS with
`tls-hw-tx-offload`, on by default here). Milestone 12 found the limits on
these cards:

- **TLS 1.2 only.** With TLS 1.3 the kernel encrypts in software
  (`TlsTxDevice` in `/proc/net/tls_stat` stays at zero), which costs more
  than longshore's own record layer.
- **Send only**: `tls-hw-rx-offload` is off.
- The cheap form, `sendfile` with `TLS_TX_ZEROCOPY_RO`, sends from a file,
  not from registered memory, and was still about four times plain TCP's
  CPU a byte (99 against 26 ms a GB).

So the card's TLS offload is not used. Putting the encryption on the card
is done with IPsec packet offload, below TLS, as above.

## See also

- `bench/results/m33-ipsec-packet-offload-dgx-spark-pair.md`: the
  measurements, raw runs and caveats.
- `bench/results/m12-kernel-tls-offload-probe.md`: the TLS offload probe.
- `openspec/notes/s3-cache/s3-cache-draft.md`, 3.8: why a cache wants the
  wire encrypted without sealing every hit.
- The kernel's `Documentation/networking/xfrm_device.rst`: crypto and
  packet offload in XFRM.
