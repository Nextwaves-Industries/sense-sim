//! One line per fact, to stderr in console mode and to a file under the service. A service
//! has no console, so without the file a refusal at startup would leave no explanation.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use crate::clock;

const KEEP_BYTES: u64 = 10 * 1024 * 1024;

static FILE: OnceLock<Mutex<File>> = OnceLock::new();

/// Sends everything from now on to `path`. The previous log is kept once as `<path>.old`,
/// so the file can't grow without limit on a machine nobody looks at.
pub fn to_file(path: &Path) -> io::Result<()> {
    if fs::metadata(path).is_ok_and(|m| m.len() > KEEP_BYTES) {
        fs::rename(path, path.with_extension("log.old"))?;
    }
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let _ = FILE.set(Mutex::new(file));
    Ok(())
}

pub fn write(message: fmt::Arguments<'_>) {
    let line = format!("{} {message}\n", clock::timestamp(clock::now_ms()));
    match FILE.get() {
        Some(file) => {
            let mut file = file.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let _ = file.write_all(line.as_bytes());
        }
        None => eprint!("{line}"),
    }
}

#[macro_export]
macro_rules! say {
    ($($message:tt)*) => { $crate::logging::write(format_args!($($message)*)) };
}
