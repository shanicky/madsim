# madsim-io-uring

[![Crate](https://img.shields.io/crates/v/madsim-io-uring.svg)](https://crates.io/crates/madsim-io-uring)
[![Docs](https://docs.rs/madsim-io-uring/badge.svg)](https://docs.rs/madsim-io-uring)

The [`io-uring`] simulator on madsim.

> If it looks like io-uring, acts like io-uring, and is used like io-uring, then it probably is io-uring.

## Usage

Replace the `io-uring` entry in your Cargo.toml:

```toml
[dependencies]
io-uring = { version = "0.1", package = "madsim-io-uring" }
```

In a normal build this transparently re-exports the real [`io-uring`] crate, so
it is a drop-in replacement. When built with `--cfg madsim` it provides a
deterministic, in-memory simulation of an io_uring instance that runs on any
platform.

## Building and testing

The simulation is selected by the `--cfg madsim` flag. Because rustdoc compiles
doctests separately, the flag must be passed via both `RUSTFLAGS` and
`RUSTDOCFLAGS`:

```sh
RUSTFLAGS="--cfg madsim" RUSTDOCFLAGS="--cfg madsim" cargo test
```

## Files in simulation

The real crate operates on raw file descriptors. Under simulation, obtain a
descriptor from madsim's deterministic file system via `AsRawFd`; the ring then
reads and writes the same in-memory inode:

```rust
use madsim_io_uring::{opcode, types, IoUring};
use std::os::fd::AsRawFd;

let file = madsim::fs::File::create("data").await?;
let fd = file.as_raw_fd();

let mut ring = IoUring::new(8)?;
let buf = b"hello";
let write = opcode::Write::new(types::Fd(fd), buf.as_ptr(), buf.len() as u32)
    .offset(0)
    .build()
    .user_data(1);
unsafe { ring.submission().push(&write).expect("submission queue is full") };
ring.submit_and_wait(1)?;

let cqe = ring.completion().next().unwrap();
assert_eq!(cqe.user_data(), 1);
assert_eq!(cqe.result(), buf.len() as i32); // bytes written, or negated errno
```

## Supported API

- **Ring**: `IoUring::new` / `builder`, `submission` / `completion` /
  `submitter` / `submit` / `submit_and_wait` / `split`.
- **Submission queue**: `push` / `push_multiple` with faithful `PushError`
  back-pressure, plus `is_full` / `len` / `capacity`.
- **Completion queue**: iteration via `next`, returning `result` / `user_data` /
  `flags`. The `result` is a byte count on success or the negated `errno` on
  failure, matching the kernel convention.
- **Opcodes**: `Nop`, `Read`, `Write`, `Fsync`, `Fallocate`, each targeting a
  `types::Fd` or a registered `types::Fixed`.
- **Registration**: `register_buffers` / `register_files` and their
  `unregister_*` counterparts.

## Limitations

The simulation models the queue mechanics and per-operation results, not kernel
internals:

- Completions are produced in submission order; out-of-order completion is not
  modeled.
- `setup_*` tuning (SQPOLL, IOPOLL, single-issuer, completion sizing, …) is
  accepted and ignored, except `setup_cqsize`.
- Submitting executes operations synchronously, so there is no partial
  submission or in-flight state; `submit_and_wait(want)` never blocks for more
  completions than were submitted.
- Vectored, networking, and other opcodes are not yet simulated.

[`io-uring`]: https://docs.rs/io-uring
