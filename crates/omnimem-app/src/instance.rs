//! One running app per data folder.
//!
//! The first launch holds an exclusive lock on a file in the data folder for
//! as long as it runs; the operating system drops it if the process dies, so
//! a crash never leaves a stale lock. A second launch finds the lock held,
//! leaves a request for the running app to show its window, and exits.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;

const LOCK_FILE: &str = "omnimem.lock";
const SHOW_REQUEST: &str = "show-window.request";

/// Held by the running app; released when dropped.
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
}

#[derive(Debug)]
pub enum Instance {
    Primary(InstanceLock),
    AlreadyRunning,
}

pub fn acquire(dir: &Path) -> io::Result<Instance> {
    std::fs::create_dir_all(dir)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK_FILE))?;
    match file.try_lock() {
        Ok(()) => Ok(Instance::Primary(InstanceLock { _file: file })),
        Err(TryLockError::WouldBlock) => Ok(Instance::AlreadyRunning),
        Err(TryLockError::Error(e)) => Err(e),
    }
}

/// Ask the running app to bring its window forward.
pub fn request_show(dir: &Path) -> io::Result<()> {
    std::fs::write(dir.join(SHOW_REQUEST), b"show")
}

/// True, once, after [`request_show`].
pub fn take_show_request(dir: &Path) -> bool {
    std::fs::remove_file(dir.join(SHOW_REQUEST)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_launch_sees_the_first_and_asks_it_to_show() {
        let dir = std::env::temp_dir().join(format!("omnimem-instance-{}", std::process::id()));
        let first = acquire(&dir).unwrap();
        assert!(matches!(first, Instance::Primary(_)));
        assert!(matches!(acquire(&dir).unwrap(), Instance::AlreadyRunning));

        assert!(!take_show_request(&dir));
        request_show(&dir).unwrap();
        assert!(take_show_request(&dir));
        assert!(!take_show_request(&dir), "a request is taken once");

        drop(first);
        assert!(
            matches!(acquire(&dir).unwrap(), Instance::Primary(_)),
            "the lock goes with the app"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
