// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Handles for connections and listeners.
//!
//! A handle is a slot and the slot's generation when the handle was made.
//! The generation rises each time the slot is freed, so a handle kept after
//! its connection closed is refused instead of reaching whatever connection
//! holds the slot next.

/// A connection of a `Net`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Conn {
    slot: u32,
    generation: u32,
}

impl Conn {
    /// The connection's slot, for a caller that keeps per-connection state
    /// in a table of its own. Slots are reused; handles are not.
    #[inline]
    pub const fn slot(self) -> u32 {
        self.slot
    }
}

/// A listener of a `Net`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Listener {
    slot: u32,
    generation: u32,
}

impl Listener {
    /// The listener's slot.
    #[inline]
    pub const fn slot(self) -> u32 {
        self.slot
    }
}

/// The current generation of each slot of a table.
pub struct Generations {
    generations: Box<[u32]>,
}

impl Generations {
    /// `slots` slots at generation 0. Allocates once, here.
    pub fn new(slots: u32) -> Generations {
        Generations {
            generations: vec![0; slots as usize].into_boxed_slice(),
        }
    }

    /// A handle for the connection now in `slot`.
    #[inline]
    pub fn conn(&self, slot: u32) -> Conn {
        Conn {
            slot,
            generation: self.generations[slot as usize],
        }
    }

    /// A handle for the listener now in `slot`.
    #[inline]
    pub fn listener(&self, slot: u32) -> Listener {
        Listener {
            slot,
            generation: self.generations[slot as usize],
        }
    }

    /// The slot of `conn`, if `conn` still names what is in it.
    #[inline]
    pub fn slot_of(&self, conn: Conn) -> Option<u32> {
        (self.generations.get(conn.slot as usize) == Some(&conn.generation)).then_some(conn.slot)
    }

    /// The slot of `listener`, if it still names what is in it.
    #[inline]
    pub fn slot_of_listener(&self, listener: Listener) -> Option<u32> {
        (self.generations.get(listener.slot as usize) == Some(&listener.generation))
            .then_some(listener.slot)
    }

    /// The slot was freed: every handle made for it so far is stale.
    #[inline]
    pub fn retire(&mut self, slot: u32) {
        let g = &mut self.generations[slot as usize];
        *g = g.wrapping_add(1);
    }
}
