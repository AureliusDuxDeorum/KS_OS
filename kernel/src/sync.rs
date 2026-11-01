//! Interrupt-safe spin lock.
//!
//! Disables interrupts on the local CPU while the lock is held and restores the
//! previous interrupt state on release. This makes it safe to take the same lock
//! from both normal code and interrupt handlers (an interrupt can never preempt
//! the holder on the same CPU, so it cannot deadlock against it).

use crate::arch;
use core::ops::{Deref, DerefMut};

pub struct IrqMutex<T> {
    inner: spin::Mutex<T>,
}

pub struct IrqGuard<'a, T> {
    guard: Option<spin::MutexGuard<'a, T>>,
    were_enabled: bool,
}

impl<T> IrqMutex<T> {
    pub const fn new(value: T) -> Self {
        Self {
            inner: spin::Mutex::new(value),
        }
    }

    pub fn lock(&self) -> IrqGuard<'_, T> {
        let were_enabled = arch::interrupts_enabled();
        arch::cli();
        IrqGuard {
            guard: Some(self.inner.lock()),
            were_enabled,
        }
    }

    /// Takes the lock only if it is free; never blocks.
    pub fn try_lock(&self) -> Option<IrqGuard<'_, T>> {
        let were_enabled = arch::interrupts_enabled();
        arch::cli();
        match self.inner.try_lock() {
            Some(g) => Some(IrqGuard {
                guard: Some(g),
                were_enabled,
            }),
            None => {
                if were_enabled {
                    arch::sti();
                }
                None
            }
        }
    }

    /// Forcibly releases the lock (panic/fault paths only).
    ///
    /// # Safety
    /// The caller must guarantee nobody else will use the protected data.
    pub unsafe fn force_unlock(&self) {
        // SAFETY: forwarded caller guarantee.
        unsafe { self.inner.force_unlock() };
    }
}

impl<T> Deref for IrqGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.guard.as_ref().expect("guard held")
    }
}

impl<T> DerefMut for IrqGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.guard.as_mut().expect("guard held")
    }
}

impl<T> Drop for IrqGuard<'_, T> {
    fn drop(&mut self) {
        self.guard = None; // release the lock first
        if self.were_enabled {
            arch::sti();
        }
    }
}
