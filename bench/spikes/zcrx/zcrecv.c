// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

// Throwaway spike: receive TCP streams on one ring and count, with io_uring
// zero-copy receive or with the kernel's copy, reading every byte or none.
//   zcrecv IFACE QUEUE ADDR PORT CONNS zc|copy read|count|stall|log [DIR]
// log: write what arrives to a file a connection with direct I/O, 1 MiB a write:
// zc writes the area's pages as they lie (a piece that is not a whole page at a
// block of the stream ends that for its connection and is counted); copy writes
// the buffer the kernel copied into.
// copy only, a 1 MiB frame with a 256-byte header whose body a log wants at a block:
//   logread   read every byte (a checksum's pass), write the frame as received
//   logshift  read, move the body to a block-aligned buffer, write that (today)
//   logpages  (zc only) write every whole page as it lies, whatever block of the
//             stream it holds: shows that the write is accepted, and what it costs
//   loglead   receive the frame 3,840 bytes into its buffer, so that the body lies
//             at a block; read, write the body from where it lies
// stall (zc only): after a second, return no chunk until nothing arrives for a
// second, then return them all and go on. Shows what an empty area does.
// Built with: gcc -O2 -Wall -o zcrecv zcrecv.c
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <sys/uio.h>
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
static double now(clockid_t c) { struct timespec t; clock_gettime(c, &t); return t.tv_sec + t.tv_nsec / 1e9; }
static void die(const char *w) { printf("%s: %s\n", w, strerror(errno)); exit(1); }
int main(int argc, char **argv) {
    if (argc != 8 && argc != 9) return 2;
    const char *ifn = argv[1]; unsigned q = atoi(argv[2]); int port = atoi(argv[4]); int conns = atoi(argv[5]);
    int zc = !strcmp(argv[6], "zc"), touch = !strcmp(argv[7], "read"), stall = !strcmp(argv[7], "stall"), logm = !strncmp(argv[7], "log", 3);
    int lpages = !strcmp(argv[7], "logpages"), lread = !strcmp(argv[7], "logread"), lshift = !strcmp(argv[7], "logshift"), llead = !strcmp(argv[7], "loglead");
    if (logm && strcmp(argv[7], "log") && !(zc && lpages) && (zc || !(lread || lshift || llead))) return 2;
    enum { HDR = 256, LEAD = 3840 };
    if ((!zc && strcmp(argv[6], "copy")) || (!touch && !stall && !logm && strcmp(argv[7], "count")) || (stall && !zc)) return 2;
    if (conns > 64) return 2;
    struct io_uring_params p; memset(&p, 0, sizeof p);
    p.cq_entries = 32768;
    p.flags = IORING_SETUP_SINGLE_ISSUER | IORING_SETUP_DEFER_TASKRUN | IORING_SETUP_CQE32 | IORING_SETUP_CQSIZE;
    int ring = syscall(425, 256, &p); if (ring < 0) die("setup");
    size_t sqlen = p.sq_off.array + p.sq_entries * 4, cqlen = p.cq_off.cqes + p.cq_entries * 32;
    char *sq = mmap(0, sqlen, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_POPULATE, ring, IORING_OFF_SQ_RING);
    char *cq = mmap(0, cqlen, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_POPULATE, ring, IORING_OFF_CQ_RING);
    struct io_uring_sqe *sqes = mmap(0, p.sq_entries * sizeof *sqes, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_POPULATE, ring, IORING_OFF_SQES);
    if (sq == MAP_FAILED || cq == MAP_FAILED || (void *)sqes == MAP_FAILED) die("ring mmap");
    unsigned *sq_tail = (unsigned *)(sq + p.sq_off.tail), *sq_arr = (unsigned *)(sq + p.sq_off.array), sq_mask = p.sq_entries - 1;
    unsigned *cq_head = (unsigned *)(cq + p.cq_off.head), *cq_tail = (unsigned *)(cq + p.cq_off.tail), cq_mask = p.cq_entries - 1;
    char *cqes = cq + p.cq_off.cqes;

    size_t alen = 256u << 20; unsigned rq_entries = 32768;
    char *area = mmap(0, alen, PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE | MAP_POPULATE, -1, 0);
    size_t rlen = rq_entries * sizeof(struct rqe) + 4096;
    char *rq = mmap(0, rlen, PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE | MAP_POPULATE, -1, 0);
    struct area_reg a = { .addr = (uint64_t)area, .len = alen };
    struct region r = { .user_addr = (uint64_t)rq, .size = rlen, .flags = 1 };
    struct ifq_reg reg = { .if_idx = if_nametoindex(ifn), .if_rxq = q, .rq_entries = rq_entries, .area_ptr = (uint64_t)&a, .region_ptr = (uint64_t)&r };
    if (zc && syscall(427, ring, 32, &reg, 1) < 0) die("register zcrx");
    /* The kernel's copy lands in a buffer a connection, as a program's would. */
    size_t blen = 1u << 20, bstep = blen + 8192; char *bufs = mmap(0, bstep * 64, PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE | MAP_POPULATE, -1, 0);
    if (bufs == MAP_FAILED) die("buffers");
    int fds[64];
    char *shifted = mmap(0, blen, PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE | MAP_POPULATE, -1, 0);
    size_t lead = llead ? LEAD : 0;
    /* log: a file a connection, and the pages (and their returns) of the write being gathered. */
    enum { GATHER = 256 }; int files[64]; uint64_t fpos[64] = {0}, unaligned = 0, written = 0; int ok[64], ngather[64] = {0};
    struct iovec (*iov)[GATHER] = logm ? calloc(64, sizeof *iov) : 0; struct rqe (*back)[8 * GATHER] = logm ? calloc(64, sizeof *back) : 0; int nback[64] = {0}; uint64_t block[64] = {0};
    for (int i = 0; logm && i < conns; i++) { char path[300]; snprintf(path, sizeof path, "%s/zcrecv-log-%d.dat", argc > 8 ? argv[8] : ".", i);
        files[i] = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_DIRECT, 0600); if (files[i] < 0) die("log file"); unlink(path); ok[i] = 1; }
    unsigned *rq_tail = (unsigned *)(rq + reg.offsets.tail); struct rqe *rqes = (struct rqe *)(rq + reg.offsets.rqes);
    unsigned *rq_head = (unsigned *)(rq + reg.offsets.head);
    unsigned rtail = 0, rmask = rq_entries - 1, max_pending = 0;
    /* stall: what was kept back, and where the run is. */
    enum { HELD = 1 << 20 }; struct rqe *held = stall ? calloc(HELD, sizeof *held) : 0; unsigned nheld = 0, given = 0;
    int phase = 0; double last = 0, stalled_at = 0, resumed_at = 0; uint64_t bytes_at_stall = 0; unsigned held_at_stall = 0;
    struct { uint32_t zcrx_id, op; uint64_t resv[2], body[6]; } flush = { .zcrx_id = reg.zcrx_id };

    int ls = socket(AF_INET, SOCK_STREAM, 0), one = 1;
    setsockopt(ls, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    struct sockaddr_in sa = { .sin_family = AF_INET, .sin_port = htons(port) }; inet_pton(AF_INET, argv[3], &sa.sin_addr);
    if (bind(ls, (void *)&sa, sizeof sa) < 0 || listen(ls, 128) < 0) die("listen");
    fprintf(stderr, "zcrecv: listening\n");
    unsigned stail = *sq_tail;
    for (int i = 0; i < conns; i++) {
        int fd = accept(ls, 0, 0); if (fd < 0) die("accept");
        struct io_uring_sqe *s = &sqes[stail & sq_mask]; memset(s, 0, sizeof *s);
        fds[i] = fd; s->fd = fd; s->user_data = i;
        if (zc) { s->opcode = OP_RECV_ZC; s->ioprio = IORING_RECV_MULTISHOT; s->file_index = reg.zcrx_id; /* zcrx_ifq_idx shares this union */ }
        else { s->opcode = IORING_OP_RECV; s->addr = (uint64_t)(bufs + i * bstep + lead); s->len = blen; if (logm) s->msg_flags = MSG_WAITALL; }
        sq_arr[stail & sq_mask] = stail & sq_mask; stail++;
    }
    __atomic_store_n(sq_tail, stail, __ATOMIC_RELEASE);
    unsigned submit = conns; int open = conns;
    uint64_t bytes = 0, cq_n = 0, sum = 0, maxlen = 0, enters = 0; double t0 = 0, c0 = 0;
    /* How the stream lies in the area: a piece that is a whole page, starting where a
       block of the stream starts, could go to a direct-I/O write as it lies. */
    uint64_t pos[64] = {0}, page_start = 0, whole_page = 0, block_bytes = 0;
    while (open > 0) {
        long rc = syscall(426, ring, submit, (phase == 1 || phase == 2) ? 0 : 1, IORING_ENTER_GETEVENTS, 0, 0);
        if (rc < 0 && errno != EINTR) die("enter");
        submit = 0; enters++;
        unsigned head = *cq_head, tail = __atomic_load_n(cq_tail, __ATOMIC_ACQUIRE);
        for (; head != tail; head++) {
            struct io_uring_cqe *c = (void *)(cqes + (size_t)(head & cq_mask) * 32);
            uint64_t off = *(uint64_t *)(c + 1);
            if (!zc && c->res > 0) {
                /* One receive done: count, read, ask for the next. */
                if (!t0) { t0 = now(CLOCK_MONOTONIC); c0 = now(CLOCK_THREAD_CPUTIME_ID); }
                char *b = bufs + c->user_data * bstep + lead;
                bytes += c->res; cq_n++; if ((uint64_t)c->res > maxlen) maxlen = c->res;
                if (touch) { int k = 0; for (; k + 8 <= c->res; k += 8) sum += *(uint64_t *)(b + k); for (; k < c->res; k++) sum += (unsigned char)b[k]; }
                if (logm && (size_t)c->res == blen) {
                    char *from = b;
                    if (lread || lshift || llead) { for (size_t k = 0; k < blen; k += 8) sum += *(uint64_t *)(b + k); }
                    /* The body, as a log wants it: at a block. Its last block is written whole. */
                    if (lshift) { memcpy(shifted, b + HDR, blen - HDR); from = shifted; }
                    if (llead) from = b + HDR;
                    if (pwrite(files[c->user_data], from, blen, fpos[c->user_data]) != (ssize_t)blen) die("write");
                    fpos[c->user_data] += blen; written += blen;
                }
                struct io_uring_sqe *s = &sqes[stail & sq_mask]; memset(s, 0, sizeof *s);
                s->opcode = IORING_OP_RECV; s->fd = fds[c->user_data]; s->user_data = c->user_data; s->addr = (uint64_t)b; s->len = blen; if (logm) s->msg_flags = MSG_WAITALL;
                sq_arr[stail & sq_mask] = stail & sq_mask; stail++; submit++;
                continue;
            }
            if (c->res <= 0 && !(c->flags & IORING_CQE_F_MORE)) {
                if (c->res < 0) printf("conn ended: %s\n", strerror(-c->res));
                open--; continue;
            }
            if (c->res <= 0) continue;
            if (!t0) { t0 = now(CLOCK_MONOTONIC); c0 = now(CLOCK_THREAD_CPUTIME_ID); }
            bytes += c->res; cq_n++; if ((uint64_t)c->res > maxlen) maxlen = c->res;
            { uint64_t in = off & 4095, *at = &pos[c->user_data & 63]; if (!in) page_start++; if (!in && c->res == 4096) whole_page++; if (!in && c->res == 4096 && !(*at & 4095)) block_bytes += 4096; *at += c->res; }
            if (touch) { char *b = area + (off & ~AREA_MASK); int k = 0; for (; k + 8 <= c->res; k += 8) sum += *(uint64_t *)(b + k); for (; k < c->res; k++) sum += (unsigned char)b[k]; }
            if (logm) {
                /* A block of the stream may come as several pieces of one page (the kernel
                   counts a reference a piece): it is whole when they have filled the page. */
                int i = c->user_data & 63; uint64_t in = off & 4095, base = (off & ~AREA_MASK) & ~4095ull, before = pos[i] - c->res;
                if (ok[i] && ((lpages && in == 0 && c->res == 4096 && (pos[i] = 4096)) || (!lpages && (before & 4095) == in && (in == 0 || base == block[i]))) && nback[i] < 8 * GATHER) {
                    block[i] = base;
                    back[i][nback[i]].off = (off & ~AREA_MASK) | a.rq_area_token; back[i][nback[i]++].len = c->res;
                    if (!(pos[i] & 4095)) { int n = ngather[i]++; iov[i][n].iov_base = area + base; iov[i][n].iov_len = 4096; }
                    if (ngather[i] == GATHER) {
                        if (pwritev(files[i], iov[i], GATHER, fpos[i]) != GATHER * 4096) die("writev");
                        fpos[i] += GATHER * 4096; written += GATHER * 4096; ngather[i] = 0;
                        for (int k = 0; k < nback[i]; k++) rqes[rtail++ & rmask] = back[i][k];
                        nback[i] = 0;
                    }
                    continue;
                }
                if (!lpages) ok[i] = 0;
                unaligned += c->res;
            }
            last = now(CLOCK_MONOTONIC);
            if (phase == 0 && stall && last - t0 > 1.0) phase = 1;
            if (phase == 3 && !resumed_at) resumed_at = last;
            if (phase == 1 && nheld < HELD) { held[nheld].off = (off & ~AREA_MASK) | a.rq_area_token; held[nheld++].len = c->res; continue; }
            struct rqe *e = &rqes[rtail & rmask];
            e->off = (off & ~AREA_MASK) | a.rq_area_token; e->len = c->res; rtail++;
        }
        if (phase == 1 && now(CLOCK_MONOTONIC) - last > 1.0) {
            phase = 2; stalled_at = last; bytes_at_stall = bytes; held_at_stall = nheld;
        }
        if (phase == 2) {
            /* No more in the ring than it has entries; the kernel is asked to take them. */
            while (given < nheld && rtail - __atomic_load_n(rq_head, __ATOMIC_ACQUIRE) < rq_entries) rqes[rtail++ & rmask] = held[given++];
            __atomic_store_n(rq_tail, rtail, __ATOMIC_RELEASE);
            if (syscall(427, ring, 36, &flush, 0) < 0) die("flush");
            if (given == nheld) phase = 3;
        }
        if (zc) { unsigned pending = rtail - __atomic_load_n(rq_head, __ATOMIC_ACQUIRE); if (pending > max_pending) max_pending = pending; }
        if (zc) __atomic_store_n(rq_tail, rtail, __ATOMIC_RELEASE);
        __atomic_store_n(sq_tail, stail, __ATOMIC_RELEASE);
        __atomic_store_n(cq_head, head, __ATOMIC_RELEASE);
    }
    double secs = now(CLOCK_MONOTONIC) - t0, cpu = now(CLOCK_THREAD_CPUTIME_ID) - c0, gb = bytes / 1e9;
    printf("{\"cmd\":\"zcrecv\",\"mode\":\"%s\",\"read\":%d,\"conns\":%d,\"secs\":%.2f,\"gb_per_s\":%.3f,\"cpu\":%.2f,\"cpu_s_per_gb\":%.3f,\"completions\":%llu,\"mean_len\":%.0f,\"max_len\":%llu,\"enters\":%llu,\"pieces_at_page_start\":%.4f,\"pieces_whole_page\":%.4f,\"bytes_in_whole_blocks\":%.4f,\"max_pending_returns\":%u,\"sum\":%llu}\n",
        argv[6], touch, conns, secs, gb / secs, cpu, cpu / gb, (unsigned long long)cq_n, (double)bytes / cq_n, (unsigned long long)maxlen, (unsigned long long)enters, cq_n ? (double)page_start / cq_n : 0, cq_n ? (double)whole_page / cq_n : 0, bytes ? (double)block_bytes / bytes : 0, max_pending, (unsigned long long)(sum & 0xff));
    if (logm) printf("{\"cmd\":\"zclog\",\"mode\":\"%s\",\"written_gb\":%.3f,\"not_written_as_it_lay_gb\":%.3f}\n", argv[6], written / 1e9, unaligned / 1e9);
    if (stall) printf("{\"cmd\":\"zcstall\",\"area_chunks\":%zu,\"held_completions\":%u,\"bytes_before_stall\":%llu,\"secs_to_resume\":%.3f,\"bytes_total\":%llu,\"phase\":%d}\n",
        alen / 4096, held_at_stall, (unsigned long long)bytes_at_stall, resumed_at - stalled_at - 1.0 > 0 ? resumed_at - stalled_at - 1.0 : 0.0, (unsigned long long)bytes, phase);
    return 0;
}
