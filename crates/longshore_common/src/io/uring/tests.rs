// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use super::completions::{decode_ring_buf, dispatch_cqe, CqeDispatch};
use super::*;

#[test]
fn cqe_dispatch_uses_slot_state_before_cqe_more_flag() {
    assert_eq!(
        dispatch_cqe(State::InFlight, IORING_CQE_F_MORE),
        CqeDispatch::Single
    );
    assert_eq!(
        dispatch_cqe(State::Accept, IORING_CQE_F_MORE),
        CqeDispatch::Single
    );
    assert_eq!(
        dispatch_cqe(State::SendZc, IORING_CQE_F_MORE),
        CqeDispatch::SendZc
    );
    assert_eq!(
        dispatch_cqe(State::ArmedAccept, IORING_CQE_F_MORE),
        CqeDispatch::Armed {
            more: true,
            read_buf: false,
        }
    );
    assert_eq!(
        dispatch_cqe(State::ArmedAccept, 0),
        CqeDispatch::Armed {
            more: false,
            read_buf: false,
        }
    );
    assert_eq!(
        dispatch_cqe(State::ArmedRecv, IORING_CQE_F_MORE | IORING_CQE_F_BUFFER),
        CqeDispatch::Armed {
            more: true,
            read_buf: true,
        }
    );
    assert_eq!(
        dispatch_cqe(State::ArmedRecv, 0),
        CqeDispatch::Armed {
            more: false,
            read_buf: true,
        }
    );
}

#[test]
fn empty_buffer_completions_return_to_ring_without_exposing_buf_id() {
    let base = 10;
    let buf_flags = IORING_CQE_F_BUFFER | (7u32 << IORING_CQE_BUFFER_SHIFT);

    let mut returned = None;
    let got = decode_ring_buf(buf_flags, 64, base, |id| returned = Some(id));
    assert_eq!(got, Some(BufId(17)));
    assert_eq!(returned, None);

    for res in [0, -ECANCELED, -104] {
        returned = None;
        let empty = decode_ring_buf(buf_flags, res, base, |id| returned = Some(id));
        assert_eq!(empty, None, "res={res}");
        assert_eq!(returned, Some(7), "res={res}");
    }

    returned = None;
    assert_eq!(
        decode_ring_buf(IORING_CQE_F_MORE, 0, base, |id| returned = Some(id)),
        None
    );
    assert_eq!(returned, None);
}
