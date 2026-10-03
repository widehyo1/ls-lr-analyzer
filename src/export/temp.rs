use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::Result;

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

pub struct Staging(pub PathBuf);

impl Staging {
    pub fn new(parent: &Path) -> Result<Self> {
        for _ in 0..100 {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(".ls-lr-analyzer-{}-{id}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
                Err(e) => return Err(e.into()),
            }
        }
        Err("Could not create a unique export staging directory".into())
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        // Only the directory created by this invocation is removed.
        let _ = fs::remove_dir_all(&self.0);
    }
}
