# longshore_dns

[![crates.io](https://img.shields.io/crates/v/longshore_dns.svg)](https://crates.io/crates/longshore_dns)
[![docs.rs](https://img.shields.io/docsrs/longshore_dns)](https://docs.rs/longshore_dns)
[![License](https://img.shields.io/crates/l/longshore_dns.svg)](https://github.com/carverauto/longshore/blob/main/LICENSE)

A DNS resolver for a fixed set of host names, driven by longshore I/O
completions. A lookup reads an in-memory cache and does no I/O, so a
request never waits on DNS.

The resolver resolves every configured name at start-up, for A records,
AAAA records or both. It refreshes them on an interval it chooses itself
and ignores record TTLs: the client sets the pace, not the servers.

## What it gives

- `codec` encodes queries and parses responses. It does no I/O and never
  reads outside the bytes it is given.
- `Resolver` drives the codec over the `Io` trait of `longshore_common`,
  on the kernel or in the simulator.
- `Config` names the hosts, the nameservers (one or two), the family, the
  refresh interval and the answer timeout. `Resolver::new` checks it and
  says what is wrong in a `ConfigError`.
- `Family` chooses which records the resolver asks for and which family a
  lookup hands out first.

## How it is driven

The resolver lives in the caller's loop, like every longshore protocol:

```text
Resolver::new(config)              once: check the settings, build the cache
resolver.start(&mut io)            once: submit the first queries
resolver.on_completion(&mut io, c) for each completion: false means "not mine"
resolver.ready()                   true once every host has an address
resolver.lookup(host)              the host's next address, in turn, from the cache
```

`host_index` turns a name into the `host` that `lookup` takes.
`connect_failed` reports an address that refused a connection: lookups
then move to the other family, if it has addresses. `connected` reports
the segment sizes a connection got, which the preference for
large-segment addresses uses.

## Part of longshore

longshore gathers this crate and ten others under one name, and re-exports
it as `longshore::dns`. Most programs depend on
[longshore](https://crates.io/crates/longshore) instead, whose client runs
this resolver for the store's host. The crates are released together:
depend on all of them at the same version.

- API documentation: <https://docs.rs/longshore_dns>
- Repository: <https://github.com/carverauto/longshore>

## License

Apache License 2.0. See [`LICENSE`](https://github.com/carverauto/longshore/blob/main/LICENSE).
