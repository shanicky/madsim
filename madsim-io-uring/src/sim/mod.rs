//! A deterministic, in-memory simulation of the `io-uring` crate.

use std::collections::VecDeque;
use std::io;
use std::os::fd::RawFd;
use std::sync::{Arc, Mutex};

pub mod cqueue;
pub mod opcode;
pub mod squeue;
pub mod types;

use squeue::Entry;

/// Decoded description of a submission queue operation.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Op {
    Nop,
    Read {
        fd: Target,
        addr: u64,
        len: u32,
        offset: u64,
    },
    Write {
        fd: Target,
        addr: u64,
        len: u32,
        offset: u64,
    },
    Fsync {
        fd: Target,
    },
    Fallocate {
        fd: Target,
        offset: u64,
        len: u64,
    },
}

/// A descriptor target: either a raw fd or an index into the registered files.
#[doc(hidden)]
#[derive(Debug, Clone, Copy)]
pub enum Target {
    Fd(RawFd),
    Fixed(u32),
}

/// Types accepted as the file-descriptor argument of an [`opcode`].
///
/// Implemented for [`types::Fd`] and [`types::Fixed`], mirroring the real
/// crate's `sealed::UseFixed` bound. Not nameable by downstream code.
#[doc(hidden)]
pub trait UseFixed {
    fn target(self) -> Target;
}

impl UseFixed for types::Fd {
    fn target(self) -> Target {
        Target::Fd(self.0)
    }
}

impl UseFixed for types::Fixed {
    fn target(self) -> Target {
        Target::Fixed(self.0)
    }
}

/// Shared, interior-mutable state of a simulated ring.
pub(crate) struct Inner {
    sq: Mutex<VecDeque<Entry>>,
    cq: Mutex<VecDeque<cqueue::Entry>>,
    sq_entries: usize,
    cq_entries: usize,
    /// Number of fixed buffers registered via `register_buffers`.
    registered_buffers: Mutex<usize>,
    /// Files registered via `register_files`, indexed by [`types::Fixed`].
    files: Mutex<Vec<RawFd>>,
}

impl Inner {
    fn new(sq_entries: usize, cq_entries: usize) -> Self {
        Inner {
            sq: Mutex::new(VecDeque::new()),
            cq: Mutex::new(VecDeque::new()),
            sq_entries,
            cq_entries,
            registered_buffers: Mutex::new(0),
            files: Mutex::new(Vec::new()),
        }
    }

    /// Resolves an operation's target to a simulated raw descriptor.
    fn resolve(&self, target: Target) -> io::Result<RawFd> {
        match target {
            Target::Fd(fd) => Ok(fd),
            Target::Fixed(index) => self
                .files
                .lock()
                .unwrap()
                .get(index as usize)
                .copied()
                .filter(|&fd| fd >= 0)
                .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF)),
        }
    }

    /// Executes one submission entry, returning the matching completion entry.
    fn run(&self, entry: &Entry) -> cqueue::Entry {
        let result = match self.run_op(entry.op) {
            Ok(n) => n as i32,
            Err(e) => -e.raw_os_error().unwrap_or(libc::EIO),
        };
        cqueue::Entry::new(entry.user_data, result, 0)
    }

    fn run_op(&self, op: Op) -> io::Result<usize> {
        match op {
            Op::Nop => Ok(0),
            Op::Read {
                fd,
                addr,
                len,
                offset,
            } => {
                let fd = self.resolve(fd)?;
                if len == 0 {
                    return Ok(0);
                }
                if addr == 0 {
                    return Err(io::Error::from_raw_os_error(libc::EFAULT));
                }
                // Safety: the address and length come from a submission queue
                // entry whose buffer the caller guarantees is valid (see the
                // `unsafe` contract on `SubmissionQueue::push`). The simulator
                // shares the caller's address space, so the pointer is live.
                let buf = unsafe { std::slice::from_raw_parts_mut(addr as *mut u8, len as usize) };
                madsim::fs::read_at_fd(fd, buf, offset)
            }
            Op::Write {
                fd,
                addr,
                len,
                offset,
            } => {
                let fd = self.resolve(fd)?;
                if len == 0 {
                    return Ok(0);
                }
                if addr == 0 {
                    return Err(io::Error::from_raw_os_error(libc::EFAULT));
                }
                // Safety: see `Op::Read` above.
                let buf = unsafe { std::slice::from_raw_parts(addr as *const u8, len as usize) };
                madsim::fs::write_at_fd(fd, buf, offset)
            }
            Op::Fsync { fd } => {
                let fd = self.resolve(fd)?;
                madsim::fs::fsync_fd(fd)?;
                Ok(0)
            }
            Op::Fallocate { fd, offset, len } => {
                let fd = self.resolve(fd)?;
                madsim::fs::fallocate_fd(fd, offset, len)?;
                Ok(0)
            }
        }
    }

    /// Drains the submission queue, executes every entry, and appends the
    /// resulting completions. Returns the number of entries submitted.
    fn submit(&self) -> io::Result<usize> {
        let entries: Vec<Entry> = {
            let mut sq = self.sq.lock().unwrap();
            sq.drain(..).collect()
        };
        let n = entries.len();
        let mut completions: Vec<cqueue::Entry> = entries.iter().map(|e| self.run(e)).collect();
        // Real io_uring may complete independent operations out of order. Shuffle
        // the completions with madsim's deterministic RNG so that consumers which
        // (incorrectly) rely on submission order are caught, while keeping every
        // seed perfectly reproducible. Completions are paired back via user_data.
        {
            use madsim::rand::{seq::SliceRandom, thread_rng};
            completions.shuffle(&mut thread_rng());
        }
        self.cq.lock().unwrap().extend(completions);
        Ok(n)
    }
}

/// A simulated `io_uring` instance.
pub struct IoUring {
    inner: Arc<Inner>,
}

impl IoUring {
    /// Creates a new ring with the given number of submission queue entries.
    pub fn new(entries: u32) -> io::Result<IoUring> {
        IoUring::builder().build(entries)
    }

    /// Returns a [`Builder`] for configuring a new ring.
    pub fn builder() -> Builder {
        Builder::default()
    }

    /// Returns an accessor for the submission queue.
    pub fn submission(&mut self) -> squeue::SubmissionQueue<'_> {
        squeue::SubmissionQueue::new(&self.inner)
    }

    /// Returns an accessor for the completion queue.
    pub fn completion(&mut self) -> cqueue::CompletionQueue<'_> {
        cqueue::CompletionQueue::new(&self.inner)
    }

    /// Returns a [`Submitter`] for submitting entries and managing
    /// registrations.
    pub fn submitter(&self) -> Submitter<'_> {
        Submitter { inner: &self.inner }
    }

    /// Submits all queued entries to be processed.
    pub fn submit(&self) -> io::Result<usize> {
        self.inner.submit()
    }

    /// Submits all queued entries and waits for at least `want` completions.
    ///
    /// In the simulator every submitted entry completes synchronously, so this
    /// is equivalent to [`submit`](Self::submit); the `want` argument is
    /// satisfied as long as it does not exceed the number of entries submitted.
    pub fn submit_and_wait(&self, _want: usize) -> io::Result<usize> {
        self.inner.submit()
    }

    /// Splits the ring into its submitter and queue accessors.
    pub fn split(
        &mut self,
    ) -> (
        Submitter<'_>,
        squeue::SubmissionQueue<'_>,
        cqueue::CompletionQueue<'_>,
    ) {
        (
            Submitter { inner: &self.inner },
            squeue::SubmissionQueue::new(&self.inner),
            cqueue::CompletionQueue::new(&self.inner),
        )
    }
}

/// Submits entries and manages kernel-side registrations for an [`IoUring`].
pub struct Submitter<'a> {
    inner: &'a Inner,
}

impl Submitter<'_> {
    /// Submits all queued entries to be processed.
    pub fn submit(&self) -> io::Result<usize> {
        self.inner.submit()
    }

    /// Submits all queued entries and waits for at least `want` completions.
    pub fn submit_and_wait(&self, _want: usize) -> io::Result<usize> {
        self.inner.submit()
    }

    /// Registers a set of fixed buffers.
    ///
    /// The simulator records the registration but does not pin any memory;
    /// `Read`/`Write` always use the address carried by the submission entry.
    ///
    /// # Safety
    /// Mirrors the real crate: the buffers must remain valid and not be moved
    /// for as long as they are registered.
    pub unsafe fn register_buffers(&self, bufs: &[libc::iovec]) -> io::Result<()> {
        let mut registered = self.inner.registered_buffers.lock().unwrap();
        if *registered > 0 {
            // The kernel rejects a second registration with `EBUSY` until the
            // previous set is unregistered.
            return Err(io::Error::from_raw_os_error(libc::EBUSY));
        }
        *registered = bufs.len();
        Ok(())
    }

    /// Unregisters all fixed buffers.
    pub fn unregister_buffers(&self) -> io::Result<()> {
        *self.inner.registered_buffers.lock().unwrap() = 0;
        Ok(())
    }

    /// Registers a set of files, addressable via [`types::Fixed`].
    pub fn register_files(&self, fds: &[RawFd]) -> io::Result<()> {
        *self.inner.files.lock().unwrap() = fds.to_vec();
        Ok(())
    }

    /// Unregisters all files.
    pub fn unregister_files(&self) -> io::Result<()> {
        self.inner.files.lock().unwrap().clear();
        Ok(())
    }
}

/// Builder for an [`IoUring`].
///
/// Every `setup_*` method is accepted for source compatibility; the simulator
/// ignores kernel-specific tuning (SQPOLL, IOPOLL, single-issuer, …) except for
/// [`setup_cqsize`](Self::setup_cqsize), which sizes the completion queue.
#[derive(Debug, Default, Clone)]
pub struct Builder {
    cq_entries: Option<u32>,
}

impl Builder {
    /// Sets the completion queue size.
    pub fn setup_cqsize(&mut self, entries: u32) -> &mut Self {
        self.cq_entries = Some(entries);
        self
    }

    /// Builds the ring with the given number of submission queue entries.
    pub fn build(&self, entries: u32) -> io::Result<IoUring> {
        // The kernel rounds the submission queue up to a power of two; the
        // completion queue defaults to twice that. Matching this keeps
        // back-pressure (when `push` starts returning `PushError`) faithful.
        let sq_entries = (entries as usize).max(1).next_power_of_two();
        let cq_entries = self
            .cq_entries
            .map(|c| (c as usize).next_power_of_two())
            .unwrap_or(sq_entries * 2);
        Ok(IoUring {
            inner: Arc::new(Inner::new(sq_entries, cq_entries)),
        })
    }
}

/// Generates accepted-but-ignored `setup_*` builder methods.
macro_rules! ignored_setup {
    ($($(#[$m:meta])* $name:ident ( $($arg:ident : $ty:ty),* ) ),* $(,)?) => {
        impl Builder {
            $(
                $(#[$m])*
                #[doc = "Accepted for source compatibility; ignored by the simulator."]
                pub fn $name(&mut self $(, $arg: $ty)*) -> &mut Self {
                    $( let _ = $arg; )*
                    self
                }
            )*
        }
    };
}

ignored_setup! {
    dontfork(),
    setup_iopoll(),
    setup_sqpoll(idle: u32),
    setup_sqpoll_cpu(cpu: u32),
    setup_clamp(),
    setup_attach_wq(fd: RawFd),
    setup_r_disabled(),
    setup_submit_all(),
    setup_coop_taskrun(),
    setup_taskrun_flag(),
    setup_defer_taskrun(),
    setup_single_issuer(),
    setup_no_sqarray(),
}

#[cfg(test)]
mod tests;
