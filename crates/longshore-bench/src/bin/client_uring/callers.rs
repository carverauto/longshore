// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Caller threads that store, fetch, check and erase their own keys at once.

use crate::die::OrDie;
use crate::{CALLERS, ROUNDS, SIZE};
use longshore::client::{Client, Error};
use longshore::mailbox::block_on;

/// The byte at `i` of a caller's object in a round.
fn pattern(caller: usize, round: usize, i: usize) -> u8 {
    (i as u32).wrapping_mul(2654435761).to_le_bytes()[1] ^ (caller * 31 + round) as u8
}

/// Each caller thread stores, fetches, checks and erases its own keys.
/// Even callers block; odd callers await futures.
pub fn run(client: &Client) {
    let callers: Vec<_> = (0..CALLERS)
        .map(|caller| {
            let client: Client = client.clone();
            std::thread::spawn(move || {
                let key = format!("caller-{caller}");
                let len = SIZE as usize;
                for round in 0..ROUNDS {
                    let asynchronous = caller % 2 == 1;
                    let mut body = if asynchronous {
                        block_on(client.buffer())
                    } else {
                        client.buffer_blocking()
                    };
                    for (i, byte) in body.as_mut_slice().iter_mut().enumerate() {
                        *byte = pattern(caller, round, i);
                    }
                    let (stored, body) = if asynchronous {
                        block_on(client.store(&key, body, len))
                    } else {
                        client.store(&key, body, len).wait()
                    };
                    assert_eq!(stored, Ok(()), "caller {caller} round {round}");
                    drop(body);
                    let into = client.buffer_blocking();
                    let (fetched, into) = if asynchronous {
                        block_on(client.fetch(&key, into))
                    } else {
                        client.fetch(&key, into).wait()
                    };
                    assert_eq!(fetched, Ok(Some(len)), "caller {caller} round {round}");
                    let ok = into
                        .as_slice()
                        .iter()
                        .enumerate()
                        .all(|(i, byte)| *byte == pattern(caller, round, i));
                    assert!(ok, "caller {caller} round {round}: bytes differ");
                    let (ranged, into) = client.fetch_range(&key, 100, 199, into).wait();
                    assert_eq!(ranged, Ok(Some(100)));
                    assert!(into.as_slice()[..100]
                        .iter()
                        .enumerate()
                        .all(|(i, byte)| *byte == pattern(caller, round, 100 + i)));
                    drop(into);
                }
                assert_eq!(client.erase(&key).wait(), Ok(()));
                let into = client.buffer_blocking();
                let (gone, into) = client.fetch(&key, into).wait();
                assert_eq!(gone, Ok(None), "caller {caller}: erased object still there");
                // Give it back first: the pool may hold only one buffer per caller.
                drop(into);
                let (no_key, _b): (Result<(), Error>, _) =
                    client.store("", client.buffer_blocking(), 1).wait();
                assert_eq!(no_key, Err(Error::BadKey));
            })
        })
        .collect();
    for c in callers {
        c.join().or_die("a caller thread failed");
    }
}
