// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The plan's arithmetic and its refusals (contract C2).

use crate::rig::base;
use longshore_common::io::Limits;
use longshore_file::{ConfigError, FileConfig, FileSizes};

#[test]
fn the_plan_adds_what_the_table_says() {
    let others = base()
        .with_ops_max(10)
        .with_files_max(3)
        .with_small_buffers(5)
        .with_gather(2, 100);
    let sizes = FileSizes::new(4, 3)
        .with_shared(6)
        .with_paths(2)
        .with_spans_max(64)
        .with_token_base(1000);
    let (l, c) = FileConfig::plan(others, sizes).unwrap();
    // ops = 4 reserved + 2 path + 6 shared.
    let ops = 12;
    assert_eq!(l.ops_max(), 10 + ops);
    assert_eq!(l.files_max(), 3 + 4);
    assert_eq!((l.gather_ops(), l.gather_spans()), (2 + ops, 100));
    assert_eq!(l.small_buffers(), 5 + 4 + 2 + ops);
    // Untouched: the class sizes are the pool's.
    assert_eq!(l.small_buffer_size(), 4096);
    assert_eq!(l.aligned_buffers(), base().aligned_buffers());
    assert_eq!(c.token_base(), 1000);
    assert_eq!(c.tokens(), u64::from(ops) + 4);
    assert_eq!(c.path_max(), (4096 - 256) / 2 - 1);
    // More spans than any other layer asked for raise the backend's.
    let (wide, _) = FileConfig::plan(base(), FileSizes::new(1, 1).with_spans_max(300)).unwrap();
    assert_eq!(wide.gather_spans(), 300);
}

#[test]
fn the_plan_refuses_what_cannot_be_kept() {
    let p = |s: FileSizes| FileConfig::plan(base(), s).map(|_| ());
    assert_eq!(p(FileSizes::new(0, 1)), Err(ConfigError::Ops));
    assert_eq!(p(FileSizes::new(2, 0)), Err(ConfigError::Ops));
    // A file can have its own slot and every shared one, no more.
    assert_eq!(p(FileSizes::new(2, 4).with_shared(3)), Ok(()));
    assert_eq!(
        p(FileSizes::new(2, 5).with_shared(3)),
        Err(ConfigError::Ops)
    );
    assert_eq!(
        p(FileSizes::new(2, 2).with_sequence_max(3)),
        Err(ConfigError::Sequence)
    );
    assert_eq!(
        p(FileSizes::new(2, 2).with_sequence_max(0)),
        Err(ConfigError::Sequence)
    );
    assert_eq!(
        p(FileSizes::new(2, 2).with_spans_max(0)),
        Err(ConfigError::Spans)
    );
    assert_eq!(
        p(FileSizes::new(2, 2).with_spans_max(1025)),
        Err(ConfigError::Spans)
    );
    assert_eq!(p(FileSizes::new(2, 2).with_spans_max(341)), Ok(()));
    assert_eq!(
        p(FileSizes::new(1, 1).with_token_base(u64::MAX - 1)),
        Err(ConfigError::Tokens)
    );
}

#[test]
fn the_small_class_must_hold_a_span_list_and_two_paths() {
    let with = |size: u32, spans: u32| {
        let l = Limits::BASE.with_small_buffer_size(size);
        FileConfig::plan(l, FileSizes::new(1, 1).with_spans_max(spans)).map(|(_, c)| c.path_max())
    };
    assert_eq!(with(0, 1), Err(ConfigError::ClassSize));
    // 256 bytes of status and two paths of one byte, each with its NUL.
    assert_eq!(with(259, 1), Err(ConfigError::ClassSize));
    assert_eq!(with(260, 1), Ok(1));
    // Twelve bytes a span.
    assert_eq!(with(1024, 86), Err(ConfigError::ClassSize));
    assert_eq!(with(1032, 86), Ok(387));
}

#[test]
fn a_second_plan_on_top_takes_the_next_slots() {
    let (l, a) = FileConfig::plan(base(), FileSizes::new(2, 1)).unwrap();
    let (l2, b) = FileConfig::plan(l, FileSizes::new(3, 1).with_token_base(a.tokens())).unwrap();
    assert_eq!(l2.files_max(), 5);
    assert_eq!(b.token_base(), a.tokens());
    assert_eq!(l2.ops_max(), l.ops_max() + 3 + 4 + 3);
}
