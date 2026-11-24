// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The kernel transport's TLS when no TLS provider is compiled in: there
//! is none, and no connection is ever protected.

use crate::config::Config;
use crate::transport::{Happened, Opened, Refused, Role, Span};
use longshore_common::io::{Io, Op, Sock, Token};

/// No TLS.
pub(super) struct Secure;

impl Secure {
    pub(super) const fn new(_conns: u32) -> Secure {
        Secure
    }

    pub(super) fn attach(&mut self, _: &Config, _: impl Fn(u32, u64) -> (Sock, Token)) {}

    pub(super) const fn can(&self, _client: bool) -> bool {
        false
    }

    pub(super) const fn is_on(&self, _slot: u32) -> bool {
        false
    }

    pub(super) const fn start<IO: Io>(
        &mut self,
        _: &mut IO,
        _slot: u32,
        _: Role<'_>,
    ) -> Result<(), Refused> {
        Err(Refused::Unsupported)
    }

    pub(super) const fn open<IO: Io>(
        &mut self,
        _: &mut IO,
        _slot: u32,
        _from: Span,
        _into: Span,
    ) -> Option<Opened> {
        None
    }

    pub(super) const fn recv<IO: Io>(&mut self, _: &mut IO, _slot: u32, _: Span) -> Option<i32> {
        None
    }

    pub(super) const fn send<IO: Io>(&mut self, _: &mut IO, _slot: u32, _: Span) {}

    pub(super) const fn say_goodbye<IO: Io>(&mut self, _: &mut IO, _slot: u32) {}

    pub(super) const fn closing(&mut self, _slot: u32) {}

    pub(super) const fn closed(&mut self, _slot: u32) {}

    pub(super) const fn end_of_stream(&self, _slot: u32, result: i32) -> i32 {
        result
    }

    pub(super) const fn on_io<IO: Io>(
        &mut self,
        _: &mut IO,
        _slot: u32,
        _sent: bool,
        _result: i32,
        _shutdown: (Token, Op),
    ) -> Happened {
        Happened::Nothing
    }
}
