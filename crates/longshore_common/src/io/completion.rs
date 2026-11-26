// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What a finished operation reports ([`Completion`]), and the fixed buffer
//! [`super::Io::tick`] delivers them into ([`Completions`]).

use super::{BufId, Token};

/// `result` is a byte count or 0 on success, or a negated Linux errno.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Completion {
    pub(crate) token: Token,
    pub(crate) result: i32,
    pub(crate) more: bool,
    pub(crate) buf: Option<BufId>,
    pub(crate) off: u32,
}

impl Completion {
    /// A single final completion carrying no selected buffer.
    #[inline]
    pub const fn new(token: Token, result: i32) -> Self {
        Self {
            token,
            result,
            more: false,
            buf: None,
            off: 0,
        }
    }

    /// This completion, saying whether more follow for its token.
    #[inline]
    pub const fn with_more(mut self, more: bool) -> Self {
        self.more = more;
        self
    }

    /// This completion, carrying the buffer an armed receive selected.
    #[inline]
    pub const fn with_buf(mut self, buf: Option<BufId>) -> Self {
        self.buf = buf;
        self
    }

    /// The token the operation was submitted with.
    #[inline]
    pub const fn token(self) -> Token {
        self.token
    }

    /// A byte count or 0 on success, or a negated Linux errno.
    #[inline]
    pub const fn result(self) -> i32 {
        self.result
    }

    /// Whether more completions will follow for [`Completion::token`].
    #[inline]
    pub const fn more(self) -> bool {
        self.more
    }

    /// Buffer selected for this completion by an armed receive, if any.
    #[inline]
    pub const fn buf(self) -> Option<BufId> {
        self.buf
    }

    /// This completion, carrying a piece of a zero-copy receive: `result`
    /// bytes of page `page` of the receive area, from `off`.
    #[inline]
    pub const fn with_piece(mut self, page: BufId, off: u32) -> Self {
        (self.buf, self.off) = (Some(page), off);
        self
    }

    /// Where in [`Completion::buf`] the bytes of this completion start:
    /// the offset of a piece in its page for an [`Op::ArmedRecvZc`],
    /// 0 for every other operation.
    ///
    /// [`Op::ArmedRecvZc`]: super::Op::ArmedRecvZc
    #[inline]
    pub const fn off(self) -> u32 {
        self.off
    }
}

/// Fixed-capacity completion buffer; `tick` never delivers more than fit.
pub struct Completions {
    items: Box<[Completion]>,
    len: usize,
}

impl Completions {
    /// An empty buffer for `capacity` completions. Allocates it, here and never
    /// again.
    pub fn new(capacity: usize) -> Completions {
        Completions {
            items: vec![Completion::new(Token(0), 0); capacity].into_boxed_slice(),
            len: 0,
        }
    }
    /// Whether no more completions fit.
    pub fn is_full(&self) -> bool {
        self.len == self.items.len()
    }
    /// Whether it holds no completions.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// How many completions it holds.
    pub fn len(&self) -> usize {
        self.len
    }
    #[cfg(any(test, feature = "sim"))]
    pub(crate) fn clear(&mut self) {
        self.len = 0;
    }
    /// Caller must check `is_full` first.
    pub fn push(&mut self, c: Completion) {
        self.items[self.len] = c;
        self.len += 1;
    }
    /// Remove every completion `take` accepts and hand it to `f`, in arrival
    /// order; the others stay, in their order. One pass, no allocation.
    pub fn drain_where(
        &mut self,
        take: impl Fn(&Completion) -> bool,
        mut f: impl FnMut(Completion),
    ) {
        let mut kept = 0;
        for i in 0..self.len {
            let c = self.items[i];
            if take(&c) {
                f(c);
            } else {
                self.items[kept] = c;
                kept += 1;
            }
        }
        self.len = kept;
    }
    /// Take the completion that arrived first.
    pub fn pop(&mut self) -> Option<Completion> {
        if self.len == 0 {
            return None;
        }
        // Deliver in arrival order.
        let c = self.items[0];
        self.items.copy_within(1..self.len, 0);
        self.len -= 1;
        Some(c)
    }
}
