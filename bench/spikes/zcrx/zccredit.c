// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

// Throwaway spike: what the kernel does with a zero-copy receive that is armed
// with a byte credit, with a full completion ring, and with no free page for
// its copy path. Three runs, each against its own sender (see the results).
//   zccredit IFACE QUEUE ADDR PORT credit FRAME   one connection of frames of FRAME
//       bytes, each starting with its length less four, big-endian; armed with
//       256 bytes, then with the rest, frame after frame
//   zccredit IFACE QUEUE ADDR PORT fullring BYTES  one connection that sends BYTES
//       and goes quiet; a completion ring of 64 entries
//   zccredit IFACE QUEUE ADDR PORT nomem           two connections: the first sends
//       much and is never returned, the second sends a little now and then
//   zccredit IFACE QUEUE ADDR PORT cancel BYTES    one connection of BYTES bytes, byte
//       i being i % 251, with a pause in them; the receive is cancelled in the
//       pause and armed again for the rest
//   zccredit IFACE QUEUE ADDR PORT copy PAGES      one connection on a port that is not
//       steered to the queue, an area of PAGES pages, no piece returned until a
//       receive has ended for want of a page; then all returned, and armed again
// credit also keeps the first frame's first page behind one of its two pieces,
// and says whether the page's bytes changed while 300 frames went by.
// Built with: gcc -O2 -Wall -o zccredit zccredit.c
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <linux/io_uring.h>
#include <net/if.h>
#include <netinet/in.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>
struct area_reg { uint64_t addr, len, rq_area_token; uint32_t flags, dmabuf_fd; uint64_t resv[2]; };
struct region { uint64_t user_addr, size; uint32_t flags, id; uint64_t mmap_offset, resv[4]; };
struct offsets { uint32_t head, tail, rqes, resv2; uint64_t resv[2]; };
struct ifq_reg { uint32_t if_idx, if_rxq, rq_entries, flags; uint64_t area_ptr, region_ptr; struct offsets offsets; uint32_t zcrx_id, rx_buf_len; uint64_t resv[3]; };
struct rqe { uint64_t off; uint32_t len, pad; };
#define OP_RECV_ZC 58
#define AREA_MASK (~(((uint64_t)1 << 48) - 1))
static double now(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec + t.tv_nsec / 1e9; }
static void die(const char *w) { printf("%s: %s\n", w, strerror(errno)); exit(1); }

static int ring; static unsigned *sq_tail, *sq_arr, sq_mask, *cq_head, *cq_tail, cq_mask, stail;
static struct io_uring_sqe *sqes; static char *cqes, *area;
static struct area_reg a; static struct ifq_reg reg;
static unsigned *rq_tail, *rq_head, rtail, rmask; static struct rqe *rqes;

static void setup(const char *ifn, unsigned q, unsigned cq_entries, size_t alen) {
    struct io_uring_params p; memset(&p, 0, sizeof p);
    p.cq_entries = cq_entries;
    p.flags = IORING_SETUP_SINGLE_ISSUER | IORING_SETUP_DEFER_TASKRUN | IORING_SETUP_CQE32 | IORING_SETUP_CQSIZE;
    ring = syscall(425, 64, &p); if (ring < 0) die("setup");
    char *sq = mmap(0, p.sq_off.array + p.sq_entries * 4, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_POPULATE, ring, IORING_OFF_SQ_RING);
    char *cq = mmap(0, p.cq_off.cqes + p.cq_entries * 32, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_POPULATE, ring, IORING_OFF_CQ_RING);
    sqes = mmap(0, p.sq_entries * sizeof *sqes, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_POPULATE, ring, IORING_OFF_SQES);
    if (sq == MAP_FAILED || cq == MAP_FAILED || (void *)sqes == MAP_FAILED) die("ring mmap");
    sq_tail = (unsigned *)(sq + p.sq_off.tail); sq_arr = (unsigned *)(sq + p.sq_off.array); sq_mask = p.sq_entries - 1;
    cq_head = (unsigned *)(cq + p.cq_off.head); cq_tail = (unsigned *)(cq + p.cq_off.tail); cq_mask = p.cq_entries - 1;
    cqes = cq + p.cq_off.cqes; stail = *sq_tail;
    printf("completion ring: %u entries\n", p.cq_entries);
    unsigned rq_entries = 4096;
    area = mmap(0, alen, PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE | MAP_POPULATE, -1, 0);
    size_t rlen = rq_entries * sizeof(struct rqe) + 4096;
    char *rq = mmap(0, rlen, PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE | MAP_POPULATE, -1, 0);
    a.addr = (uint64_t)area; a.len = alen;
    static struct region r; r.user_addr = (uint64_t)rq; r.size = rlen; r.flags = 1;
    reg.if_idx = if_nametoindex(ifn); reg.if_rxq = q; reg.rq_entries = rq_entries; reg.area_ptr = (uint64_t)&a; reg.region_ptr = (uint64_t)&r;
    if (syscall(427, ring, 32, &reg, 1) < 0) die("register zcrx");
    rq_tail = (unsigned *)(rq + reg.offsets.tail); rq_head = (unsigned *)(rq + reg.offsets.head); rqes = (struct rqe *)(rq + reg.offsets.rqes); rmask = rq_entries - 1;
}
static void give_back(uint64_t off, unsigned len) {
    struct rqe *e = &rqes[rtail++ & rmask]; e->off = (off & ~AREA_MASK) | a.rq_area_token; e->len = len; e->pad = 0;
    __atomic_store_n(rq_tail, rtail, __ATOMIC_RELEASE);
}
static void flush(void) {
    struct { uint32_t zcrx_id, op; uint64_t resv[2], body[6]; } f; memset(&f, 0, sizeof f); f.zcrx_id = reg.zcrx_id;
    if (syscall(427, ring, 36, &f, 0) < 0) die("flush");
}
static void arm(int fd, unsigned credit, uint64_t tag) {
    struct io_uring_sqe *s = &sqes[stail & sq_mask]; memset(s, 0, sizeof *s);
    s->opcode = OP_RECV_ZC; s->fd = fd; s->ioprio = IORING_RECV_MULTISHOT; s->len = credit; s->user_data = tag; s->file_index = reg.zcrx_id;
    sq_arr[stail & sq_mask] = stail & sq_mask; stail++;
    __atomic_store_n(sq_tail, stail, __ATOMIC_RELEASE);
}
static long enter(unsigned submit, unsigned min) {
    long rc = syscall(426, ring, submit, min, IORING_ENTER_GETEVENTS, 0, 0);
    if (rc < 0 && errno != EINTR && errno != EBUSY) die("enter");
    return rc;
}
static int listener(const char *addr, int port) {
    int ls = socket(AF_INET, SOCK_STREAM, 0), one = 1;
    setsockopt(ls, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    struct sockaddr_in sa = { .sin_family = AF_INET, .sin_port = htons(port) }; inet_pton(AF_INET, addr, &sa.sin_addr);
    if (bind(ls, (void *)&sa, sizeof sa) < 0 || listen(ls, 16) < 0) die("listen");
    fprintf(stderr, "zccredit: listening\n");
    return ls;
}

// One arming, to its last completion. Returns the bytes its data completions carried;
// *res and *flags are the last completion's. Pieces are noted through `first`/`n`.
struct got { uint64_t first_off; unsigned first_len, pieces; int res; unsigned flags; };
static uint64_t one_arming(int fd, unsigned credit, struct got *g, int keep_first) {
    uint64_t bytes = 0; unsigned submit = 1; memset(g, 0, sizeof *g);
    arm(fd, credit, 1);
    for (;;) {
        enter(submit, 1); submit = 0;
        unsigned head = *cq_head, tail = __atomic_load_n(cq_tail, __ATOMIC_ACQUIRE); int done = 0;
        for (; head != tail; head++) {
            struct io_uring_cqe *c = (void *)(cqes + (size_t)(head & cq_mask) * 32);
            uint64_t off = *(uint64_t *)(c + 1);
            if (!(c->flags & IORING_CQE_F_MORE)) { g->res = c->res; g->flags = c->flags; done = 1; continue; }
            if (!g->pieces) { g->first_off = off & ~AREA_MASK; g->first_len = c->res; }
            g->pieces++; bytes += c->res;
            if (!(keep_first && g->pieces == 1)) give_back(off, c->res);
        }
        __atomic_store_n(cq_head, head, __ATOMIC_RELEASE);
        if (done) return bytes;
    }
}

static int run_credit(int fd, unsigned frame) {
    unsigned frames = 0, bad_prefix = 0, hdr_one_piece = 0, hdr_at_page_start = 0, body_same_page = 0, body_wrong = 0, hdr_wrong = 0;
    unsigned ends[4] = {0}; struct got h, b; uint64_t kept = 0; unsigned kept_len = 0; int page_changed = -1, reused_after = -1;
    static unsigned char was[4096];
    for (; frames < 4000; frames++) {
        if (frames == 300 && kept_len) {
            // 300 MiB went through an area of 64: the page was not handed out again.
            page_changed = memcmp(was, area + (kept & ~4095ull), 4096) != 0;
            give_back(kept, kept_len);
        }
        if (frames == 1500 && kept_len) reused_after = memcmp(was, area + (kept & ~4095ull), 4096) != 0;
        uint64_t n = one_arming(fd, 256, &h, 1);
        if (n < 256) { printf("stream ended in a header: %llu of 256 bytes, last completion res %d flags %#x\n", (unsigned long long)n, h.res, h.flags); break; }
        if (n != 256) hdr_wrong++;
        ends[(h.res != 0) * 2 + (h.flags != 0)]++;
        unsigned char *p = (unsigned char *)area + h.first_off;
        unsigned said = (unsigned)p[0] << 24 | p[1] << 16 | p[2] << 8 | p[3];
        if (said != frame - 4) bad_prefix++;
        if (h.pieces == 1 && h.first_len == 256) hdr_one_piece++;
        if (!(h.first_off & 4095)) hdr_at_page_start++;
        n = one_arming(fd, frame - 256, &b, frames == 0);
        if (frames == 0 && b.first_off == h.first_off + 256 && !(h.first_off & 4095)) {
            kept = b.first_off; kept_len = b.first_len; memcpy(was, area + h.first_off, 4096);
        } else if (frames == 0) give_back(b.first_off, b.first_len);
        if (n != frame - 256) { body_wrong++; if (n < frame - 256) { printf("stream ended in a body: %llu of %u\n", (unsigned long long)n, frame - 256); break; } }
        ends[(b.res != 0) * 2 + (b.flags != 0)]++;
        // The body goes on where the header stopped, in the same page: a second reference on it.
        if (b.first_off == h.first_off + 256) body_same_page++;
        give_back(h.first_off, 256);
    }
    printf("{\"cmd\":\"zccredit\",\"frames\":%u,\"prefix_wrong\":%u,\"header_bytes_wrong\":%u,\"body_bytes_wrong\":%u,\"header_in_one_piece\":%u,\"header_at_page_start\":%u,\"body_goes_on_in_the_header's_page\":%u,\"last_completions_res0_flags0\":%u,\"last_completions_other\":%u,\"page_kept_by_one_piece\":%d,\"its_bytes_changed_while_kept\":%d,\"its_bytes_changed_after_return\":%d}\n",
        frames, bad_prefix, hdr_wrong, body_wrong, hdr_one_piece, hdr_at_page_start, body_same_page, ends[0], ends[1] + ends[2] + ends[3], kept_len != 0, page_changed, reused_after);
    return 0;
}

static int run_fullring(int fd, unsigned want) {
    sleep(2); // everything the sender sends is in the socket's queue, and the sender is quiet
    arm(fd, want, 1);
    uint64_t bytes = 0, at_gap = 0; unsigned submit = 1, enters = 0, most = 0, ends = 0; int last = 1, quiet_after = -1; double t0 = now(), last_data = t0, gap = 0, resumed = 0;
    while (now() - t0 < 14 && bytes < want) {
        enter(submit, 0); submit = 0; enters++;
        unsigned head = *cq_head, tail = __atomic_load_n(cq_tail, __ATOMIC_ACQUIRE), n = tail - head;
        if (n > most) most = n;
        for (; head != tail; head++) {
            struct io_uring_cqe *c = (void *)(cqes + (size_t)(head & cq_mask) * 32);
            if (!(c->flags & IORING_CQE_F_MORE)) { ends++; last = c->res; printf("last completion: res %d (%s) flags %#x after %llu bytes\n", c->res, c->res < 0 ? strerror(-c->res) : "-", c->flags, (unsigned long long)bytes); continue; }
            if (now() - last_data > 2 && !resumed) { gap = now() - last_data; resumed = now() - t0; at_gap = bytes; }
            bytes += c->res; give_back(*(uint64_t *)(c + 1), c->res); last_data = now();
        }
        __atomic_store_n(cq_head, head, __ATOMIC_RELEASE);
        if (!n) usleep(1000);
        if (ends && bytes < want && quiet_after < 0) quiet_after = 0;
    }
    printf("{\"cmd\":\"zcfullring\",\"wanted\":%u,\"got\":%llu,\"most_completions_a_turn\":%u,\"enters\":%u,\"last_completions\":%u,\"last_res\":%d,\"secs_since_last_data\":%.2f,\"stalled_at_bytes\":%llu,\"stalled_secs\":%.2f,\"went_on_at_sec\":%.2f}\n",
        want, (unsigned long long)bytes, most, enters, ends, last, now() - last_data, (unsigned long long)at_gap, gap, resumed);
    return 0;
}

static int run_nomem(int fa, int fb) {
    enum { HELD = 1 << 16 }; static struct rqe held[HELD]; unsigned nheld = 0, submit = 2;
    uint64_t a_bytes = 0, b_bytes[2] = {0}; unsigned b_ends[2] = {0}, b_nomem[2] = {0}, b_rearms = 0, a_ends = 0; int a_last = 1;
    arm(fa, 0, 1); arm(fb, 0, 2);
    double t0 = now(); int phase = 0;
    while (now() - t0 < 9) {
        if (!phase && now() - t0 > 4) {
            phase = 1; printf("returning %u pieces of the first connection\n", nheld);
            for (unsigned i = 0; i < nheld; i++) { rqes[rtail++ & rmask] = held[i]; if (!((i + 1) % 2048)) { __atomic_store_n(rq_tail, rtail, __ATOMIC_RELEASE); flush(); } }
            __atomic_store_n(rq_tail, rtail, __ATOMIC_RELEASE); flush(); nheld = 0;
        }
        enter(submit, 0); submit = 0;
        unsigned head = *cq_head, tail = __atomic_load_n(cq_tail, __ATOMIC_ACQUIRE), n = tail - head;
        for (; head != tail; head++) {
            struct io_uring_cqe *c = (void *)(cqes + (size_t)(head & cq_mask) * 32);
            uint64_t off = *(uint64_t *)(c + 1);
            if (!(c->flags & IORING_CQE_F_MORE)) {
                if (c->user_data == 1) { a_ends++; a_last = c->res; if (c->res < 0 && c->res != -ENOMEM) continue; if (c->res < 0) { arm(fa, 0, 1); submit++; } continue; }
                b_ends[phase]++; if (c->res == -ENOMEM) b_nomem[phase]++;
                if (c->res < 0 && b_rearms < 100000) { arm(fb, 0, 2); submit++; b_rearms++; }
                continue;
            }
            if (c->user_data == 1) {
                a_bytes += c->res;
                if (!phase && nheld < HELD) { held[nheld].off = (off & ~AREA_MASK) | a.rq_area_token; held[nheld].len = c->res; held[nheld++].pad = 0; }
                else give_back(off, c->res);
            } else { b_bytes[phase] += c->res; give_back(off, c->res); }
        }
        __atomic_store_n(cq_head, head, __ATOMIC_RELEASE);
        if (!n) usleep(2000);
    }
    printf("{\"cmd\":\"zcnomem\",\"first_bytes\":%llu,\"first_ends\":%u,\"first_last_res\":%d,\"second_bytes_while_held\":%llu,\"second_bytes_after_return\":%llu,\"second_ends_while_held\":%u,\"second_enomem_while_held\":%u,\"second_ends_after_return\":%u,\"second_enomem_after_return\":%u,\"second_armed_again\":%u}\n",
        (unsigned long long)a_bytes, a_ends, a_last, (unsigned long long)b_bytes[0], (unsigned long long)b_bytes[1], b_ends[0], b_nomem[0], b_ends[1], b_nomem[1], b_rearms);
    return 0;
}

// The bytes of one connection, byte i being i % 251: a receive cancelled part-way and
// armed again for the rest goes on at the next byte.
static int run_cancel(int fd, unsigned want) {
    uint64_t pos = 0, wrong = 0, at_cancel = 0; unsigned submit = 1, armed = 1; int cancelled = 0, recv_end = 1, cancel_res = 1, ends = 0; double t0 = now();
    arm(fd, want, 1);
    while (now() - t0 < 10 && pos < want) {
        enter(submit, 0); submit = 0;
        unsigned head = *cq_head, tail = __atomic_load_n(cq_tail, __ATOMIC_ACQUIRE), n = tail - head;
        for (; head != tail; head++) {
            struct io_uring_cqe *c = (void *)(cqes + (size_t)(head & cq_mask) * 32);
            if (c->user_data == 9) { cancel_res = c->res; continue; }
            if (!(c->flags & IORING_CQE_F_MORE)) {
                ends++; recv_end = c->res; armed = 0;
                if (c->res == -ECANCELED && pos < want) { arm(fd, want - (unsigned)pos, 1); submit++; armed = 1; }
                continue;
            }
            uint64_t off = *(uint64_t *)(c + 1); unsigned char *b = (unsigned char *)area + (off & ~AREA_MASK);
            for (int k = 0; k < c->res; k++) wrong += b[k] != (pos + k) % 251;
            pos += c->res; give_back(off, c->res);
        }
        __atomic_store_n(cq_head, head, __ATOMIC_RELEASE);
        if (!cancelled && pos > 0 && !n && now() - t0 > 1.0 && armed) {
            // In the sender's pause: cancel the receive by its tag.
            struct io_uring_sqe *s = &sqes[stail & sq_mask]; memset(s, 0, sizeof *s);
            s->opcode = IORING_OP_ASYNC_CANCEL; s->fd = -1; s->addr = 1; s->user_data = 9;
            sq_arr[stail & sq_mask] = stail & sq_mask; stail++; __atomic_store_n(sq_tail, stail, __ATOMIC_RELEASE);
            submit++; cancelled = 1; at_cancel = pos;
        }
        if (!n) usleep(1000);
    }
    printf("{\"cmd\":\"zccancel\",\"wanted\":%u,\"got\":%llu,\"bytes_wrong\":%llu,\"cancelled_at\":%llu,\"cancel_result\":%d,\"receive_ends\":%d,\"last_receive_result\":%d}\n",
        want, (unsigned long long)pos, (unsigned long long)wrong, (unsigned long long)at_cancel, cancel_res, ends, recv_end);
    return 0;
}

// A flow that does not reach the registered queue is copied by the kernel into pages
// of the area's free list. No piece is returned until a receive has ended for want of
// a page; then all are, and the receive is armed again and again.
static int run_copy(int fd) {
    enum { HELD = 1 << 16 }; static struct rqe held[HELD]; unsigned nheld = 0, submit = 1, rearms = 0, nomem[2] = {0}, other = 0, maxlen = 0, at_start = 0, pieces = 0;
    uint64_t bytes[2] = {0}; int phase = 0, last = 1; double t0 = now(), t_nomem = 0, t_again = 0;
    arm(fd, 0, 1);
    while (now() - t0 < 9) {
        enter(submit, 0); submit = 0;
        unsigned head = *cq_head, tail = __atomic_load_n(cq_tail, __ATOMIC_ACQUIRE), n = tail - head;
        for (; head != tail; head++) {
            struct io_uring_cqe *c = (void *)(cqes + (size_t)(head & cq_mask) * 32);
            uint64_t off = *(uint64_t *)(c + 1);
            if (!(c->flags & IORING_CQE_F_MORE)) {
                last = c->res;
                if (c->res == -ENOMEM) {
                    nomem[phase]++;
                    if (!phase) {
                        phase = 1; t_nomem = now() - t0;
                        for (unsigned i = 0; i < nheld; i++) { rqes[rtail++ & rmask] = held[i]; if (!((i + 1) % 2048)) { __atomic_store_n(rq_tail, rtail, __ATOMIC_RELEASE); flush(); } }
                        __atomic_store_n(rq_tail, rtail, __ATOMIC_RELEASE); flush();
                    }
                } else if (c->res < 0) other++;
                if (c->res < 0 && rearms < 4000) { arm(fd, 0, 1); submit++; rearms++; }
                continue;
            }
            pieces++; if ((unsigned)c->res > maxlen) maxlen = c->res; if (!(off & 4095)) at_start++;
            if (phase && !bytes[1]) t_again = now() - t0;
            bytes[phase] += c->res;
            if (!phase && nheld < HELD) { held[nheld].off = (off & ~AREA_MASK) | a.rq_area_token; held[nheld].len = c->res; held[nheld++].pad = 0; }
            else give_back(off, c->res);
        }
        __atomic_store_n(cq_head, head, __ATOMIC_RELEASE);
        if (!n) usleep(2000);
    }
    printf("{\"cmd\":\"zccopy\",\"bytes_before_no_page\":%llu,\"pieces_held_then\":%u,\"no_page_at_sec\":%.2f,\"bytes_after_return\":%llu,\"first_bytes_again_at_sec\":%.2f,\"enomem_before\":%u,\"enomem_after_return\":%u,\"other_errors\":%u,\"armed_again\":%u,\"last_result\":%d,\"pieces\":%u,\"longest_piece\":%u,\"pieces_at_page_start\":%u}\n",
        (unsigned long long)bytes[0], nheld, t_nomem, (unsigned long long)bytes[1], t_again, nomem[0], nomem[1], other, rearms, last, pieces, maxlen, at_start);
    return 0;
}

int main(int argc, char **argv) {
    if (argc < 6) return 2;
    const char *mode = argv[5]; int nomem = !strcmp(mode, "nomem"), full = !strcmp(mode, "fullring");
    if (!nomem && argc < 7) return 2;
    int cancel = !strcmp(mode, "cancel"), copy = !strcmp(mode, "copy");
    size_t pages = nomem ? 512 : copy ? (size_t)atoi(argv[6]) : (size_t)16384;
    setup(argv[1], atoi(argv[2]), full ? 64 : 32768, pages * 4096);
    int ls = listener(argv[3], atoi(argv[4]));
    int fd = accept(ls, 0, 0); if (fd < 0) die("accept");
    if (nomem) { int fb = accept(ls, 0, 0); if (fb < 0) die("accept"); return run_nomem(fd, fb); }
    if (full) return run_fullring(fd, atoi(argv[6]));
    if (cancel) return run_cancel(fd, atoi(argv[6]));
    if (copy) return run_copy(fd);
    return run_credit(fd, atoi(argv[6]));
}
