// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! An object storage client for Linux `io_uring`.
//!
//! longshore moves a lot of data with little CPU. One thread drives
//! everything from `io_uring` completions. Buffers, connection slots and
//! operation slots are allocated when the client is built, and object bodies
//! go to the network from the caller's registered buffers without being
//! copied.
//!
//! # Highlights
//!
//! - One handle, [`client::Client`], that any thread can share, with calls
//!   that are futures and can also block.
//! - Over plain HTTP, an upload's body goes from the caller's buffer to the
//!   network card without being copied.
//! - No allocation after the client is built, apart from the TLS handshake.
//! - By default a checksum goes with every upload for the store to verify,
//!   and a download is checked against the checksum the store returns.
//! - No async runtime of its own. It works under any executor and from
//!   plain threads.
//! - Every protocol layer runs unchanged in a deterministic simulator (the
//!   `sim` module, behind the `sim` feature), which is how the retry and
//!   failure paths are tested.
//!
//! # Stores
//!
//! | Store | State |
//! |---|---|
//! | Amazon S3 | Works over HTTP and HTTPS |
//! | S3-compatible stores | Bucket in the host name or in the path. Run against Linode Object Storage and versitygw |
//! | Google Cloud Storage | Works with a bearer token |
//! | Azure Blob Storage | Request signing only. The store cannot use it yet |
//!
//! # Platform
//!
//! Linux 6.1 or later, on x86-64 or AArch64. The crate builds on other
//! systems, and the simulator runs there, but [`client::Builder::build`]
//! returns [`client::BuildError::Unsupported`] because there is no
//! `io_uring` to run on.
//!
//! In a container two things have to be set on the node. The runtime's
//! default seccomp profile refuses the `io_uring` system calls, so the
//! container needs a profile that allows `io_uring_setup`, `io_uring_enter`
//! and `io_uring_register`. And the buffers are locked memory, so
//! `RLIMIT_MEMLOCK` has to be at least the size of the client, where a
//! container commonly gets 8 MiB. The repository's documentation says how
//! each was done on Kubernetes.
//!
//! # Using this crate
//!
//! ## Building a client
//!
//! A [`client::Builder`] holds the settings. Fill in the fields that differ
//! from the defaults and call `build`, which starts the I/O threads.
//!
//! ```no_run
//! use longshore::client::Builder;
//! use longshore::store::creds::Credentials;
//!
//! let credentials = Credentials::new("ACCESS_KEY", "SECRET_KEY").unwrap();
//! let mut settings = Builder::new("my-bucket.s3.us-east-1.amazonaws.com", "us-east-1", credentials);
//! settings.tls = true;
//! settings.port = 443;
//! settings.threads = 4; // four independent I/O threads
//! let client = settings.build().unwrap();
//! ```
//!
//! A store that expects the bucket in the path takes the endpoint as the
//! host and the bucket separately:
//!
//! ```no_run
//! # use longshore::client::Builder;
//! # use longshore::store::creds::Credentials;
//! # let credentials = Credentials::new("ACCESS_KEY", "SECRET_KEY").unwrap();
//! let mut settings = Builder::new("objects.example.com", "us-east-1", credentials);
//! settings.bucket_in_path = Some("my-bucket".into());
//! ```
//!
//! ## Storing and fetching
//!
//! A body lives in a [`client::Buffer`] borrowed from the client. An
//! operation takes the buffer and hands it back with its result, so the
//! same buffer can carry the next request.
//!
//! ```no_run
//! # use longshore::client::Builder;
//! # use longshore::store::creds::Credentials;
//! # let client = Builder::new("b.s3.us-east-1.amazonaws.com", "us-east-1", Credentials::new("K", "S").unwrap()).build().unwrap();
//! let mut body = client.buffer_blocking();
//! body.as_mut_slice()[..5].copy_from_slice(b"hello");
//!
//! let (stored, body) = client.store("greeting", body, 5).wait();
//! stored.unwrap();
//!
//! let (fetched, body) = client.fetch("greeting", body).wait();
//! assert_eq!(fetched.unwrap(), Some(5)); // None would mean no such object
//! assert_eq!(&body.as_slice()[..5], b"hello");
//!
//! client.erase("greeting").wait().unwrap();
//! ```
//!
//! ## Storing only if nothing is there
//!
//! [`client::Client::store_no_clobber`] stores an object only when the key
//! is free. It says whether this call stored it.
//!
//! ```no_run
//! # use longshore::client::Builder;
//! # use longshore::store::creds::Credentials;
//! # let client = Builder::new("b.s3.us-east-1.amazonaws.com", "us-east-1", Credentials::new("K", "S").unwrap()).build().unwrap();
//! # let body = client.buffer_blocking();
//! let (stored, body) = client.store_no_clobber("lock", body, 0).wait();
//! match stored.unwrap() {
//!     true => println!("this call stored it"),
//!     false => println!("an object was already there"),
//! }
//! ```
//!
//! ## Reading part of an object
//!
//! ```no_run
//! # use longshore::client::Builder;
//! # use longshore::store::creds::Credentials;
//! # let client = Builder::new("b.s3.us-east-1.amazonaws.com", "us-east-1", Credentials::new("K", "S").unwrap()).build().unwrap();
//! # let into = client.buffer_blocking();
//! // Bytes 0 to 1023, both ends included.
//! let (fetched, into) = client.fetch_range("video.mp4", 0, 1023, into).wait();
//! let len = fetched.unwrap().expect("no such object");
//! let bytes = &into.as_slice()[..len];
//! ```
//!
//! ## Objects larger than one buffer
//!
//! One operation moves at most one buffer. A larger object goes up as a
//! multipart upload: begin it, store each buffer as a numbered part, and
//! complete it with the parts in order. Parts are separate operations, so
//! several can be in flight at once.
//!
//! ```no_run
//! # use longshore::client::Builder;
//! # use longshore::store::creds::Credentials;
//! # let client = Builder::new("b.s3.us-east-1.amazonaws.com", "us-east-1", Credentials::new("K", "S").unwrap()).build().unwrap();
//! let upload = client.upload_begin("big.bin").wait().unwrap();
//!
//! let mut parts = Vec::new();
//! let mut body = client.buffer_blocking();
//! for number in 1..=3 {
//!     let len = body.len(); // fill the buffer here
//!     let (part, back) = client.upload_part("big.bin", &upload, number, body, len).wait();
//!     parts.push(part.unwrap());
//!     body = back;
//! }
//!
//! // The list of parts is written into the buffer passed here.
//! let (done, _body) = client.upload_complete("big.bin", &upload, &parts, body).wait();
//! done.unwrap();
//! ```
//!
//! An upload that is neither completed nor aborted keeps its parts in the
//! store. [`client::Client::upload_abort`] discards one.
//!
//! ## From async code
//!
//! Every operation returns a [`client::Pending`], which is a `Future`. The
//! same calls work under any executor. A call waits when no buffer or
//! operation slot is free, so nothing queues without bound.
//!
//! ```no_run
//! use longshore::client::{Client, Error};
//!
//! async fn save(client: &Client, key: &str, bytes: &[u8]) -> Result<(), Error> {
//!     let mut body = client.buffer().await;
//!     body.as_mut_slice()[..bytes.len()].copy_from_slice(bytes);
//!     let (stored, _body) = client.store(key, body, bytes.len()).await;
//!     stored
//! }
//! ```
//!
//! ## Knowing exactly what the store answered
//!
//! `store` and `fetch` retry and decide what an answer means. A caller that
//! has to account for every request uses [`client::Client::put`],
//! [`client::Client::head`] and [`client::Client::get`]. They take a
//! [`client::Budget`] of attempts and a deadline, and return a
//! [`store::report::Report`] of what was sent and what the store answered,
//! without turning a status into success or failure.
//!
//! ## Credentials that change
//!
//! Temporary credentials expire. Give the builder a file and each I/O
//! thread rereads it, tries new credentials with one request, and adopts
//! them if the store accepts them.
//!
//! ```no_run
//! # use longshore::client::Builder;
//! # use longshore::store::creds::Credentials;
//! # let credentials = Credentials::new("ACCESS_KEY", "SECRET_KEY").unwrap();
//! let mut settings = Builder::new("my-bucket.s3.us-east-1.amazonaws.com", "us-east-1", credentials);
//! settings.credentials_file = Some("/run/secrets/s3-credentials".into());
//! settings.credentials_reread_secs = 60;
//! ```
//!
//! ## Memory
//!
//! Everything is allocated when the client is built, per I/O thread:
//! [`client::Builder::buffers`] buffers of [`client::Builder::buffer_size`]
//! bytes lent to callers, and what [`client::Builder::connections`]
//! connections need. The defaults are 64 buffers of 8 MiB and 64
//! connections for each thread. `buffer_size` is the largest object one
//! operation moves. The buffers are registered with the kernel, which locks
//! them in memory.
//!
//! # The store underneath
//!
//! [`store`] is the single-threaded API the client is built on. Its caller
//! owns the event loop, gives the store registered buffers, submits each
//! operation with a token and collects outcomes by polling. It has what the
//! client does not expose yet: listing, `stat`, copy, batch erase, object
//! metadata and presigned URLs. [`client::Worker`] is one I/O thread's
//! logic without the thread, for a program that already runs an event loop
//! on each core.
//!
//! # Features
//!
//! - `sim`: the deterministic simulator, for tests.
//! - `fault-injection`: lets a test break a running client's ring. Off
//!   everywhere else.
//!
//! # Crates
//!
//! This crate holds no code. It gathers the crates the client is made of
//! under one name:
//!
//! - [`io`], from `longshore_common`: the [`io::Io`] trait every layer is
//!   written against, and the `io_uring` backend.
//! - [`dns`], [`http`], [`tls`]: the protocols, from `longshore_dns`,
//!   `longshore_http_client` and `longshore_tls`.
//! - [`checksum`], from `longshore_crc`.
//! - [`store`], from `longshore_s3`: the object store API: operations,
//!   signing, retries, listing and credentials.
//! - [`client`], from `longshore_s3`: a handle any thread can share, over
//!   one or more I/O threads that each run a store.
//! - [`crypto`], from `longshore_s3`: the request-signing primitives.
//! - `sim` (feature `sim`), from `longshore_common`: a deterministic
//!   simulator that stands in for the kernel in tests.

pub use longshore_common::io;
#[cfg(feature = "sim")]
pub use longshore_common::sim;
pub use longshore_crc as checksum;
pub use longshore_dns as dns;
pub use longshore_http_client as http;
pub use longshore_s3 as store;
pub use longshore_s3::{client, crypto};
pub use longshore_tls as tls;
