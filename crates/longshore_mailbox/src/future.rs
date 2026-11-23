// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Level-2 [`CallFuture`] built over [`SlotWaiter`], [`Sender`] and
//! [`block_on`].

use crate::mailbox::Sender;
use crate::pool::SlotWaiter;
use crate::queue::TrySendError;
use crate::sync::{current, park, Thread};
use core::future::Future;
use core::pin::{pin, Pin};
use core::task::{Context, Poll, Waker};
use std::sync::Arc;
use std::task::Wake;

struct Parker(Thread);

impl Wake for Parker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

#[cfg(not(longshore_model))]
fn with_parker<R>(f: impl FnOnce(&Waker) -> R) -> R {
    thread_local! {
        static PARKER: Waker = Waker::from(Arc::new(Parker(current())));
    }
    PARKER.with(f)
}

/// Drive `f` to completion on the calling thread, parking between polls.
///
/// Outside `--cfg longshore_model`, reuses a thread-local waker so repeated
/// calls on a warmed thread allocate nothing.
pub fn block_on<F: Future>(f: F) -> F::Output {
    fn drive<F: Future>(mut f: Pin<&mut F>, waker: &Waker) -> F::Output {
        let mut cx = Context::from_waker(waker);
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
            park();
        }
    }
    // One waker per thread, made the first time the thread blocks here.
    #[cfg(not(longshore_model))]
    {
        let mut f = pin!(f);
        with_parker(|waker| drive(f.as_mut(), waker))
    }
    // The model checker's threads do not outlive one run of the model.
    #[cfg(longshore_model)]
    drive(pin!(f), &Waker::from(Arc::new(Parker(current()))))
}

enum Stage<M, A, F> {
    Slot {
        build: F,
        closed: A,
    },
    /// A slot is held and the message built, but the queue was full. Other
    /// senders' messages can fill it whatever its capacity, so the send is
    /// tried again on the next poll.
    Queued {
        slot: u32,
        msg: M,
        closed: A,
    },
    Sent(u32),
    Done,
}

/// Future for a single request/answer exchange on one [`Sender`].
pub struct CallFuture<'a, M: Copy, A, F: FnOnce(u32) -> M> {
    sender: &'a Sender<M, A>,
    waiter: SlotWaiter,
    stage: Stage<M, A, F>,
}

impl<'a, M: Copy, A, F: FnOnce(u32) -> M> CallFuture<'a, M, A, F> {
    pub(crate) fn new(sender: &'a Sender<M, A>, build: F, closed: A) -> Self {
        Self {
            sender,
            waiter: SlotWaiter::new(),
            stage: Stage::Slot { build, closed },
        }
    }

    fn poll_slot(&mut self, cx: &mut Context<'_>) -> Poll<Option<A>> {
        match self.stage {
            Stage::Slot { .. } => {}
            Stage::Queued { .. } => return self.send_queued(cx),
            Stage::Sent(_) | Stage::Done => return Poll::Ready(None),
        }
        match self
            .waiter
            .poll_one(self.sender.pool(), || !self.sender.is_closed(), cx)
        {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => match core::mem::replace(&mut self.stage, Stage::Done) {
                Stage::Slot { closed, .. } => Poll::Ready(Some(closed)),
                Stage::Queued { .. } | Stage::Sent(_) | Stage::Done => unreachable!(),
            },
            Poll::Ready(Some(slot)) => {
                match core::mem::replace(&mut self.stage, Stage::Done) {
                    Stage::Slot { build, closed } => {
                        let msg = build(slot);
                        self.stage = Stage::Queued { slot, msg, closed };
                    }
                    Stage::Queued { .. } | Stage::Sent(_) | Stage::Done => unreachable!(),
                }
                self.send_queued(cx)
            }
        }
    }

    /// Send the built message for the slot held. A full queue is not an
    /// error: the receiver drains it, so the future wakes itself and tries
    /// again on the next poll.
    fn send_queued(&mut self, cx: &mut Context<'_>) -> Poll<Option<A>> {
        let Stage::Queued { slot, msg, .. } = self.stage else {
            return Poll::Ready(None);
        };
        match self.sender.send_slot(slot, None, msg) {
            Ok(()) => {
                self.stage = Stage::Sent(slot);
                Poll::Ready(None)
            }
            Err(TrySendError::Closed(_)) => {
                self.sender.pool().give(slot);
                match core::mem::replace(&mut self.stage, Stage::Done) {
                    Stage::Queued { closed, .. } => Poll::Ready(Some(closed)),
                    Stage::Slot { .. } | Stage::Sent(_) | Stage::Done => unreachable!(),
                }
            }
            Err(TrySendError::Full(_)) => {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }

    /// Block the calling thread until the receiver answers or closes.
    pub fn wait(mut self) -> A {
        if let Some(early) = block_on(core::future::poll_fn(|cx| self.poll_slot(cx))) {
            return early;
        }
        match self.stage {
            Stage::Sent(slot) => {
                let ans = self.sender.wait_answer(slot);
                self.stage = Stage::Done;
                ans
            }
            Stage::Slot { .. } | Stage::Queued { .. } | Stage::Done => {
                unreachable!("slot already acquired and the message sent")
            }
        }
    }
}

impl<M: Copy, A, F: FnOnce(u32) -> M> Future for CallFuture<'_, M, A, F> {
    type Output = A;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<A> {
        // Safety: nothing in CallFuture is self-referential.
        let me = unsafe { self.get_unchecked_mut() };
        if let Some(early) = core::task::ready!(me.poll_slot(cx)) {
            return Poll::Ready(early);
        }
        match me.stage {
            Stage::Sent(slot) => match me.sender.poll_answer(slot, cx) {
                Poll::Ready(ans) => {
                    me.stage = Stage::Done;
                    Poll::Ready(ans)
                }
                Poll::Pending => Poll::Pending,
            },
            Stage::Slot { .. } | Stage::Queued { .. } => {
                unreachable!("slot already acquired and the message sent")
            }
            #[expect(
                clippy::panic,
                reason = "polling a future after it returned Ready breaks the Future contract"
            )]
            Stage::Done => panic!("polled CallFuture after completion"),
        }
    }
}

impl<M: Copy, A, F: FnOnce(u32) -> M> Drop for CallFuture<'_, M, A, F> {
    fn drop(&mut self) {
        match self.stage {
            Stage::Slot { .. } => self.waiter.forget_one(self.sender.pool()),
            // The message never went out: nobody will answer this slot.
            Stage::Queued { slot, .. } => self.sender.pool().give(slot),
            Stage::Sent(slot) => {
                let _: Option<A> = self.sender.detach(slot);
            }
            Stage::Done => {}
        }
    }
}
