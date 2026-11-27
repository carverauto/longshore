# Copyright 2026 Carver Automation Corporation.
#
# Licensed under the Apache License, Version 2.0 (the "License").
# SPDX-License-Identifier: Apache-2.0

# Throwaway: send N frames of LEN bytes, each starting with LEN-4 big-endian,
# with or without MSG_EOR on each frame's send.
import socket, struct, sys, time
addr, port, length, eor = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), sys.argv[4] == "on"
s = socket.create_connection((addr, port)); s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
frame = struct.pack(">I", length - 4) + bytes(length - 4)
flags = socket.MSG_EOR if eor else 0
t = time.time(); n = 0
try:
    while n < 4100:
        s.sendall(frame, flags); n += 1
except OSError:
    pass
dt = time.time() - t
print(f"sent {n} frames in {dt:.2f}s, {n*length/dt/1e9:.2f} GB/s")
