// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

// Throwaway: does this kernel and driver take a zero-copy receive registration?
#define _GNU_SOURCE
#include <errno.h>
#include <net/if.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>
struct area_reg { uint64_t addr, len, rq_area_token; uint32_t flags, dmabuf_fd; uint64_t resv[2]; };
struct region { uint64_t user_addr, size; uint32_t flags, id; uint64_t mmap_offset, resv[4]; };
struct offsets { uint32_t head, tail, rqes, resv2; uint64_t resv[2]; };
struct ifq_reg { uint32_t if_idx, if_rxq, rq_entries, flags; uint64_t area_ptr, region_ptr; struct offsets offsets; uint32_t zcrx_id, rx_buf_len; uint64_t resv[3]; };
int main(int argc, char **argv) {
    const char *ifn = argv[1]; unsigned q = atoi(argv[2]); unsigned buf = argc > 3 ? atoi(argv[3]) : 0;
    char params[120]; memset(params, 0, sizeof params);
    uint32_t *p = (uint32_t *)params; p[1] = 4096; /* cq_entries */
    p[2] = (1u << 12) | (1u << 13) | (1u << 11) | (1u << 3); /* SINGLE_ISSUER, DEFER_TASKRUN, CQE32, CQSIZE */
    int ring = syscall(425, 64, params);
    if (ring < 0) { printf("setup: %s\n", strerror(errno)); return 1; }
    size_t alen = 64u << 20;
    void *area = mmap(0, alen, PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE | (buf > 4096 ? MAP_HUGETLB : 0), -1, 0);
    if (area == MAP_FAILED) { printf("area mmap: %s\n", strerror(errno)); return 1; }
    size_t rlen = 4096 * 16 + 4096;
    void *rq = mmap(0, rlen, PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE, -1, 0);
    struct area_reg a = { .addr = (uint64_t)area, .len = alen };
    struct region r = { .user_addr = (uint64_t)rq, .size = rlen, .flags = 1 };
    struct ifq_reg reg = { .if_idx = if_nametoindex(ifn), .if_rxq = q, .rq_entries = 4096, .area_ptr = (uint64_t)&a, .region_ptr = (uint64_t)&r, .rx_buf_len = buf };
    long rc = syscall(427, ring, 32, &reg, 1);
    if (rc < 0) { printf("register %s queue %u buf %u: REFUSED: %s (%d)\n", ifn, q, buf, strerror(errno), errno); return 2; }
    printf("register %s queue %u buf %u: TAKEN zcrx_id=%u token=%llx offsets head=%u tail=%u rqes=%u\n", ifn, q, buf, reg.zcrx_id, (unsigned long long)a.rq_area_token, reg.offsets.head, reg.offsets.tail, reg.offsets.rqes);
    return 0;
}
