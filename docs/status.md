# Status

| Area | State |
|---|---|
| S3: `store`, `store_no_clobber`, `fetch`, ranged `fetch`, `list`, `list_continue`, `erase` | Works against Amazon S3 and Linode Object Storage, over HTTP and HTTPS |
| Multipart upload: begin, store part, complete, abort | Works against Amazon S3, Linode Object Storage, Google Cloud Storage and versitygw. Each part is one registered buffer |
| `stat`, stores with a content type and metadata, `copy`, `erase_many`, listing with a delimiter, presigned URLs | Work against Amazon S3, Linode Object Storage and Google Cloud Storage. `erase_many` and presigned URLs need signing keys, so they are not available with Google's bearer token |
| Google Cloud Storage | Works against a real bucket, with a bearer token |
| Azure Blob Storage | Request signing only, checked against the Azurite emulator. The store cannot use it yet |
| Bucket addressing | The bucket goes in the host name by default, or in the path when configured, as self-hosted S3-compatible stores commonly expect. Both forms run against Linode Object Storage. The path form also runs against Ceph 20.2.5 and RustFS 1.0.6, which accept nothing else (`bench/results/m27-self-hosted-stores.md`). MinIO is not tested: it no longer issues releases |
| Shareable client | `longshore::client`: a handle usable from any thread over one or more independent I/O threads, with async and blocking calls and lent buffers. Store, conditional store, fetch, ranged fetch, erase and multipart upload. Run on Linux 6.8 and 6.14 against the in-tree server, and measured against Amazon S3 |
| `put`, `head`, `get`: one request within a deadline, reported as the store answered it | In the shareable client and the low-level API. Tested in the simulator and on real rings against the in-tree server, and run three times against Linode Object Storage over HTTPS, from AArch64 and x86-64 (`m24-put-head-get-linode.md`). Not yet run against Amazon S3 |
| IPv4 and IPv6 | The resolver asks for A records, AAAA records or both, and prefers the family the configuration names. When connections to that family fail and the name has addresses in the other, it uses the other. Run over IPv6 against Linode Object Storage |
| Large-segment addresses | Optional, off by default. A store's name can resolve to addresses whose connections get different TCP segment sizes, and on a network card that does not segment in hardware the small ones cost several times more CPU per byte. With the setting on, the client measures each new connection, keeps its connections on the addresses with large segments, and asks DNS again sooner when every address is small. It can be switched while the store runs. Works on Linux 6.1 and later |
| TLS 1.3 | `rustls` does the handshake. After that, records are sealed and opened in this repository with AES-GCM from AWS-LC |
| Checksums | CRC-64/NVME for S3, CRC-32C for Google. A fetch is verified when the store returns a checksum, and both stores do |
| Credentials | Long-lived keys, temporary keys with a session token, or a bearer token. Read from a file and replaced when the file changes, once the store has accepted the new ones unless that check is turned off |
