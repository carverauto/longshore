# longshore_file

File I/O over the `Io` trait of `longshore_common`, and nothing built on it.

- `Files` is a fixed set of files and operations driven from the caller's
  own loop. It borrows the I/O backend on each call and never waits, so
  sockets (`longshore_tcp`), the S3 client and files share one ring and
  one thread.
- One API on io_uring, the portable `kqueue`/`epoll` backend and the
  deterministic simulator, with its disk faults and power loss.
- Handles that cannot reach a reused slot, operations reported once,
  whole transfers, ordered sequences, buffers lent without copies, direct
  I/O checked the same way on every backend, and one error type.
- No file can take every slot: each open file has an operation of its own.

Logs, pages, records and checksums are left to the program built on it.
