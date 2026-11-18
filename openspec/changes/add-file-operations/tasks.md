## 1. Pools and registration
- [ ] 1.1 `Limits`: file slots, aligned buffers, their size and block size; layout of the third pool
- [ ] 1.2 Aligned allocation; registration by ranges of at most 1 GiB; fixed-buffer operations name range and address
- [ ] 1.3 Locked-memory check when the ring backend is built, with both sizes in the error
- [ ] 1.4 S3 figures before and after on `io_uring`, recorded in `bench/results/`

## 2. Operations
- [ ] 2.1 `Op` variants: open, close, read, write, sync, reserve, punch, status, rename, unlink, make-directory; the status decoder
- [ ] 2.2 Engine: the direct I/O check before any world or the ring sees the operation
- [ ] 2.3 Ring backend: files in the registered table, each operation, error mapping
- [ ] 2.4 POSIX world: the same operations by blocking calls, on macOS and Linux
- [ ] 2.5 Virtual world: files in memory with size and holes, seeded delay
- [ ] 2.6 Shared file tests generic over `Io`, run on all three

## 3. Chains
- [ ] 3.1 `Io` call that takes a chain or refuses it whole
- [ ] 3.2 Ring: linked submissions, one completion each, cancellation of the rest on failure
- [ ] 3.3 Engine: a chain's operations one at a time, for both worlds
- [ ] 3.4 Shared tests: write then sync; a failure at each position; `Busy`

## 4. Real kernels
- [ ] 4.1 `file_uring` check binary, sealed against allocation after setup, added to `scripts/test-linux.sh` and CI
- [ ] 4.2 Run on Linux 6.1 and on a current kernel
- [ ] 4.3 Run on a real disk with direct I/O and sync; latency of write-then-sync as a chain against two submissions, recorded in `bench/results/`

## 5. Project
- [ ] 5.1 README: file operations, the aligned pool, the locked-memory requirement
