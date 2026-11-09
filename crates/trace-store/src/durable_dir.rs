//! Creating a run directory so that it survives a crash: the [`crate::BlobStore`] contract needs
//! the run directory and its entry in its parent to be on disk before blobs are stored in it.
//!
//! A directory that exists is not thereby durable: a concurrent creator may have made it and not
//! flushed its parent yet. So `create` does not infer durability from existence. It flushes every
//! directory on the run directory's canonical path, from the run directory up to its filesystem
//! anchor, whether this call created it or not.
//!
//! The anchor is the topmost directory of that path on the run directory's filesystem: the next
//! directory up is on another device, or there is none (`/`). The anchor is flushed, which records
//! its child's entry. Its own entry lives in the parent filesystem as a mount point, which must
//! exist before the mount and is the system's to keep. Creating directories never crosses a
//! filesystem boundary, so every directory this or any racing call created sits below the anchor
//! on the same device, and its parent is flushed. A symlink on the given path is resolved and the
//! physical directories are flushed; the link's own entry is taken as given, as this call never
//! creates links.
//!
//! Each directory up to the anchor must be readable by this process, since flushing one means
//! opening it. An unreadable one (an execute-only `0711` ancestor, say) is an error naming it, not
//! a silent skip.

use std::fs::{self, File};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Create `dir` and any missing ancestors, then flush every directory on its canonical path up to
/// its filesystem anchor (see the module docs). On error, returns the path that failed.
pub(crate) fn create(dir: &Path) -> Result<(), (PathBuf, io::Error)> {
    create_flushed(dir).map(drop)
}

/// [`create`], returning the directories it flushed, bottom-up, for tests.
fn create_flushed(dir: &Path) -> Result<Vec<PathBuf>, (PathBuf, io::Error)> {
    fs::create_dir_all(dir).map_err(|e| (dir.to_path_buf(), e))?;
    let real = fs::canonicalize(dir).map_err(|e| (dir.to_path_buf(), e))?;
    let mut flushed = Vec::new();
    let mut cur = real.as_path();
    let dev = device(cur)?;
    loop {
        sync(cur)?;
        flushed.push(cur.to_path_buf());
        match cur.parent() {
            Some(parent) if device(parent)? == dev => cur = parent,
            _ => break,
        }
    }
    Ok(flushed)
}

fn device(dir: &Path) -> Result<u64, (PathBuf, io::Error)> {
    fs::metadata(dir)
        .map(|m| m.dev())
        .map_err(|e| (dir.to_path_buf(), e))
}

fn sync(dir: &Path) -> Result<(), (PathBuf, io::Error)> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| (dir.to_path_buf(), e))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::thread;

    use super::*;

    /// `flushed` is `dir`'s canonical path and every ancestor up to the filesystem anchor.
    fn assert_full_chain(dir: &Path, flushed: &[PathBuf]) {
        let real = fs::canonicalize(dir).unwrap();
        let dev = fs::metadata(&real).unwrap().dev();
        let expected: Vec<PathBuf> = real.ancestors().map(Path::to_path_buf).collect();
        assert_eq!(flushed, &expected[..flushed.len()], "not a bottom-up chain");
        let anchor = flushed.last().unwrap();
        assert!(
            anchor
                .parent()
                .is_none_or(|p| fs::metadata(p).unwrap().dev() != dev),
            "stopped below the anchor at {}",
            anchor.display()
        );
    }

    #[test]
    fn creates_nested_missing_directories_and_flushes_up_to_the_anchor() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("runs").join("r1");
        let flushed = create_flushed(&dir).unwrap();
        assert!(dir.is_dir());
        assert_full_chain(&dir, &flushed);
        assert!(flushed.contains(&fs::canonicalize(tmp.path()).unwrap()));
    }

    #[test]
    fn existing_components_are_flushed_as_if_new() {
        // Another creator made `base/new-runs/r1` and has not flushed anything: every level,
        // fully or partly pre-existing, must still be flushed by this call.
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("base");
        let full = base.join("new-runs").join("r1");
        fs::create_dir_all(&full).unwrap();
        let partial = base.join("other").join("r2");
        fs::create_dir_all(base.join("other")).unwrap();
        for dir in [&full, &partial] {
            let flushed = create_flushed(dir).unwrap();
            assert_full_chain(dir, &flushed);
            assert!(flushed.contains(&fs::canonicalize(&base).unwrap()));
        }
    }

    #[test]
    fn concurrent_initializers_all_succeed_and_each_flushes_the_full_chain() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("base").join("new-runs").join("r1");
        let n = 8;
        let barrier = Arc::new(Barrier::new(n));
        let handles: Vec<_> = (0..n)
            .map(|_| {
                let (dir, barrier) = (dir.clone(), barrier.clone());
                thread::spawn(move || {
                    barrier.wait();
                    create_flushed(&dir)
                })
            })
            .collect();
        for h in handles {
            let flushed = h.join().unwrap().unwrap();
            assert_full_chain(&dir, &flushed);
        }
    }

    #[test]
    fn a_symlinked_ancestor_flushes_the_physical_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let dir = link.join("runs").join("r1");
        let flushed = create_flushed(&dir).unwrap();
        assert_full_chain(&dir, &flushed);
        let real = fs::canonicalize(&real).unwrap();
        assert_eq!(flushed[0], real.join("runs").join("r1"));
        assert!(flushed.contains(&real));
        assert!(!flushed.iter().any(|p| p.ends_with("link")));
    }

    #[test]
    fn a_file_in_the_way_is_an_error_naming_the_path() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("runs");
        fs::write(&file, b"x").unwrap();
        let (path, _) = create(&file.join("r1")).unwrap_err();
        assert!(path.starts_with(&file), "{}", path.display());
    }
}
