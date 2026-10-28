//! Native index ownership must exclude another backend while leaving file bytes readable for updates.
use anyhow::{Context, Result};
use std::path::Path;

/// One backend owns one canonical index until its wrapper exits. Windows releases abandoned mutexes
/// after a forced job shutdown, so no stale PID or locked snapshot file can resurrect ownership.
pub(crate) struct IndexGuard {
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
    #[cfg(not(windows))]
    _file: std::fs::File,
}

impl IndexGuard {
    /// Acquire `directory` without waiting for another backend. The directory must already exist;
    /// contention returns a visible error before copying configuration or starting the child JVM.
    pub(crate) fn acquire(directory: &Path) -> Result<Self> {
        let directory = directory
            .canonicalize()
            .context("Invalid private Java index")?;
        #[cfg(windows)]
        {
            use sha2::{Digest, Sha256};
            use windows_sys::Win32::{
                Foundation::{CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT},
                System::Threading::{CreateMutexW, WaitForSingleObject},
            };
            let identity = format!(
                "{:x}",
                Sha256::digest(directory.to_string_lossy().to_lowercase().as_bytes())
            );
            let name: Vec<u16> = format!("Local\\Nanobug.Java.Index.{identity}")
                .encode_utf16()
                .chain(Some(0))
                .collect();
            // The OS gate is process-owned metadata; it holds no byte-range lock in copied data.
            let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
            anyhow::ensure!(
                !handle.is_null(),
                "Cannot create Java index owner: {}",
                std::io::Error::last_os_error()
            );
            match unsafe { WaitForSingleObject(handle, 0) } {
                WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Self { handle }),
                outcome => {
                    let error = std::io::Error::last_os_error();
                    unsafe {
                        CloseHandle(handle);
                    }
                    if outcome == WAIT_TIMEOUT {
                        anyhow::bail!("The private Java index already has a live backend");
                    }
                    anyhow::bail!("Cannot acquire Java index owner: {error}");
                }
            }
        }
        #[cfg(not(windows))]
        {
            // POSIX advisory locks do not prevent snapshot reads. Windows uses the named gate above.
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(directory.join(".nanobug-owner"))?;
            file.try_lock()
                .context("The private Java index already has a live backend")?;
            Ok(Self { _file: file })
        }
    }
}

#[cfg(windows)]
impl Drop for IndexGuard {
    /// The owning main thread retains this guard through child exit, including initialization errors.
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::System::Threading::ReleaseMutex(self.handle);
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The OS gate excludes a concurrent owner, permits an independent index and releases on drop.
    #[test]
    fn index_ownership_excludes_another_thread_and_releases_after_drop() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let owner = IndexGuard::acquire(first.path()).unwrap();
        let path = first.path().to_owned();
        assert!(
            std::thread::spawn(move || IndexGuard::acquire(&path).is_err())
                .join()
                .unwrap()
        );
        let other = IndexGuard::acquire(second.path()).unwrap();
        drop(other);
        drop(owner);
        assert!(IndexGuard::acquire(first.path()).is_ok());
    }
}
