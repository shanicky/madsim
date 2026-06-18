//! Submission queue types.

use std::fmt;

use super::{Inner, Op};

/// A submission queue entry (SQE).
///
/// Build one from an [`opcode`](crate::opcode) and optionally attach
/// [`user_data`](Self::user_data) before pushing it onto the queue.
#[derive(Debug, Clone, Copy)]
pub struct Entry {
    pub(crate) op: Op,
    pub(crate) user_data: u64,
    #[allow(dead_code)]
    pub(crate) flags: Flags,
}

impl Entry {
    pub(crate) fn from_op(op: Op) -> Entry {
        Entry {
            op,
            user_data: 0,
            flags: Flags::empty(),
        }
    }

    /// Sets the user data, an application-supplied value returned unchanged in
    /// the matching completion queue entry.
    pub fn user_data(mut self, user_data: u64) -> Entry {
        self.user_data = user_data;
        self
    }

    /// Sets the submission flags for this entry.
    pub fn flags(mut self, flags: Flags) -> Entry {
        self.flags = flags;
        self
    }

    /// Sets the personality (credentials) for this entry. Accepted but ignored
    /// by the simulator.
    pub fn personality(self, _personality: u16) -> Entry {
        self
    }
}

/// Submission flags for an [`Entry`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags(u8);

impl Flags {
    /// `fd` is an index into the registered files array.
    pub const FIXED_FILE: Flags = Flags(1 << 0);
    /// Start this entry only after previously submitted entries complete.
    pub const IO_DRAIN: Flags = Flags(1 << 1);
    /// Form a link with the next entry in the submission ring.
    pub const IO_LINK: Flags = Flags(1 << 2);
    /// Like [`IO_LINK`](Self::IO_LINK) but unaffected by completion results.
    pub const IO_HARDLINK: Flags = Flags(1 << 3);
    /// Issue the operation asynchronously from the start.
    pub const ASYNC: Flags = Flags(1 << 4);
    /// Select a buffer from a registered group.
    pub const BUFFER_SELECT: Flags = Flags(1 << 5);

    /// Returns an empty set of flags.
    pub const fn empty() -> Flags {
        Flags(0)
    }

    /// Returns the raw bits of the flags.
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Returns `true` if all flags in `other` are set in `self`.
    pub const fn contains(self, other: Flags) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for Flags {
    type Output = Flags;
    fn bitor(self, rhs: Flags) -> Flags {
        Flags(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for Flags {
    fn bitor_assign(&mut self, rhs: Flags) {
        self.0 |= rhs.0;
    }
}

/// The error returned by [`SubmissionQueue::push`] when the queue is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushError;

impl fmt::Display for PushError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("submission queue is full")
    }
}

impl std::error::Error for PushError {}

/// An accessor for the submission queue of an [`IoUring`](crate::IoUring).
pub struct SubmissionQueue<'a> {
    inner: &'a Inner,
}

impl<'a> SubmissionQueue<'a> {
    pub(crate) fn new(inner: &'a Inner) -> Self {
        SubmissionQueue { inner }
    }

    /// Synchronizes the submission queue with the kernel. A no-op in the
    /// simulator.
    pub fn sync(&mut self) {}

    /// Returns the capacity of the submission queue.
    pub fn capacity(&self) -> usize {
        self.inner.sq_entries
    }

    /// Returns the number of entries waiting to be submitted.
    pub fn len(&self) -> usize {
        self.inner.sq.lock().unwrap().len()
    }

    /// Returns `true` if no entries are waiting to be submitted.
    pub fn is_empty(&self) -> bool {
        self.inner.sq.lock().unwrap().is_empty()
    }

    /// Returns `true` if the submission queue is at capacity.
    pub fn is_full(&self) -> bool {
        self.inner.sq.lock().unwrap().len() >= self.inner.sq_entries
    }

    /// Pushes an entry onto the submission queue.
    ///
    /// Returns [`PushError`] if the queue is already at capacity.
    ///
    /// # Safety
    /// The caller must ensure that any buffers and file descriptors referenced
    /// by `entry` remain valid until the operation completes.
    pub unsafe fn push(&mut self, entry: &Entry) -> Result<(), PushError> {
        let mut sq = self.inner.sq.lock().unwrap();
        if sq.len() >= self.inner.sq_entries {
            return Err(PushError);
        }
        sq.push_back(*entry);
        Ok(())
    }

    /// Pushes multiple entries onto the submission queue.
    ///
    /// Returns [`PushError`] without enqueuing anything if the entries would
    /// not all fit.
    ///
    /// # Safety
    /// See [`push`](Self::push).
    pub unsafe fn push_multiple(&mut self, entries: &[Entry]) -> Result<(), PushError> {
        let mut sq = self.inner.sq.lock().unwrap();
        if sq.len() + entries.len() > self.inner.sq_entries {
            return Err(PushError);
        }
        sq.extend(entries.iter().copied());
        Ok(())
    }
}
