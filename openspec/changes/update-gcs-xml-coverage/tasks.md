## 1. High-level client
- [ ] 1.1 Vendor in the client's configuration, passed to each store it builds
- [ ] 1.2 Batch erase on Google Cloud Storage as single erases, with the same report of failures
- [ ] 1.3 Simulation tests: store, fetch, list, multipart and batch erase through the client against the Google form of the test server

## 2. Reported operations
- [ ] 2.1 Test server: object generations, `x-goog-generation`, the `generation` query parameter
- [ ] 2.2 Reported put, head and get for `Vendor::Gcs`
- [ ] 2.3 Simulation tests mirroring the S3 ones, lost answers included

## 3. Bearer token from the metadata server
- [ ] 3.1 Request, JSON field reader into a fixed buffer, refresh ahead of expiry
- [ ] 3.2 Simulation tests: first fetch, refresh, a server that fails, a token that is refused
- [ ] 3.3 Property and fuzz tests for the reader

## 4. Zonal buckets
- [ ] 4.1 Run against a real zonal bucket through the XML API: store, conditional store, fetch, ranged fetch, head, listing, erase, copy, a wrong checksum; record in `bench/results/`
- [ ] 4.2 `zonal` in the store's configuration; multipart refused when submitted
- [ ] 4.3 Whatever 4.1 shows to be refused by the store is refused when submitted, the same way

## 5. Real stores and documentation
- [ ] 5.1 The client's driver against a standard Google Cloud Storage bucket (closes `add-high-level-client` 3.4)
- [ ] 5.2 README: what works on which store, and what a zonal bucket cannot do
