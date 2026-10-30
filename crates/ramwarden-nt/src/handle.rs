use windows::Win32::Foundation::{CloseHandle, HANDLE};

/// An owned kernel handle, closed on drop.
///
/// Every OS resource RamWarden opens on Windows is held through one of these,
/// so no code path can leak a handle by returning early.
#[derive(Debug)]
pub struct Handle(HANDLE);

// SAFETY: a kernel handle is a process-wide token; the kernel serialises
// operations on the object it names, so moving or sharing it across threads is
// sound.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Handle {
    /// Take ownership of a raw handle. Returns `None` for null or invalid.
    pub fn new(raw: HANDLE) -> Option<Self> {
        if raw.is_invalid() || raw.0.is_null() {
            None
        } else {
            Some(Handle(raw))
        }
    }

    pub fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: we own the handle and nothing else closes it.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
