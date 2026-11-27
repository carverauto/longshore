// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The card's header-and-data-split setting, read over ethtool netlink.
//!
//! The kernel refuses a receive area on a queue that does not split
//! headers from payload with the same `EINVAL` as several other faults. A
//! refused registration asks the card here, so that the refusal names the
//! cause for certain. It is asked only after a refusal: a registration that
//! succeeds costs nothing, and a question that cannot be asked never refuses
//! a backend that would have worked. Values are from `ethtool_netlink.h`,
//! `genetlink.h` and `netlink.h`.

use crate::io::sys::{close, socket};
use core::ffi::{c_int, c_void};

const AF_NETLINK: c_int = 16;
const SOCK_RAW: c_int = 3;
const SOCK_CLOEXEC: c_int = 0o2_000_000;
const NETLINK_GENERIC: c_int = 16;
const MSG_DONTWAIT: c_int = 0x40;
const NLM_F_REQUEST: u16 = 0x01;
const NLMSG_ERROR: u16 = 0x02;
const NLA_F_NESTED: u16 = 1 << 15;
/// `NLMSG_MIN_TYPE`: the generic netlink controller.
const GENL_ID_CTRL: u16 = 0x10;
const CTRL_CMD_GETFAMILY: u8 = 3;
const CTRL_ATTR_FAMILY_ID: u16 = 1;
const CTRL_ATTR_FAMILY_NAME: u16 = 2;
const ETHTOOL_MSG_RINGS_GET: u8 = 15;
const ETHTOOL_GENL_VERSION: u8 = 1;
const ETHTOOL_A_RINGS_HEADER: u16 = 1;
const ETHTOOL_A_RINGS_TCP_DATA_SPLIT: u16 = 11;
const ETHTOOL_A_HEADER_DEV_INDEX: u16 = 1;
/// A netlink header and a generic netlink header.
const HEADS: usize = 16 + 4;

extern "C" {
    fn send(fd: c_int, buf: *const c_void, len: usize, flags: c_int) -> isize;
    fn recv(fd: c_int, buf: *mut c_void, len: usize, flags: c_int) -> isize;
}

/// What a card says of its split.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Split {
    Off,
    On,
    /// The driver does not say.
    Unknown,
}

/// The split of interface `if_idx`, or `None` where it could not be asked.
pub(super) fn split_of(if_idx: u32) -> Option<Split> {
    let fd = unsafe { socket(AF_NETLINK, SOCK_RAW | SOCK_CLOEXEC, NETLINK_GENERIC) };
    if fd < 0 {
        return None;
    }
    let found = ask_split(fd, if_idx);
    let _: i32 = unsafe { close(fd) };
    found
}

fn ask_split(fd: c_int, if_idx: u32) -> Option<Split> {
    let mut reply = [0u8; 4096];
    let mut name = [0u8; 12];
    let name_len = put_attr(&mut name, 0, CTRL_ATTR_FAMILY_NAME, b"ethtool\0")?;
    let control = ask(
        fd,
        (GENL_ID_CTRL, CTRL_CMD_GETFAMILY, 1),
        &name[..name_len],
        &mut reply,
    )?;
    let id = attr_of(control, CTRL_ATTR_FAMILY_ID)?;
    let family = u16::from_ne_bytes(id.get(..2)?.try_into().ok()?);
    let mut index = [0u8; 8];
    let index_len = put_attr(
        &mut index,
        0,
        ETHTOOL_A_HEADER_DEV_INDEX,
        &if_idx.to_ne_bytes(),
    )?;
    let mut header = [0u8; 12];
    let nested = ETHTOOL_A_RINGS_HEADER | NLA_F_NESTED;
    let header_len = put_attr(&mut header, 0, nested, &index[..index_len])?;
    let request = (family, ETHTOOL_MSG_RINGS_GET, ETHTOOL_GENL_VERSION);
    split_in(ask(fd, request, &header[..header_len], &mut reply)?)
}

/// The split a reply to `ETHTOOL_MSG_RINGS_GET` carries; `Unknown` if it
/// carries none. `None` for what is not such a reply.
pub(super) fn split_in(reply: &[u8]) -> Option<Split> {
    if kind_of(reply)? == NLMSG_ERROR {
        return None;
    }
    Some(
        match attr_of(reply, ETHTOOL_A_RINGS_TCP_DATA_SPLIT).and_then(|v| v.first()) {
            Some(1) => Split::Off,
            Some(2) => Split::On,
            Some(_) | None => Split::Unknown,
        },
    )
}

/// Send one request of `(family, command, version)` with `attrs`, and
/// receive its reply into `into`. The kernel answers a generic netlink
/// request before `send` returns, so the receive does not wait.
fn ask<'a>(
    fd: c_int,
    (family, cmd, version): (u16, u8, u8),
    attrs: &[u8],
    into: &'a mut [u8],
) -> Option<&'a [u8]> {
    let mut msg = [0u8; 64];
    let len = HEADS + attrs.len();
    msg.get_mut(HEADS..len)?.copy_from_slice(attrs);
    msg[..4].copy_from_slice(&u32::try_from(len).ok()?.to_ne_bytes());
    msg[4..6].copy_from_slice(&family.to_ne_bytes());
    msg[6..8].copy_from_slice(&NLM_F_REQUEST.to_ne_bytes());
    msg[8..12].copy_from_slice(&1u32.to_ne_bytes());
    (msg[16], msg[17]) = (cmd, version);
    let sent = unsafe { send(fd, msg.as_ptr().cast(), len, 0) };
    if usize::try_from(sent).ok()? != len {
        return None;
    }
    let received = unsafe { recv(fd, into.as_mut_ptr().cast(), into.len(), MSG_DONTWAIT) };
    let reply = into.get(..usize::try_from(received).ok()?)?;
    (kind_of(reply)? != NLMSG_ERROR).then_some(reply)
}

/// Write the attribute `(kind, value)` at `at` of `into`, padded to four
/// bytes; the end of it.
fn put_attr(into: &mut [u8], at: usize, kind: u16, value: &[u8]) -> Option<usize> {
    let len = 4 + value.len();
    let end = at + len.next_multiple_of(4);
    let slot = into.get_mut(at..end)?;
    slot[..2].copy_from_slice(&u16::try_from(len).ok()?.to_ne_bytes());
    slot[2..4].copy_from_slice(&kind.to_ne_bytes());
    slot[4..len].copy_from_slice(value);
    Some(end)
}

/// The message type of a netlink message.
fn kind_of(msg: &[u8]) -> Option<u16> {
    Some(u16::from_ne_bytes(msg.get(4..6)?.try_into().ok()?))
}

/// The value of the top-level attribute `want` of a generic netlink
/// message, if it has one whole.
pub(super) fn attr_of(msg: &[u8], want: u16) -> Option<&[u8]> {
    let total = u32::from_ne_bytes(msg.get(..4)?.try_into().ok()?) as usize;
    let mut rest = msg.get(HEADS..total.min(msg.len()))?;
    while rest.len() >= 4 {
        let len = u16::from_ne_bytes([rest[0], rest[1]]) as usize;
        let kind = u16::from_ne_bytes([rest[2], rest[3]]) & !NLA_F_NESTED;
        let value = rest.get(4..len)?;
        if kind == want {
            return Some(value);
        }
        rest = rest.get(len.next_multiple_of(4)..).unwrap_or(&[]);
    }
    None
}
