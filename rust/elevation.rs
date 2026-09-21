use anyhow::{Context, Result};
use std::{mem::size_of, os::windows::ffi::OsStrExt, ptr};
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_CANCELLED},
    Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY},
    System::Threading::{GetCurrentProcess, OpenProcessToken},
    UI::{
        Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW},
        WindowsAndMessaging::SW_SHOWNORMAL,
    },
};

fn is_elevated() -> Result<bool> {
    let mut token = ptr::null_mut();
    // SAFETY: The process pseudo-handle is valid and token is writable storage.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(std::io::Error::last_os_error()).context("无法读取进程权限");
    }
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut length = 0;
    // SAFETY: The opened token is live and the buffer has the specified size.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut length,
        )
    };
    let error = std::io::Error::last_os_error();
    // SAFETY: This function owns the token and closes it exactly once.
    unsafe { CloseHandle(token) };
    if ok == 0 {
        return Err(error).context("无法检查管理员权限");
    }
    Ok(elevation.TokenIsElevated != 0)
}

/// Runs before any window, hotkey, or single-instance registration. Worker
/// processes inherit the GUI token; MCP and the copied updater do not use this.
pub fn relaunch_if_requested(silent: bool) -> Result<bool> {
    let executable = std::env::current_exe()?;
    let setting = executable.with_file_name("run-as-admin");
    let enabled = match std::fs::read_to_string(setting) {
        Ok(value) => value.trim() == "1",
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error).context("无法读取管理员启动设置"),
    };
    if !enabled || is_elevated()? {
        return Ok(false);
    }
    let path: Vec<u16> = executable
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let verb: Vec<u16> = "runas\0".encode_utf16().collect();
    let arguments: Vec<u16> = if silent { "--silent\0" } else { "\0" }
        .encode_utf16()
        .collect();
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC,
        lpVerb: verb.as_ptr(),
        lpFile: path.as_ptr(),
        lpParameters: arguments.as_ptr(),
        nShow: SW_SHOWNORMAL,
        // SAFETY: All omitted ShellExecute fields support zero/null defaults.
        ..unsafe { std::mem::zeroed() }
    };
    // SAFETY: All UTF-16 buffers remain alive for the synchronous Shell call.
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_CANCELLED as i32) {
            return Ok(true);
        }
        return Err(error).context("Windows 管理员启动失败");
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    #[test]
    fn current_process_token_can_be_queried() {
        super::is_elevated().expect("current process token");
    }
}
