use std::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

/// 用于一些快速操作的同步自旋锁
pub struct SpinLock<T> {
    lock: AtomicBool,
    data: UnsafeCell<T>,
}

pub struct LockGuard<'a, T> {
    lock: &'a AtomicBool,
    data: NonNull<T>,
}

impl<T> SpinLock<T> {
    pub fn new(val: T) -> Self {
        Self {
            lock: AtomicBool::new(false),
            data: UnsafeCell::new(val),
        }
    }

    pub fn lock(&self) -> LockGuard<'_, T> {
        while self
            .lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::hint::spin_loop();
        }
        LockGuard {
            lock: &self.lock,
            data: unsafe { NonNull::new_unchecked(self.data.get()) },
        }
    }
}

impl<T> Drop for LockGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.store(false, Ordering::Release);
    }
}
impl<T> Deref for LockGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.data.as_ref() }
    }
}

impl<T> DerefMut for LockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.data.as_mut() }
    }
}

unsafe impl<T: Send> Send for SpinLock<T> {}
unsafe impl<T> Sync for SpinLock<T> {}
