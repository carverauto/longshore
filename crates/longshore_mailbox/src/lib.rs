// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Bounded lock-free mailboxes between I/O cores and caller waiter slots.

mod cells;
mod future;
mod mailbox;
mod pool;
mod queue;
pub mod sync;

pub use cells::Cells;
pub use future::{block_on, CallFuture};
pub use mailbox::{mailbox, mailbox_parked, mailbox_with_wake, Config, Mailbox, Receiver, Sender};
pub use pool::{waiter_id, Pool, SlotWaiter};
pub use queue::{
    queue, queue_parked, queue_with_wake, NoWaker, QueueReceiver, QueueSender, Recv, TryRecv,
    TrySendError,
};
