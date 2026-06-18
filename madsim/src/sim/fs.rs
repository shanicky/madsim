//! Asynchronous file system.

use spin::{Mutex, RwLock};
use std::{
    collections::HashMap,
    fmt,
    io::{Error, ErrorKind, Result},
    os::fd::{AsRawFd, RawFd},
    path::{Path, PathBuf},
    sync::Arc,
};
use tracing::*;

use crate::{
    plugin::{node, simulator, Simulator},
    rand::GlobalRng,
    task::NodeId,
    time::TimeHandle,
    Config,
};

/// File system simulator.
#[cfg_attr(docsrs, doc(cfg(madsim)))]
#[derive(Default)]
pub struct FsSim {
    handles: Mutex<HashMap<NodeId, FsNodeHandle>>,
}

impl Simulator for FsSim {
    fn new(_rand: &GlobalRng, _time: &TimeHandle, _config: &Config) -> Self {
        Default::default()
    }

    fn create_node(&self, id: NodeId) {
        let mut handles = self.handles.lock();
        handles.insert(id, FsNodeHandle::new());
    }

    fn reset_node(&self, id: NodeId) {
        self.power_fail(id);
    }
}

impl FsSim {
    /// Return a handle of the specified node.
    fn get_node(&self, id: NodeId) -> FsNodeHandle {
        let handles = self.handles.lock();
        handles[&id].clone()
    }

    /// Simulate a power failure. All data that does not reach the disk will be lost.
    pub fn power_fail(&self, _id: NodeId) {
        // TODO
    }

    /// Get the size of given file.
    pub fn get_file_size(&self, node: NodeId, path: impl AsRef<Path>) -> Result<u64> {
        let path = path.as_ref();
        let handle = self.handles.lock()[&node].clone();
        let fs = handle.fs.lock();
        let inode = fs
            .get(path)
            .ok_or_else(|| Error::new(ErrorKind::NotFound, format!("file not found: {path:?}")))?;
        Ok(inode.metadata().len())
    }
}

/// File system simulator for a node.
#[derive(Clone)]
struct FsNodeHandle {
    fs: Arc<Mutex<HashMap<PathBuf, Arc<INode>>>>,
    /// Maps simulated raw file descriptors to open inodes for this node.
    fds: Arc<Mutex<FdTable>>,
}

impl FsNodeHandle {
    fn new() -> Self {
        FsNodeHandle {
            fs: Arc::new(Mutex::new(HashMap::new())),
            fds: Arc::new(Mutex::new(FdTable::new())),
        }
    }

    fn current() -> Self {
        simulator::<FsSim>().get_node(node())
    }

    async fn open(&self, path: impl AsRef<Path>) -> Result<File> {
        let path = path.as_ref();
        trace!(?path, "open file");
        let fs = self.fs.lock();
        let inode = fs
            .get(path)
            .ok_or_else(|| Error::new(ErrorKind::NotFound, format!("file not found: {path:?}")))?
            .clone();
        Ok(File {
            inode,
            can_write: false,
            fds: self.fds.clone(),
            fd: Mutex::new(None),
        })
    }

    fn create_sync(&self, path: impl AsRef<Path>) -> Result<File> {
        let path = path.as_ref();
        trace!(?path, "create file");
        let mut fs = self.fs.lock();
        let inode = fs
            .entry(path.into())
            .and_modify(|inode| inode.truncate())
            .or_insert_with(|| Arc::new(INode::new(path)))
            .clone();
        Ok(File {
            inode,
            can_write: true,
            fds: self.fds.clone(),
            fd: Mutex::new(None),
        })
    }

    async fn create(&self, path: impl AsRef<Path>) -> Result<File> {
        self.create_sync(path)
    }

    async fn metadata(&self, path: impl AsRef<Path>) -> Result<Metadata> {
        let path = path.as_ref();
        let fs = self.fs.lock();
        let inode = fs
            .get(path)
            .ok_or_else(|| Error::new(ErrorKind::NotFound, format!("file not found: {path:?}")))?;
        Ok(inode.metadata())
    }
}

struct INode {
    path: PathBuf,
    data: RwLock<Vec<u8>>,
}

impl INode {
    fn new(path: &Path) -> Self {
        INode {
            path: path.into(),
            data: RwLock::new(Vec::new()),
        }
    }

    fn truncate(&self) {
        self.data.write().clear();
    }

    fn metadata(&self) -> Metadata {
        Metadata {
            len: self.data.read().len() as u64,
        }
    }

    /// Reads bytes starting at `offset`, returning the number of bytes read.
    /// Reads that start at or beyond the end of the file return `0`.
    fn read_at(&self, buf: &mut [u8], offset: u64) -> usize {
        let data = self.data.read();
        let offset = offset as usize;
        if offset >= data.len() {
            return 0;
        }
        let end = data.len().min(offset + buf.len());
        let len = end - offset;
        buf[..len].copy_from_slice(&data[offset..end]);
        len
    }

    /// Writes `buf` starting at `offset`, zero-filling any gap before `offset`
    /// and extending the file as needed.
    fn write_at(&self, buf: &[u8], offset: u64) {
        let mut data = self.data.write();
        let offset = offset as usize;
        if offset > data.len() {
            data.resize(offset, 0);
        }
        let end = data.len().min(offset + buf.len());
        let overlap = end - offset;
        data[offset..end].copy_from_slice(&buf[..overlap]);
        if overlap < buf.len() {
            data.extend_from_slice(&buf[overlap..]);
        }
    }

    /// Grows the file so that `offset + len` bytes are allocated, zero-filling
    /// new space. Never shrinks the file.
    fn fallocate(&self, offset: u64, len: u64) -> Result<()> {
        let needed = offset
            .checked_add(len)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| Error::from_raw_os_error(libc::EFBIG))?;
        let mut data = self.data.write();
        if data.len() < needed {
            data.resize(needed, 0);
        }
        Ok(())
    }
}

/// Per-node table mapping simulated raw file descriptors to open inodes.
///
/// The raw `io-uring` simulator resolves the [`RawFd`] carried by a submission
/// queue entry back to the in-memory inode through this table, so ring I/O
/// shares the same files as the rest of [`mod@crate::fs`].
struct FdTable {
    next: RawFd,
    map: HashMap<RawFd, FdEntry>,
}

struct FdEntry {
    inode: Arc<INode>,
    writable: bool,
}

impl FdTable {
    fn new() -> Self {
        // Start past the conventional stdio descriptors; the exact values are
        // meaningful only to the simulator.
        FdTable {
            next: 3,
            map: HashMap::new(),
        }
    }

    fn register(&mut self, inode: Arc<INode>, writable: bool) -> RawFd {
        let fd = self.next;
        self.next += 1;
        self.map.insert(fd, FdEntry { inode, writable });
        fd
    }

    fn unregister(&mut self, fd: RawFd) {
        self.map.remove(&fd);
    }
}

/// A reference to an open file on the filesystem.
pub struct File {
    inode: Arc<INode>,
    can_write: bool,
    /// Node-local descriptor table this file's fd is registered in.
    fds: Arc<Mutex<FdTable>>,
    /// Lazily-allocated simulated descriptor, registered on first `as_raw_fd`.
    fd: Mutex<Option<RawFd>>,
}

impl fmt::Debug for File {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("File")
            .field("path", &self.inode.path)
            .finish()
    }
}

impl File {
    /// Attempts to open a file in read-only mode.
    pub async fn open(path: impl AsRef<Path>) -> Result<File> {
        let handle = FsNodeHandle::current();
        handle.open(path).await
    }

    /// Opens a file in write-only mode.
    ///
    /// This function will create a file if it does not exist, and will truncate it if it does.
    pub async fn create(path: impl AsRef<Path>) -> Result<File> {
        let handle = FsNodeHandle::current();
        handle.create(path).await
    }

    /// Synchronous twin of [`create`](Self::create).
    ///
    /// The simulated file system performs no real I/O, so creation is a pure
    /// in-memory operation. This lets synchronous code (such as a storage
    /// backend exercised under `--cfg madsim`) open files without an async
    /// context.
    pub fn create_sync(path: impl AsRef<Path>) -> Result<File> {
        FsNodeHandle::current().create_sync(path)
    }

    /// Reads a number of bytes starting from a given offset.
    ///
    /// Reads that start at or beyond the end of the file return `Ok(0)`.
    #[instrument(skip(buf), fields(len = buf.len()))]
    pub async fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
        let len = self.inode.read_at(buf, offset);
        // TODO: random delay
        Ok(len)
    }

    /// Attempts to write an entire buffer starting from a given offset.
    #[instrument(skip(buf), fields(len = buf.len()))]
    pub async fn write_all_at(&self, buf: &[u8], offset: u64) -> Result<()> {
        if !self.can_write {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "the file is read only",
            ));
        }
        self.inode.write_at(buf, offset);
        // TODO: random delay
        // TODO: simulate buffer, write will not take effect until flush or close
        Ok(())
    }

    /// Truncates or extends the underlying file, updating the size of this file to become `size`.
    #[instrument]
    pub async fn set_len(&self, size: u64) -> Result<()> {
        self.set_len_sync(size)
    }

    /// Synchronous twin of [`set_len`](Self::set_len).
    pub fn set_len_sync(&self, size: u64) -> Result<()> {
        self.inode.data.write().resize(size as usize, 0);
        // TODO: random delay
        Ok(())
    }

    /// Attempts to sync all OS-internal metadata to disk.
    #[instrument]
    pub async fn sync_all(&self) -> Result<()> {
        // TODO: random delay
        Ok(())
    }

    /// Queries metadata about the underlying file.
    #[instrument]
    pub async fn metadata(&self) -> Result<Metadata> {
        Ok(self.inode.metadata())
    }

    /// Registers (on first call) and returns this file's simulated descriptor.
    fn sim_raw_fd(&self) -> RawFd {
        let mut slot = self.fd.lock();
        if let Some(fd) = *slot {
            return fd;
        }
        let fd = self
            .fds
            .lock()
            .register(self.inode.clone(), self.can_write);
        *slot = Some(fd);
        fd
    }
}

/// Returns a simulated raw file descriptor.
///
/// The descriptor is registered in the node-local descriptor table on first use
/// and stays valid until this `File` is dropped. It is meaningful only to the
/// simulator (e.g. the raw `io-uring` simulator), not to the host OS.
impl AsRawFd for File {
    fn as_raw_fd(&self) -> RawFd {
        self.sim_raw_fd()
    }
}

impl Drop for File {
    fn drop(&mut self) {
        if let Some(fd) = *self.fd.lock() {
            self.fds.lock().unregister(fd);
        }
    }
}

/// Read the entire contents of a file into a bytes vector.
pub async fn read(path: impl AsRef<Path>) -> Result<Vec<u8>> {
    let handle = FsNodeHandle::current();
    let file = handle.open(path).await?;
    let data = file.inode.data.read().clone();
    // TODO: random delay
    Ok(data)
}

/// Given a path, query the file system to get information about a file, directory, etc.
pub async fn metadata(path: impl AsRef<Path>) -> Result<Metadata> {
    let handle = FsNodeHandle::current();
    handle.metadata(path).await
}

/// Looks up the inode registered for `fd` in the current node's table.
fn inode_for_fd(fd: RawFd, need_write: bool) -> Result<Arc<INode>> {
    let handle = FsNodeHandle::current();
    let table = handle.fds.lock();
    let entry = table
        .map
        .get(&fd)
        .ok_or_else(|| Error::from_raw_os_error(libc::EBADF))?;
    if need_write && !entry.writable {
        return Err(Error::from_raw_os_error(libc::EBADF));
    }
    Ok(entry.inode.clone())
}

/// Reads from the file behind a simulated raw descriptor at `offset`.
///
/// This is the bridge used by the raw `io-uring` simulator to turn a `Read`
/// submission queue entry into an in-memory file-system read. `fd` must have
/// been obtained from [`File::as_raw_fd`] on the current node.
pub fn read_at_fd(fd: RawFd, buf: &mut [u8], offset: u64) -> Result<usize> {
    Ok(inode_for_fd(fd, false)?.read_at(buf, offset))
}

/// Writes to the file behind a simulated raw descriptor at `offset`, returning
/// the number of bytes written.
///
/// This is the bridge used by the raw `io-uring` simulator to turn a `Write`
/// submission queue entry into an in-memory file-system write. The descriptor
/// must refer to a writable file.
pub fn write_at_fd(fd: RawFd, buf: &[u8], offset: u64) -> Result<usize> {
    inode_for_fd(fd, true)?.write_at(buf, offset);
    Ok(buf.len())
}

/// Grows the file behind a simulated raw descriptor so that `offset + len`
/// bytes are allocated. Used by the raw `io-uring` simulator's `Fallocate`.
pub fn fallocate_fd(fd: RawFd, offset: u64, len: u64) -> Result<()> {
    inode_for_fd(fd, true)?.fallocate(offset, len)
}

/// Validates that `fd` refers to an open file. Used by the raw `io-uring`
/// simulator's `Fsync`, which is otherwise a no-op since data is never buffered.
pub fn fsync_fd(fd: RawFd) -> Result<()> {
    inode_for_fd(fd, false)?;
    Ok(())
}

/// Metadata information about a file.
pub struct Metadata {
    len: u64,
}

impl Metadata {
    /// Returns the size of the file, in bytes, this metadata is for.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> u64 {
        self.len
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Runtime;

    #[test]
    fn create_open_read_write() {
        let runtime = Runtime::new();
        let node = runtime.create_node().build();
        let f = node.spawn(async move {
            assert_eq!(
                File::open("file").await.err().unwrap().kind(),
                ErrorKind::NotFound
            );
            let file = File::create("file").await.unwrap();
            file.write_all_at(b"hello", 0).await.unwrap();

            let mut buf = [0u8; 10];
            let read_len = file.read_at(&mut buf, 2).await.unwrap();
            assert_eq!(read_len, 3);
            assert_eq!(&buf[..3], b"llo");
            drop(file);

            // writing to a read-only file should be denied
            let rofile = File::open("file").await.unwrap();
            assert_eq!(
                rofile.write_all_at(b"gg", 0).await.err().unwrap().kind(),
                ErrorKind::PermissionDenied
            );

            // create should truncate existing file
            let file = File::create("file").await.unwrap();
            let read_len = file.read_at(&mut buf, 0).await.unwrap();
            assert_eq!(read_len, 0);
        });
        runtime.block_on(f).unwrap();
    }
}
