// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

// Throwaway spike: receive TCP streams with io_uring zero-copy receive and count.
//   zcrecv IFACE QUEUE ADDR PORT CONNS [touch]
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
static double now(clockid_t c) { struct timespec t; clock_gettime(c, &t); return t.tv_sec + t.tv_nsec / 1e9; }
static void die(const char *w) { printf("%s: %s\n", w, strerror(errno)); exit(1); }
int main(int argc, char **argv) {
    if (argc < 6) return 2;
    const char *ifn = argv[1]; unsigned q = atoi(argv[2]); int port = atoi(argv[4]); int conns = atoi(argv[5]);
    int touch = argc > 6;
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
    if (syscall(427, ring, 32, &reg, 1) < 0) die("register zcrx");
    unsigned *rq_tail = (unsigned *)(rq + reg.offsets.tail); struct rqe *rqes = (struct rqe *)(rq + reg.offsets.rqes);
    unsigned rtail = 0, rmask = rq_entries - 1;

    int ls = socket(AF_INET, SOCK_STREAM, 0), one = 1;
    setsockopt(ls, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    struct sockaddr_in sa = { .sin_family = AF_INET, .sin_port = htons(port) }; inet_pton(AF_INET, argv[3], &sa.sin_addr);
    if (bind(ls, (void *)&sa, sizeof sa) < 0 || listen(ls, 128) < 0) die("listen");
    fprintf(stderr, "zcrecv: listening\n");
    unsigned stail = *sq_tail;
    for (int i = 0; i < conns; i++) {
        int fd = accept(ls, 0, 0); if (fd < 0) die("accept");
        struct io_uring_sqe *s = &sqes[stail & sq_mask]; memset(s, 0, sizeof *s);
        s->opcode = OP_RECV_ZC; s->fd = fd; s->ioprio = IORING_RECV_MULTISHOT; s->user_data = fd;
        s->file_index = reg.zcrx_id; /* zcrx_ifq_idx shares this union */
        sq_arr[stail & sq_mask] = stail & sq_mask; stail++;
    }
    __atomic_store_n(sq_tail, stail, __ATOMIC_RELEASE);
    unsigned submit = conns; int open = conns;
    uint64_t bytes = 0, cq_n = 0, sum = 0, maxlen = 0, enters = 0; double t0 = 0, c0 = 0;
    while (open > 0) {
        long rc = syscall(426, ring, submit, 1, IORING_ENTER_GETEVENTS, 0, 0);
        if (rc < 0 && errno != EINTR) die("enter");
        submit = 0; enters++;
        unsigned head = *cq_head, tail = __atomic_load_n(cq_tail, __ATOMIC_ACQUIRE);
        for (; head != tail; head++) {
            struct io_uring_cqe *c = (void *)(cqes + (size_t)(head & cq_mask) * 32);
            uint64_t off = *(uint64_t *)(c + 1);
            if (c->res <= 0 && !(c->flags & IORING_CQE_F_MORE)) {
                if (c->res < 0) printf("conn ended: %s\n", strerror(-c->res));
                open--; continue;
            }
            if (c->res <= 0) continue;
            if (!t0) { t0 = now(CLOCK_MONOTONIC); c0 = now(CLOCK_THREAD_CPUTIME_ID); }
            bytes += c->res; cq_n++; if ((uint64_t)c->res > maxlen) maxlen = c->res;
            if (touch) { uint64_t *w = (uint64_t *)(area + (off & ~AREA_MASK)); for (int k = 0; k + 8 <= c->res; k += 8) sum += w[k / 8]; }
            struct rqe *e = &rqes[rtail & rmask];
            e->off = (off & ~AREA_MASK) | a.rq_area_token; e->len = c->res; rtail++;
        }
        __atomic_store_n(rq_tail, rtail, __ATOMIC_RELEASE);
        __atomic_store_n(cq_head, head, __ATOMIC_RELEASE);
    }
    double secs = now(CLOCK_MONOTONIC) - t0, cpu = now(CLOCK_THREAD_CPUTIME_ID) - c0, gb = bytes / 1e9;
    printf("{\"cmd\":\"zcrecv\",\"touch\":%d,\"conns\":%d,\"secs\":%.2f,\"gb_per_s\":%.3f,\"cpu\":%.2f,\"cpu_s_per_gb\":%.3f,\"completions\":%llu,\"mean_len\":%.0f,\"max_len\":%llu,\"enters\":%llu,\"sum\":%llu}\n",
        touch, conns, secs, gb / secs, cpu, cpu / gb, (unsigned long long)cq_n, (double)bytes / cq_n, (unsigned long long)maxlen, (unsigned long long)enters, (unsigned long long)(sum & 0xff));
    return 0;
}
