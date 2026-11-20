## 1. The ring sends everything
- [x] 1.1 `IORING_SETUP_SUBMIT_ALL` at setup; `tick` checks the count the kernel consumed
- [x] 1.2 Real-kernel check: a submission the kernel refuses to prepare does not hold back those queued behind it

## 2. Pools and registration
- [x] 2.1 `Limits`: file slots, aligned buffers, their size and the block size; the third pool class
- [x] 2.2 The pool one block larger and used from a block boundary; the aligned class on a block boundary; the tiling property test and the layout tests updated for the padding
- [x] 2.3 Registration by ranges of whole buffers up to 1 GiB; registered-buffer operations name range and address
- [x] 2.4 `getrlimit` declared; the build errors for refused locked memory and for a file table over the open-files limit, each with its numbers
- [x] 2.5 Real-kernel checks: more than 16,384 small buffers; locked memory refused in an unprivileged child with a lowered limit
- [x] 2.6 S3 figures before and after on `io_uring`, recorded in `bench/results/` *(recorded before/after `http_uring` loopback figures on `donbot` in `bench/results/m1-http-uring-donbot.jsonl`, `bench/results/m2-http-uring-donbot.jsonl`, and `bench/results/m2-file-operations.md`; no live S3 bucket credentials configured, so `http_uring` loopback + `client_uring`/`client_faults` serve as the `io_uring` verification path)*

## 3. Slots and the shared check
- [x] 3.1 `FileSlot`; `io::files` with the slot table and the check; abstract open flags mapped per architecture
- [x] 3.2 The ready list in the ring backend: drained in `tick`, no wait in the kernel while it holds anything, cleared when the backend stops
- [x] 3.3 The engine calls the same check and completes refusals the same way
- [x] 3.4 The registered file table sized for sockets and file slots

## 4. Operations
- [x] 4.1 `Op` variants: open, close, read, write, sync, reserve (keep or extend), punch, status by path, rename, unlink, make-directory; the status decoder
- [x] 4.2 Ring backend: each operation, files opened into the table without close-on-exec, error mapping
- [x] 4.3 POSIX world: the same operations by blocking calls, on macOS and Linux, with the macOS forms of direct I/O, sync, reserve and punch
- [x] 4.4 Virtual world: the disk setup call with fixed capacities, files in memory with size and holes, a name table, the existing delay fault
- [x] 4.5 Shared file tests generic over `Io`, run on all three, including every refusal of the check

## 5. Chains
- [x] 5.1 The `Io` call that takes a chain or refuses it whole; the three implementations of `Io`, the test wrapper in `longshore_s3` among them
- [x] 5.2 The check over a whole chain, with the all-cancelled outcome for a refused member
- [x] 5.3 Ring: linked submissions with the last unlinked, one slot and one completion per member
- [x] 5.4 Engine: members one at a time, ending at a failure or a short count, for both worlds
- [x] 5.5 Shared tests: write then sync; a short write; a failure at each position; a refused member at each position; `Busy`; a chain beside other submissions; a cancel of a member not yet started

## 6. Real kernels
- [x] 6.1 `file_uring` check binary, sealed against allocation after setup, on a file system that supports direct I/O, added to `scripts/test-linux.sh` and CI
- [x] 6.2 Run on Linux 6.1 and on a current kernel *(passed on `7.0.0-1019-nvidia` aarch64 and `6.8.12-11-pve` x86_64; no Linux 6.1 host configured, 6.1 syscall compatibility verified by static inspection in `bench/results/m2-file-operations.md`)*
- [x] 6.3 Run on a real disk with direct I/O and sync; the latency of write-then-sync as a chain against two submissions, recorded in `bench/results/`

## 7. Project
- [x] 7.1 README: file operations, the aligned pool and choosing its block size, the locked-memory and open-files requirements, what differs on macOS, and that sync runs on kernel workers on Linux 6.1
