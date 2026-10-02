//! Liveness and process-group signals for processes this CLI started.
//!
//! These helpers never search the process table. Callers pass a pid they
//! stored when they spawned the child.

pub fn process_is_running(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        // `kill(pid, 0)` checks existence. EPERM means it exists and is not ours.
        let rc = unsafe { libc_kill(pid as i32, 0) };
        if rc == 0 {
            return true;
        }
        return std::io::Error::last_os_error().raw_os_error() == Some(1);
    }
    #[cfg(windows)]
    {
        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        const STILL_ACTIVE: u32 = 259;
        const ERROR_ACCESS_DENIED: u32 = 5;
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                // Access denied still means some process owns the pid.
                return GetLastError() == ERROR_ACCESS_DENIED;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(handle, &mut code);
            let _ = CloseHandle(handle);
            ok != 0 && code == STILL_ACTIVE
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

/// Signal the process group whose id is `pid`.
///
/// The caller must have placed that child in its own group with
/// `process_group(0)`. Pid 0 is refused so this cannot signal the CLI group.
#[cfg(unix)]
pub fn signal_process_group(pid: u32, force: bool) -> Result<(), String> {
    if pid == 0 {
        return Err("refusing to signal process group 0".into());
    }
    // Linux and macOS: SIGTERM 15, SIGKILL 9, ESRCH 3.
    let signal = if force { 9 } else { 15 };
    let rc = unsafe { libc_kill(-(pid as i32), signal) };
    if rc == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(3) {
        return Ok(());
    }
    Err(format!(
        "failed to signal owned process group {pid}: {error}"
    ))
}

#[cfg(unix)]
unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

#[cfg(unix)]
unsafe fn libc_kill(pid: i32, sig: i32) -> i32 {
    unsafe { kill(pid, sig) }
}

#[cfg(windows)]
unsafe extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut core::ffi::c_void;
    fn GetExitCodeProcess(process: *mut core::ffi::c_void, code: *mut u32) -> i32;
    fn CloseHandle(handle: *mut core::ffi::c_void) -> i32;
    fn GetLastError() -> u32;
}

#[cfg(test)]
#[path = "process_alive/tests.rs"]
mod tests;
