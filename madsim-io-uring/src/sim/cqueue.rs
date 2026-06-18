//! Completion queue types.

use super::Inner;

/// A completion queue entry (CQE).
#[derive(Debug, Clone, Copy)]
pub struct Entry {
    user_data: u64,
    result: i32,
    flags: u32,
}

impl Entry {
    pub(crate) fn new(user_data: u64, result: i32, flags: u32) -> Entry {
        Entry {
            user_data,
            result,
            flags,
        }
    }

    /// The result of the operation: a non-negative byte count on success, or
    /// the negated `errno` on failure (matching the kernel's convention).
    pub fn result(&self) -> i32 {
        self.result
    }

    /// The user data copied from the originating submission queue entry.
    pub fn user_data(&self) -> u64 {
        self.user_data
    }

    /// The completion flags.
    pub fn flags(&self) -> u32 {
        self.flags
    }
}

/// An accessor for the completion queue of an [`IoUring`](crate::IoUring).
///
/// Iterate it to reap completions; [`next`](Iterator::next) yields each ready
/// [`Entry`] in submission order.
pub struct CompletionQueue<'a> {
    inner: &'a Inner,
}

impl<'a> CompletionQueue<'a> {
    pub(crate) fn new(inner: &'a Inner) -> Self {
        CompletionQueue { inner }
    }

    /// Synchronizes the completion queue with the kernel. A no-op in the
    /// simulator.
    pub fn sync(&mut self) {}

    /// Returns the number of completions ready to be reaped.
    pub fn len(&self) -> usize {
        self.inner.cq.lock().unwrap().len()
    }

    /// Returns `true` if there are no completions ready.
    pub fn is_empty(&self) -> bool {
        self.inner.cq.lock().unwrap().is_empty()
    }

    /// Returns `true` if the completion queue is at its nominal capacity.
    pub fn is_full(&self) -> bool {
        self.inner.cq.lock().unwrap().len() >= self.inner.cq_entries
    }

    /// The number of completions lost to overflow. Always `0`: the simulator
    /// never drops completions.
    pub fn overflow(&self) -> u32 {
        0
    }
}

impl Iterator for CompletionQueue<'_> {
    type Item = Entry;

    fn next(&mut self) -> Option<Entry> {
        self.inner.cq.lock().unwrap().pop_front()
    }
}
