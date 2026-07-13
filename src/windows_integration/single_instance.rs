use std::io;
use std::iter;
use std::ptr;

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows_sys::Win32::System::Threading::CreateMutexW;

const INSTANCE_MUTEX_NAME: &str = "Local\\MMB.SingleInstance";

pub struct SingleInstanceGuard {
    handle: HANDLE,
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

pub fn acquire_single_instance() -> io::Result<Option<SingleInstanceGuard>> {
    acquire_named_instance(INSTANCE_MUTEX_NAME)
}

fn acquire_named_instance(name: &str) -> io::Result<Option<SingleInstanceGuard>> {
    let wide_name: Vec<u16> = name.encode_utf16().chain(iter::once(0)).collect();
    let handle = unsafe { CreateMutexW(ptr::null(), 0, wide_name.as_ptr()) };
    let error = unsafe { GetLastError() };

    if handle.is_null() {
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    if error == ERROR_ALREADY_EXISTS {
        unsafe {
            CloseHandle(handle);
        }
        return Ok(None);
    }

    Ok(Some(SingleInstanceGuard { handle }))
}

#[cfg(test)]
mod tests {
    use super::acquire_named_instance;

    #[test]
    fn a_named_mutex_allows_only_one_instance() {
        let name = format!("Local\\MMB.Test.SingleInstance.{}", std::process::id());
        let first = acquire_named_instance(&name)
            .expect("first mutex acquisition should succeed")
            .expect("first mutex acquisition should own the mutex");
        let second = acquire_named_instance(&name).expect("second mutex acquisition should work");

        assert!(second.is_none());
        drop(first);
        assert!(
            acquire_named_instance(&name)
                .expect("mutex should be reusable after release")
                .is_some()
        );
    }
}
