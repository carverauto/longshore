## 1. The ring sends everything
- [ ] 1.1 `IORING_SETUP_SUBMIT_ALL` at setup; `tick` checks the count the kernel consumed
- [ ] 1.2 Real-kernel check: a submission the kernel refuses to prepare does not hold back those queued behind it

## 2. Pools and registration
- [ ] 2.1 `Limits`: file slots, aligned buffers, their size and the block size; the third pool class
- [ ] 2.2 The pool one block larger and used from a block boundary; the aligned class on a block boundary; the tiling property test and the layout tests updated for the padding
- [ ] 2.3 Registration by ranges of whole buffers up to 1 GiB; registered-buffer operations name range and address
- [ ] 2.4 `getrlimit` declared; the build errors for refused locked memory and for a file table over the open-files limit, each with its numbers
- [ ] 2.5 Real-kernel checks: more than 16,384 small buffers; locked memory refused in an unprivileged child with a lowered limit
- [ ] 2.6 S3 figures before and after on `io_uring`, recorded in `bench/results/`

## 3. Slots and the shared check
- [ ] 3.1 `FileSlot`; `io::files` with the slot table and the check; abstract open flags mapped per architecture
- [ ] 3.2 The ready list in the ring backend: drained in `tick`, no wait in the kernel while it holds anything, cleared when the backend stops
- [ ] 3.3 The engine calls the same check and completes refusals the same way
- [ ] 3.4 The registered file table sized for sockets and file slots

## 4. Operations
- [ ] 4.1 `Op` variants: open, close, read, write, sync, reserve (keep or extend), punch, status by path, rename, unlink, make-directory; the status decoder
- [ ] 4.2 Ring backend: each operation, files opened into the table without close-on-exec, error mapping
- [ ] 4.3 POSIX world: the same operations by blocking calls, on macOS and Linux, with the macOS forms of direct I/O, sync, reserve and punch
- [ ] 4.4 Virtual world: the disk setup call with fixed capacities, files in memory with size and holes, a name table, the existing delay fault
- [ ] 4.5 Shared file tests generic over `Io`, run on all three, including every refusal of the check

## 5. Chains
- [ ] 5.1 The `Io` call that takes a chain or refuses it whole; the three implementations of `Io`, the test wrapper in `longshore_s3` among them
- [ ] 5.2 The check over a whole chain, with the all-cancelled outcome for a refused member
- [ ] 5.3 Ring: linked submissions with the last unlinked, one slot and one completion per member
- [ ] 5.4 Engine: members one at a time, ending at a failure or a short count, for both worlds
- [ ] 5.5 Shared tests: write then sync; a short write; a failure at each position; a refused member at each position; `Busy`; a chain beside other submissions; a cancel of a member not yet started

## 6. Real kernels
- [ ] 6.1 `file_uring` check binary, sealed against allocation after setup, on a file system that supports direct I/O, added to `scripts/test-linux.sh` and CI
- [ ] 6.2 Run on Linux 6.1 and on a current kernel
- [ ] 6.3 Run on a real disk with direct I/O and sync; the latency of write-then-sync as a chain against two submissions, recorded in `bench/results/`

## 7. Project
- [ ] 7.1 README: file operations, the aligned pool and choosing its block size, the locked-memory and open-files requirements, what differs on macOS, and that sync runs on kernel workers on Linux 6.1
