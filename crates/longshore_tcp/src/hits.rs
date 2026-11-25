// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A table of counts, a row for each state and a column for each event:
//! what the coverage of each state table is kept in.

/// How often each of `C` events came in each of `R` states. Counted in
/// debug builds only.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Hits<const R: usize, const C: usize>([[u32; C]; R]);

impl<const R: usize, const C: usize> Hits<R, C> {
    /// Nothing counted.
    pub(crate) const NONE: Hits<R, C> = Hits([[0; C]; R]);

    /// The count of `col` in `row`.
    pub(crate) const fn get(&self, row: usize, col: usize) -> u32 {
        self.0[row][col]
    }

    /// Count `col` in `row`, in a debug build.
    #[inline]
    pub(crate) fn count(&mut self, row: usize, col: usize) {
        if cfg!(debug_assertions) {
            let cell = &mut self.0[row][col];
            *cell = cell.saturating_add(1);
        }
    }

    /// Add what `other` counted.
    pub(crate) fn add(&mut self, other: &Hits<R, C>) {
        for (row, more) in self.0.iter_mut().zip(other.0.iter()) {
            for (cell, n) in row.iter_mut().zip(more.iter()) {
                *cell = cell.saturating_add(*n);
            }
        }
    }
}
