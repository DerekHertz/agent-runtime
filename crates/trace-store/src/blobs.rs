//! Content-addressed blob store for a run: `runs/<run-id>/blobs/<hash>` (SPEC section 5,
//! "Layout"). A blob's name is the lowercase hex of the blake3 hash of its exact bytes, so equal
//! content is stored once and a name always says what its content must hash to.
//!
//! Visibility: `put` writes the bytes to a private temporary file in `blobs/` and then renames it
//! onto the final name. On a POSIX file system that rename is atomic, so a reader that opens a
//! blob sees the whole content or no file at all, never a prefix. An existing blob is never
//! opened for writing or truncated; a racing or repeated `put` of the same content at most
//! replaces the file with an identical one.
//!
//! Persistence: before the rename, `put` flushes the temporary file (`File::sync_all`, which is
//! `F_FULLFSYNC` on macOS), and after it, the `blobs/` directory. A `put` that finds the blob
//! already present flushes that file and `blobs/` too, because another writer may not have
//! finished its own flushes. `open` flushes the run directory so that the `blobs/` entry is
//! recorded in it. The store does not create the run directory: the caller must create it and
//! make it durable (flush it and its ancestors) before `open`. Given that, and an OS and drive
//! that honour these flushes, a returned hash names a blob that survives a crash or power loss.
//!
//! `get` rehashes what it reads and refuses content that does not match its name.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Directory of the blob store inside a run directory.
pub const BLOBS_DIR: &str = "blobs";

/// The blake3 hash of a blob's exact bytes, all 32 bytes of it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlobHash([u8; 32]);

impl BlobHash {
    /// Hash `bytes` without storing them.
    pub fn of(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl From<[u8; 32]> for BlobHash {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

/// Lowercase hex, 64 characters: the blob's file name.
impl fmt::Display for BlobHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for BlobHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BlobHash({self})")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    #[error("blob io error at {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("blob {hash} is not in the store")]
    Missing { hash: BlobHash },
    #[error("blob file {path} is corrupt: its content hashes to {actual}, not {expected}")]
    Corrupt {
        path: PathBuf,
        expected: BlobHash,
        actual: BlobHash,
    },
}

/// The blob store of one run, rooted at `run_dir/blobs`. Any number of stores, threads and
/// processes may `put` and `get` on the same directory at once.
#[derive(Debug)]
pub struct BlobStore {
    dir: PathBuf,
}

/// Distinguishes this process's temporary files from each other; the pid distinguishes them from
/// other processes'.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Temporary files start with this, so no temporary name can be a 64-hex blob name.
const TEMP_PREFIX: &str = ".tmp-";

impl BlobStore {
    /// Open the store of `run_dir`, creating `run_dir/blobs` if absent. `run_dir` must already
    /// exist; a missing one is refused, not created. Making `run_dir` itself durable is the
    /// caller's job (see the module docs).
    pub fn open(run_dir: &Path) -> Result<Self, BlobError> {
        if !fs::metadata(run_dir)
            .map_err(|source| io_error(run_dir, source))?
            .is_dir()
        {
            return Err(io_error(
                run_dir,
                io::Error::new(
                    io::ErrorKind::NotADirectory,
                    "run directory is not a directory",
                ),
            ));
        }
        let dir = run_dir.join(BLOBS_DIR);
        match fs::create_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && dir.is_dir() => {}
            Err(source) => return Err(io_error(&dir, source)),
        }
        // Even on reopen: a racing `open` may have created `blobs/` and not yet flushed it.
        sync_dir(run_dir)?;
        Ok(Self { dir })
    }

    /// The `blobs/` directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where the blob named `hash` lives, whether or not it is stored.
    pub fn path(&self, hash: &BlobHash) -> PathBuf {
        self.dir.join(hash.to_string())
    }

    /// Store `bytes` exactly and return their hash. Storing content that is already present and
    /// intact writes nothing. A file under the content's name that does not hold that content is
    /// replaced whole, never patched.
    pub fn put(&self, bytes: &[u8]) -> Result<BlobHash, BlobError> {
        let hash = BlobHash::of(bytes);
        let path = self.path(&hash);
        // Fast path only: a missing or bad blob falls through to the authoritative write below.
        // An intact blob may come from a writer that has not flushed it or `blobs/` yet, so flush
        // both before reporting success.
        if let Ok((file, _)) = read_verified(&path, &hash) {
            file.sync_all().map_err(|source| io_error(&path, source))?;
            sync_dir(&self.dir)?;
            return Ok(hash);
        }
        let temp = TempFile::create(&self.dir)?;
        temp.write(bytes)?;
        temp.publish(&path)?;
        sync_dir(&self.dir)?;
        Ok(hash)
    }

    /// The exact bytes stored under `hash`. Refuses a missing blob and a file whose content does
    /// not hash to its name.
    pub fn get(&self, hash: &BlobHash) -> Result<Vec<u8>, BlobError> {
        read_verified(&self.path(hash), hash).map(|(_, bytes)| bytes)
    }
}

/// Read the file at `path` and check that it hashes to `hash`; return the open file and its bytes.
fn read_verified(path: &Path, hash: &BlobHash) -> Result<(File, Vec<u8>), BlobError> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(BlobError::Missing { hash: *hash })
        }
        Err(source) => return Err(io_error(path, source)),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    let actual = BlobHash::of(&bytes);
    if actual != *hash {
        return Err(BlobError::Corrupt {
            path: path.to_path_buf(),
            expected: *hash,
            actual,
        });
    }
    Ok((file, bytes))
}

/// A temporary file this process created in `blobs/`. Removed on drop unless it was published.
struct TempFile {
    path: PathBuf,
    file: File,
    published: bool,
}

impl TempFile {
    fn create(dir: &Path) -> Result<Self, BlobError> {
        loop {
            let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!("{TEMP_PREFIX}{}-{n}", std::process::id()));
            // create_new never opens an existing file, so a temporary file left by a crashed
            // process that had the same pid is skipped, not reused.
            match File::options().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file,
                        published: false,
                    })
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => return Err(io_error(&path, source)),
            }
        }
    }

    fn write(&self, bytes: &[u8]) -> Result<(), BlobError> {
        let mut file = &self.file;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|source| io_error(&self.path, source))
    }

    /// Atomically rename the complete file onto `dest`.
    fn publish(mut self, dest: &Path) -> Result<(), BlobError> {
        fs::rename(&self.path, dest).map_err(|source| io_error(dest, source))?;
        self.published = true;
        Ok(())
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if !self.published {
            // Best effort: the error being returned to the caller is the one that matters.
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn sync_dir(dir: &Path) -> Result<(), BlobError> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|source| io_error(dir, source))
}

fn io_error(path: &Path, source: io::Error) -> BlobError {
    BlobError::Io {
        path: path.to_path_buf(),
        source,
    }
}
