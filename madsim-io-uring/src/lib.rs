//! The [`io-uring`] simulator on madsim.
//!
//! This crate mirrors the public API of the low-level [`io-uring`] crate. In a
//! normal build it simply re-exports the real crate, so it is a drop-in
//! replacement. When built with `--cfg madsim` it provides a deterministic,
//! in-memory simulation of an io_uring instance that runs on any platform and
//! integrates with the rest of madsim.
//!
//! # Simulation model
//!
//! The simulated ring keeps its submission and completion queues in process
//! memory. Submitting the queue executes each entry immediately and
//! synchronously — exactly like the real (blocking) [`submit_and_wait`] — but
//! the "I/O" is an in-memory copy rather than a syscall:
//!
//! * The **buffer** side of a `Read`/`Write` is the raw pointer carried by the
//!   submission queue entry. Because the simulator runs in the same process as
//!   the caller, that pointer is a valid address and is used directly.
//! * The **file** side is resolved from the entry's [`RawFd`] back to an inode
//!   in madsim's simulated file system. Obtain such a descriptor from
//!   [`madsim::fs::File::as_raw_fd`].
//!
//! Submission-queue back-pressure is modelled faithfully: [`push`] returns
//! [`squeue::PushError`] once the queue is at capacity, which is the failure
//! mode exercised by batch-submitting code.
//!
//! # Example
//!
//! ```no_run
//! use madsim_io_uring::{opcode, IoUring};
//!
//! let mut ring = IoUring::new(8)?;
//! let nop = opcode::Nop::new().build().user_data(0x42);
//! // Safety: `Nop` references no buffers.
//! unsafe { ring.submission().push(&nop).expect("submission queue is full") };
//! ring.submit_and_wait(1)?;
//! let cqe = ring.completion().next().expect("a completion");
//! assert_eq!(cqe.user_data(), 0x42);
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! See the crate README for a file I/O example that bridges to
//! [`madsim::fs`](https://docs.rs/madsim/latest/madsim/fs/index.html).
//!
//! [`io-uring`]: https://docs.rs/io-uring
//! [`submit_and_wait`]: IoUring::submit_and_wait
//! [`push`]: squeue::SubmissionQueue::push
//! [`RawFd`]: std::os::fd::RawFd

#[cfg(all(not(madsim), target_os = "linux"))]
pub use io_uring::*;

#[cfg(all(not(madsim), not(target_os = "linux")))]
compile_error!(
    "the real `io-uring` crate is only available on Linux; \
     build with `--cfg madsim` to use the simulator"
);

#[cfg(madsim)]
pub use self::sim::*;
#[cfg(madsim)]
mod sim;
