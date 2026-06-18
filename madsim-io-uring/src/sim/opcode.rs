//! Operation builders that produce submission queue [`Entry`]s.
//!
//! Only the file-oriented opcodes used by the simulator are provided: [`Nop`],
//! [`Read`], [`Write`], [`Fsync`], and [`Fallocate`]. Each mirrors the real
//! crate's builder shape (`new(...).offset(..).build()`).

use super::squeue::Entry;
use super::types::FsyncFlags;
use super::{Op, Target, UseFixed};

/// A no-operation. Completes with a result of `0`.
#[derive(Debug, Default, Clone, Copy)]
pub struct Nop;

impl Nop {
    /// Creates a new `Nop`.
    pub fn new() -> Self {
        Nop
    }

    /// Builds the submission entry.
    pub fn build(self) -> Entry {
        Entry::from_op(Op::Nop)
    }
}

/// Reads from a file descriptor at an offset, equivalent to `pread(2)`.
pub struct Read {
    fd: Target,
    buf: *mut u8,
    len: u32,
    offset: u64,
}

impl Read {
    /// Creates a read of `len` bytes into `buf` from `fd`.
    pub fn new(fd: impl UseFixed, buf: *mut u8, len: u32) -> Self {
        Read {
            fd: fd.target(),
            buf,
            len,
            offset: 0,
        }
    }

    /// Sets the file offset to read from.
    pub fn offset(mut self, offset: u64) -> Self {
        self.offset = offset;
        self
    }

    /// Sets the I/O priority. Accepted but ignored by the simulator.
    pub fn ioprio(self, _ioprio: u16) -> Self {
        self
    }

    /// Sets per-I/O `preadv2`-style flags. Accepted but ignored.
    pub fn rw_flags(self, _rw_flags: i32) -> Self {
        self
    }

    /// Sets the buffer group for buffer selection. Accepted but ignored.
    pub fn buf_group(self, _buf_group: u16) -> Self {
        self
    }

    /// Builds the submission entry.
    pub fn build(self) -> Entry {
        Entry::from_op(Op::Read {
            fd: self.fd,
            addr: self.buf as u64,
            len: self.len,
            offset: self.offset,
        })
    }
}

/// Writes to a file descriptor at an offset, equivalent to `pwrite(2)`.
pub struct Write {
    fd: Target,
    buf: *const u8,
    len: u32,
    offset: u64,
}

impl Write {
    /// Creates a write of `len` bytes from `buf` to `fd`.
    pub fn new(fd: impl UseFixed, buf: *const u8, len: u32) -> Self {
        Write {
            fd: fd.target(),
            buf,
            len,
            offset: 0,
        }
    }

    /// Sets the file offset to write to.
    pub fn offset(mut self, offset: u64) -> Self {
        self.offset = offset;
        self
    }

    /// Sets the I/O priority. Accepted but ignored by the simulator.
    pub fn ioprio(self, _ioprio: u16) -> Self {
        self
    }

    /// Sets per-I/O `pwritev2`-style flags. Accepted but ignored.
    pub fn rw_flags(self, _rw_flags: i32) -> Self {
        self
    }

    /// Builds the submission entry.
    pub fn build(self) -> Entry {
        Entry::from_op(Op::Write {
            fd: self.fd,
            addr: self.buf as u64,
            len: self.len,
            offset: self.offset,
        })
    }
}

/// Synchronizes a file's state, equivalent to `fsync(2)`.
///
/// The simulator never buffers writes, so this only validates the descriptor.
pub struct Fsync {
    fd: Target,
}

impl Fsync {
    /// Creates an fsync of `fd`.
    pub fn new(fd: impl UseFixed) -> Self {
        Fsync { fd: fd.target() }
    }

    /// Sets the fsync flags. Accepted but ignored by the simulator.
    pub fn flags(self, _flags: FsyncFlags) -> Self {
        self
    }

    /// Builds the submission entry.
    pub fn build(self) -> Entry {
        Entry::from_op(Op::Fsync { fd: self.fd })
    }
}

/// Manipulates file space, equivalent to `fallocate(2)`.
///
/// The simulator only ever grows the file to `offset + len`; the `mode` is
/// accepted but ignored.
pub struct Fallocate {
    fd: Target,
    len: u64,
    offset: u64,
}

impl Fallocate {
    /// Creates a fallocate of `len` bytes on `fd`.
    pub fn new(fd: impl UseFixed, len: u64) -> Self {
        Fallocate {
            fd: fd.target(),
            len,
            offset: 0,
        }
    }

    /// Sets the offset at which to allocate.
    pub fn offset(mut self, offset: u64) -> Self {
        self.offset = offset;
        self
    }

    /// Sets the fallocate mode. Accepted but ignored by the simulator.
    pub fn mode(self, _mode: i32) -> Self {
        self
    }

    /// Builds the submission entry.
    pub fn build(self) -> Entry {
        Entry::from_op(Op::Fallocate {
            fd: self.fd,
            offset: self.offset,
            len: self.len,
        })
    }
}
