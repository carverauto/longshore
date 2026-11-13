## 1. Core
- [ ] 1.1 Request and completion queues allocated at build time; eventfd wake-up of the I/O thread; simulation and real-kernel tests
- [ ] 1.2 `Client` builder: endpoint, region, credentials, limits, TLS; start-up and orderly shutdown
- [ ] 1.3 Buffer lending with a guard type; waiting when the pool is empty
- [ ] 1.4 Error type and outcome mapping

## 2. Operations
- [ ] 2.1 Async and blocking forms of store, conditional store, fetch, ranged fetch, stat, list and erase on the lending path
- [ ] 2.2 Copying convenience forms that take and return slices
- [ ] 2.3 Streaming upload on multipart with abort on drop
- [ ] 2.4 Streaming download on ranged fetches with checksum verification of each range where the store allows it

## 3. Quality
- [ ] 3.1 Cancellation tests: futures dropped at every stage never free a buffer the kernel still holds
- [ ] 3.2 Tests under at least two async executors and from plain threads
- [ ] 3.3 Overhead benchmark against the low-level API, recorded in `bench/results/`
- [ ] 3.4 Runs against Amazon S3 and Google Cloud Storage

## 4. Packaging
- [ ] 4.1 Compile on non-Linux targets with an unsupported error at build of a client
- [ ] 4.2 Crate documentation with runnable examples, and an `examples/` directory
- [ ] 4.3 Stated minimum Rust version, a feature that turns TLS off, and a note on the C toolchain AWS-LC needs
- [ ] 4.4 README: quick start with the high-level client
