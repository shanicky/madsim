//! Common types referenced by submission entries.

use std::os::fd::RawFd;

/// A raw file descriptor that has not been registered with the ring.
#[derive(Debug, Clone, Copy)]
pub struct Fd(pub RawFd);

/// An index into the files registered via
/// [`Submitter::register_files`](crate::Submitter::register_files).
#[derive(Debug, Clone, Copy)]
pub struct Fixed(pub u32);

/// Flags for the [`Fsync`](crate::opcode::Fsync) operation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FsyncFlags(u32);

impl FsyncFlags {
    /// Full file integrity sync (the default).
    pub const fn empty() -> FsyncFlags {
        FsyncFlags(0)
    }

    /// Data-only sync, like `fdatasync(2)`.
    pub const DATASYNC: FsyncFlags = FsyncFlags(1 << 0);

    /// Returns the raw bits of the flags.
    pub const fn bits(self) -> u32 {
        self.0
    }
}

impl std::ops::BitOr for FsyncFlags {
    type Output = FsyncFlags;
    fn bitor(self, rhs: FsyncFlags) -> FsyncFlags {
        FsyncFlags(self.0 | rhs.0)
    }
}
