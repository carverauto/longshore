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

enum Stage<A, F> {
    Slot { build: F, closed: A },
    Sent(u32),
    Done,
}

/// Future for a single request/answer exchange on one [`Sender`].
pub struct CallFuture<'a, M: Copy, A, F: FnOnce(u32) -> M> {
    sender: &'a Sender<M, A>,
    waiter: SlotWaiter,
    stage: Stage<A, F>,
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
        if !matches!(self.stage, Stage::Slot { .. }) {
            return Poll::Ready(None);
        }
        match self
            .waiter
            .poll_one(self.sender.pool(), || !self.sender.is_closed(), cx)
        {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => match core::mem::replace(&mut self.stage, Stage::Done) {
                Stage::Slot { closed, .. } => Poll::Ready(Some(closed)),
                Stage::Sent(_) | Stage::Done => unreachable!(),
            },
            Poll::Ready(Some(slot)) => {
                let (build, closed) = match core::mem::replace(&mut self.stage, Stage::Done) {
                    Stage::Slot { build, closed } => (build, closed),
                    Stage::Sent(_) | Stage::Done => unreachable!(),
                };
                let msg = build(slot);
                match self.sender.send_slot(slot, None, msg) {
                    Ok(()) => {
                        self.stage = Stage::Sent(slot);
                        Poll::Ready(None)
                    }
                    Err(TrySendError::Closed(_)) => {
                        self.sender.pool().give(slot);
                        Poll::Ready(Some(closed))
                    }
                    #[expect(
                        clippy::panic,
                        reason = "full while a slot is held only if Config::capacity < slots, a misconfiguration with no answer to return"
                    )]
                    Err(TrySendError::Full(_)) => {
                        self.sender.pool().give(slot);
                        panic!(
                            "mailbox queue full while holding a slot (capacity must be >= slots)"
                        );
                    }
                }
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
            Stage::Slot { .. } | Stage::Done => unreachable!("slot already acquired"),
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
            Stage::Slot { .. } => unreachable!("slot already acquired"),
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
            Stage::Sent(slot) => {
                let _: Option<A> = self.sender.detach(slot);
            }
            Stage::Done => {}
        }
    }
}
