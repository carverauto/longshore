## 1. Measuring
- [x] 1.1 `Op::Connect` result carries the segment size and the path's limit; a setting on the `Io` trait
- [x] 1.2 Backend: descriptor route, read `TCP_MAXSEG`, release the descriptor on every path
- [x] 1.3 Simulator: a segment size per listening address
- [x] 1.4 Real-kernel self-check: a loopback connect reports loopback's segment size, and a failed connect leaks no descriptor

## 2. Choosing
- [x] 2.1 HTTP client reports address and size of each opened connection
- [x] 2.2 Resolver: size per address, the choice in lookups, the minimum number of addresses, sizes across a refresh, early refresh when every address is small
- [x] 2.4 Switching the setting while the store runs
- [x] 2.3 Simulation tests: mixed sizes converge on the large-segment addresses; all alike changes nothing; setting off changes nothing

## 3. Real stores
- [x] 3.1 Run on Linux 6.1 and on a current kernel (6.1, 6.8, 6.14 and 7.0)
- [ ] 3.2 Five runs per cell against Amazon S3 from both instance types, with the setting on and off
- [ ] 3.3 Sweep the TLS send batch size on both instance types and set the default from it
- [ ] 3.4 README: measurement tables, the setting, and its trade-off
