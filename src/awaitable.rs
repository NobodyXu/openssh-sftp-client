use core::fmt::Debug;
use core::hint::spin_loop;
use core::mem;
use core::task::Waker;

use std::sync::Arc;

use parking_lot::Mutex;

#[derive(Debug)]
enum InnerState<Input, Output> {
    Ongoing(Option<Input>, Option<Waker>),

    /// The awaitable is done
    Done(Output),
}
use InnerState::*;

#[derive(Debug)]
pub(crate) struct Awaitable<Input, Output>(Arc<Mutex<InnerState<Input, Output>>>);

impl<Input, Output> Clone for Awaitable<Input, Output> {
    fn clone(&self) -> Self {
        Awaitable(self.0.clone())
    }
}

impl<Input: Debug, Output: Debug> Awaitable<Input, Output> {
    pub(crate) fn new(input: Option<Input>) -> Self {
        let state = Ongoing(input, None);
        Self(Arc::new(Mutex::new(state)))
    }

    /// Return true if the task is already done.
    pub(crate) fn install_waker(&self, waker: Waker) -> bool {
        let mut guard = self.0.lock();

        match &mut *guard {
            Ongoing(_input, stored_waker) => {
                if stored_waker.is_some() {
                    panic!("Waker is installed twice before the awaitable is done");
                }
                *stored_waker = Some(waker);
                false
            }
            Done(_) => true,
        }
    }

    pub(crate) fn take_input(&self) -> Option<Input> {
        let mut guard = self.0.lock();

        match &mut *guard {
            Ongoing(input, _stored_waker) => input.take(),
            Done(_) => None,
        }
    }

    pub(crate) fn done(self, value: Output) {
        let stored_waker = {
            // hold the lock so that the waker will be called
            // only after self is dropped.
            let mut guard = self.0.lock();

            let prev_state = mem::replace(&mut *guard, Done(value));

            match prev_state {
                Done(_) => panic!("Awaitable is marked as done twice"),
                Ongoing(_input, stored_waker) => stored_waker,
            }
        };

        drop(self);

        if let Some(waker) = stored_waker {
            waker.wake();
        }
    }

    /// Precondition: This must be called only if `install_waker` returns `true`
    /// or the waker registered in `install_waker` is called.
    pub(crate) fn get_value(self) -> Option<Output> {
        let mut this = self.0;
        let state = loop {
            match Arc::try_unwrap(this) {
                Ok(mutex) => break mutex.into_inner(),

                // This branch would only happen if `install_waker` returns
                // `true`, which is quite rare considering that usually
                // the waker will be registered first before the response
                // arrived.
                //
                // `done` has been called, but it hasn't drop `self` yet.
                // Use busy loop to wait for it to happen.
                Err(arc) => {
                    spin_loop();
                    this = arc;
                }
            }
        };

        match state {
            Done(value) => Some(value),
            _ => None,
        }
    }
}
