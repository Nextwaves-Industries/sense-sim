//! The one signal that ends the service.
//!
//! A Win32 event rather than an atomic flag, so a pipe thread can wait on it and on its I/O
//! at the same time. That's what keeps a blocked `ConnectNamedPipe` from holding up a stop.

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::ptr::null;
use std::sync::Arc;
use std::time::Duration;

use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
use windows_sys::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForSingleObject};

#[derive(Clone)]
pub struct Stop(Arc<OwnedHandle>);

impl Stop {
    pub fn new() -> io::Result<Self> {
        Ok(Self(Arc::new(manual_reset_event()?)))
    }

    pub fn signal(&self) {
        unsafe { SetEvent(self.raw()) };
    }

    pub fn is_set(&self) -> bool {
        self.wait(Some(Duration::ZERO))
    }

    /// True once signalled. With a limit, false if that time passed first.
    pub fn wait(&self, limit: Option<Duration>) -> bool {
        let ms = limit.map_or(INFINITE, |d| d.as_millis().min(u128::from(INFINITE - 1)) as u32);
        unsafe { WaitForSingleObject(self.raw(), ms) == WAIT_OBJECT_0 }
    }

    pub(crate) fn raw(&self) -> RawHandle {
        self.0.as_raw_handle()
    }
}

pub(crate) fn manual_reset_event() -> io::Result<OwnedHandle> {
    let event = unsafe { CreateEventW(null(), 1, 0, null()) };
    if event.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(event) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_copy_sees_the_signal() {
        let stop = Stop::new().unwrap();
        let copy = stop.clone();
        assert!(!copy.is_set());
        assert!(!copy.wait(Some(Duration::from_millis(1))));
        stop.signal();
        assert!(copy.is_set());
        assert!(copy.wait(None));
    }
}
