use std::future::Future;
use std::os::fd::AsRawFd;

use madsim::fs::File;
use madsim::runtime::Runtime;

use super::squeue::PushError;
use super::{opcode, types, IoUring};

/// Runs `fut` to completion inside a fresh single-node simulation.
fn run<Fut>(fut: Fut)
where
    Fut: Future<Output = ()> + Send + 'static,
{
    let runtime = Runtime::new();
    let node = runtime.create_node().build();
    let handle = node.spawn(fut);
    runtime.block_on(handle).unwrap();
}

#[test]
fn nop_completes() {
    run(async {
        let mut ring = IoUring::new(8).unwrap();
        let entry = opcode::Nop::new().build().user_data(42);
        unsafe { ring.submission().push(&entry).unwrap() };
        assert_eq!(ring.submit_and_wait(1).unwrap(), 1);
        let cqe = ring.completion().next().unwrap();
        assert_eq!(cqe.user_data(), 42);
        assert_eq!(cqe.result(), 0);
        assert!(ring.completion().next().is_none());
    });
}

#[test]
fn write_then_read_round_trip() {
    run(async {
        let file = File::create("data").await.unwrap();
        let fd = file.as_raw_fd();
        let mut ring = IoUring::new(8).unwrap();

        let data = *b"hello io_uring";
        let write = opcode::Write::new(types::Fd(fd), data.as_ptr(), data.len() as u32)
            .offset(0)
            .build()
            .user_data(1);
        unsafe { ring.submission().push(&write).unwrap() };
        ring.submit_and_wait(1).unwrap();
        let cqe = ring.completion().next().unwrap();
        assert_eq!(cqe.user_data(), 1);
        assert_eq!(cqe.result(), data.len() as i32);

        let mut buf = [0u8; 32];
        let read = opcode::Read::new(types::Fd(fd), buf.as_mut_ptr(), buf.len() as u32)
            .offset(0)
            .build()
            .user_data(2);
        unsafe { ring.submission().push(&read).unwrap() };
        ring.submit_and_wait(1).unwrap();
        let cqe = ring.completion().next().unwrap();
        assert_eq!(cqe.user_data(), 2);
        assert_eq!(cqe.result(), data.len() as i32);
        assert_eq!(&buf[..data.len()], &data);
    });
}

#[test]
fn read_at_offset_and_past_eof() {
    run(async {
        let file = File::create("data").await.unwrap();
        let fd = file.as_raw_fd();
        file.write_all_at(b"0123456789", 0).await.unwrap();
        let mut ring = IoUring::new(8).unwrap();

        // Read the middle of the file.
        let mut buf = [0u8; 4];
        let read = opcode::Read::new(types::Fd(fd), buf.as_mut_ptr(), buf.len() as u32)
            .offset(3)
            .build()
            .user_data(1);
        unsafe { ring.submission().push(&read).unwrap() };
        ring.submit_and_wait(1).unwrap();
        assert_eq!(ring.completion().next().unwrap().result(), 4);
        assert_eq!(&buf, b"3456");

        // Read entirely past the end of the file: a zero-length completion.
        let read = opcode::Read::new(types::Fd(fd), buf.as_mut_ptr(), buf.len() as u32)
            .offset(100)
            .build()
            .user_data(2);
        unsafe { ring.submission().push(&read).unwrap() };
        ring.submit_and_wait(1).unwrap();
        assert_eq!(ring.completion().next().unwrap().result(), 0);
    });
}

#[test]
fn bad_descriptor_reports_ebadf() {
    run(async {
        let mut ring = IoUring::new(8).unwrap();
        let mut buf = [0u8; 8];
        let read = opcode::Read::new(types::Fd(4242), buf.as_mut_ptr(), buf.len() as u32)
            .build()
            .user_data(7);
        unsafe { ring.submission().push(&read).unwrap() };
        ring.submit_and_wait(1).unwrap();
        let cqe = ring.completion().next().unwrap();
        assert_eq!(cqe.user_data(), 7);
        assert_eq!(cqe.result(), -libc::EBADF);
    });
}

#[test]
fn submission_queue_backpressure() {
    run(async {
        let mut ring = IoUring::new(2).unwrap();
        let entry = opcode::Nop::new().build();
        let mut sq = ring.submission();
        assert_eq!(sq.capacity(), 2);
        unsafe {
            sq.push(&entry).unwrap();
            sq.push(&entry).unwrap();
            assert_eq!(sq.push(&entry), Err(PushError));
        }
        assert!(sq.is_full());
        assert_eq!(sq.len(), 2);
    });
}

#[test]
fn batch_submit_pairs_by_user_data() {
    run(async {
        let file = File::create("data").await.unwrap();
        let fd = file.as_raw_fd();
        let mut ring = IoUring::new(16).unwrap();

        let chunks: [[u8; 4]; 3] = [*b"aaaa", *b"bbbb", *b"cccc"];
        for (i, chunk) in chunks.iter().enumerate() {
            let write = opcode::Write::new(types::Fd(fd), chunk.as_ptr(), 4)
                .offset((i * 4) as u64)
                .build()
                .user_data(i as u64);
            unsafe { ring.submission().push(&write).unwrap() };
        }
        assert_eq!(ring.submit_and_wait(3).unwrap(), 3);

        let mut seen = Vec::new();
        while let Some(cqe) = ring.completion().next() {
            assert_eq!(cqe.result(), 4);
            seen.push(cqe.user_data());
        }
        seen.sort_unstable();
        assert_eq!(seen, vec![0, 1, 2]);

        let mut buf = [0u8; 12];
        file.read_at(&mut buf, 0).await.unwrap();
        assert_eq!(&buf, b"aaaabbbbcccc");
    });
}

#[test]
fn fallocate_grows_file() {
    run(async {
        let file = File::create("data").await.unwrap();
        let fd = file.as_raw_fd();
        let mut ring = IoUring::new(8).unwrap();

        let entry = opcode::Fallocate::new(types::Fd(fd), 4096)
            .offset(0)
            .build()
            .user_data(1);
        unsafe { ring.submission().push(&entry).unwrap() };
        ring.submit_and_wait(1).unwrap();
        assert_eq!(ring.completion().next().unwrap().result(), 0);

        assert_eq!(file.metadata().await.unwrap().len(), 4096);
    });
}

#[test]
fn fallocate_overflow_is_rejected() {
    run(async {
        let file = File::create("data").await.unwrap();
        let fd = file.as_raw_fd();
        let mut ring = IoUring::new(8).unwrap();
        // `offset + len` overflows u64; the simulator must report an error
        // rather than panic or wrap (which would differ between debug/release).
        let entry = opcode::Fallocate::new(types::Fd(fd), u64::MAX)
            .offset(u64::MAX)
            .build()
            .user_data(1);
        unsafe { ring.submission().push(&entry).unwrap() };
        ring.submit_and_wait(1).unwrap();
        assert_eq!(ring.completion().next().unwrap().result(), -libc::EFBIG);
    });
}

#[test]
fn registered_fixed_file() {
    run(async {
        let file = File::create("data").await.unwrap();
        let fd = file.as_raw_fd();
        let mut ring = IoUring::new(8).unwrap();
        ring.submitter().register_files(&[fd]).unwrap();

        let data = *b"fixed";
        let write = opcode::Write::new(types::Fixed(0), data.as_ptr(), data.len() as u32)
            .build()
            .user_data(1);
        unsafe { ring.submission().push(&write).unwrap() };
        ring.submit_and_wait(1).unwrap();
        assert_eq!(
            ring.completion().next().unwrap().result(),
            data.len() as i32
        );

        let mut buf = [0u8; 5];
        file.read_at(&mut buf, 0).await.unwrap();
        assert_eq!(&buf, b"fixed");
    });
}

#[test]
fn fsync_validates_descriptor() {
    run(async {
        let file = File::create("data").await.unwrap();
        let fd = file.as_raw_fd();
        let mut ring = IoUring::new(8).unwrap();

        let good = opcode::Fsync::new(types::Fd(fd)).build().user_data(1);
        let bad = opcode::Fsync::new(types::Fd(9999)).build().user_data(2);
        unsafe {
            ring.submission().push(&good).unwrap();
            ring.submission().push(&bad).unwrap();
        }
        ring.submit_and_wait(2).unwrap();
        // Completions may arrive in any order; pair them back by user_data.
        let mut results = std::collections::HashMap::new();
        while let Some(cqe) = ring.completion().next() {
            results.insert(cqe.user_data(), cqe.result());
        }
        assert_eq!(results[&1], 0);
        assert_eq!(results[&2], -libc::EBADF);
    });
}

#[test]
fn register_buffers_is_exclusive() {
    run(async {
        let ring = IoUring::new(8).unwrap();
        let mut buf = [0u8; 16];
        let iov = libc::iovec {
            iov_base: buf.as_mut_ptr() as *mut libc::c_void,
            iov_len: buf.len(),
        };
        let submitter = ring.submitter();
        unsafe {
            submitter.register_buffers(std::slice::from_ref(&iov)).unwrap();
            // A second registration without unregistering is rejected.
            let err = submitter
                .register_buffers(std::slice::from_ref(&iov))
                .unwrap_err();
            assert_eq!(err.raw_os_error(), Some(libc::EBUSY));
        }
        submitter.unregister_buffers().unwrap();
        // After unregistering, registration succeeds again.
        unsafe { submitter.register_buffers(std::slice::from_ref(&iov)).unwrap() };
    });
}

/// Exercises the crate through the standard `#[madsim::test]` harness, the way
/// a downstream consumer's tests would, confirming the descriptor bridge works
/// on the default node.
#[madsim::test]
async fn works_under_madsim_test_harness() {
    let file = File::create("data").await.unwrap();
    let fd = file.as_raw_fd();
    let mut ring = IoUring::new(8).unwrap();

    let data = *b"harness";
    let write = opcode::Write::new(types::Fd(fd), data.as_ptr(), data.len() as u32)
        .build()
        .user_data(1);
    unsafe { ring.submission().push(&write).unwrap() };
    ring.submit_and_wait(1).unwrap();
    assert_eq!(ring.completion().next().unwrap().result(), data.len() as i32);

    let mut buf = [0u8; 7];
    file.read_at(&mut buf, 0).await.unwrap();
    assert_eq!(&buf, b"harness");
}

/// The observable completion stream must be identical across independent runs.
#[test]
fn deterministic_output() {
    fn scenario() -> Vec<(u64, i32)> {
        let results = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let out = results.clone();
        run(async move {
            let file = File::create("data").await.unwrap();
            let fd = file.as_raw_fd();
            let mut ring = IoUring::new(8).unwrap();
            let data = *b"deterministic";
            let write = opcode::Write::new(types::Fd(fd), data.as_ptr(), data.len() as u32)
                .build()
                .user_data(1);
            let mut buf = [0u8; 16];
            let read = opcode::Read::new(types::Fd(fd), buf.as_mut_ptr(), buf.len() as u32)
                .build()
                .user_data(2);
            unsafe {
                ring.submission().push(&write).unwrap();
                ring.submission().push(&read).unwrap();
            }
            ring.submit_and_wait(2).unwrap();
            let mut v = out.lock().unwrap();
            while let Some(cqe) = ring.completion().next() {
                v.push((cqe.user_data(), cqe.result()));
            }
        });
        std::sync::Arc::try_unwrap(results)
            .unwrap()
            .into_inner()
            .unwrap()
    }

    assert_eq!(scenario(), scenario());
}

/// The simulator models out-of-order completion: across seeds at least one run
/// reaps completions in a non-submission order, and every run still yields the
/// full set of completions (paired by user_data).
#[test]
fn completions_can_be_out_of_order() {
    fn reap_order(seed: u64) -> Vec<u64> {
        let out = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = out.clone();
        let runtime = madsim::runtime::Runtime::with_seed_and_config(seed, madsim::Config::default());
        let node = runtime.create_node().build();
        let handle = node.spawn(async move {
            let mut ring = IoUring::new(16).unwrap();
            for i in 0..8u64 {
                let entry = opcode::Nop::new().build().user_data(i);
                unsafe { ring.submission().push(&entry).unwrap() };
            }
            ring.submit_and_wait(8).unwrap();
            let mut v = sink.lock().unwrap();
            while let Some(cqe) = ring.completion().next() {
                v.push(cqe.user_data());
            }
        });
        runtime.block_on(handle).unwrap();
        std::sync::Arc::try_unwrap(out).unwrap().into_inner().unwrap()
    }

    let identity: Vec<u64> = (0..8).collect();
    let mut any_reordered = false;
    for seed in 0..32u64 {
        let order = reap_order(seed);
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, identity, "all 8 completions must be present (seed {seed})");
        if order != identity {
            any_reordered = true;
        }
    }
    assert!(
        any_reordered,
        "the simulator never reordered completions across 32 seeds"
    );
}
