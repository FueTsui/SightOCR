use anyhow::{ensure, Context, Result};
use std::{io::Write, mem::size_of, os::windows::ffi::OsStrExt, path::Path, ptr};
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_CANCELLED, WAIT_OBJECT_0},
    Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY},
    System::Threading::{
        GetCurrentProcess, GetExitCodeProcess, OpenProcessToken, WaitForSingleObject, INFINITE,
    },
    UI::{
        Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW},
        WindowsAndMessaging::{SW_HIDE, SW_SHOWNORMAL},
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

fn read_setting(path: &Path) -> Result<bool> {
    match std::fs::read_to_string(path) {
        Ok(value) => Ok(value.trim() == "1"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("无法读取管理员启动设置"),
    }
}

pub fn is_enabled() -> Result<bool> {
    read_setting(&std::env::current_exe()?.with_file_name("run-as-admin"))
}

fn write_setting(path: &Path, enabled: bool) -> Result<()> {
    if enabled {
        let mut temporary =
            tempfile::NamedTempFile::new_in(path.parent().context("安装目录无效")?)?;
        temporary.write_all(b"1\r\n")?;
        temporary.as_file().sync_all()?;
        temporary.persist(path).map_err(|error| error.error)?;
    } else if let Err(error) = std::fs::remove_file(path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error.into());
        }
    }
    Ok(())
}

pub fn write_current_setting(enabled: bool) -> Result<()> {
    write_setting(
        &std::env::current_exe()?.with_file_name("run-as-admin"),
        enabled,
    )
}

pub fn set_enabled(enabled: bool) -> Result<()> {
    if is_enabled()? == enabled {
        return Ok(());
    }
    match write_current_setting(enabled) {
        Ok(()) => return Ok(()),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied) => {}
        Err(error) => return Err(error).context("无法保存管理员运行选项"),
    }
    let path: Vec<u16> = std::env::current_exe()?
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let verb: Vec<u16> = "runas\0".encode_utf16().collect();
    let arguments: Vec<u16> = if enabled {
        "--set-run-as-admin on\0"
    } else {
        "--set-run-as-admin off\0"
    }
    .encode_utf16()
    .collect();
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | SEE_MASK_NOCLOSEPROCESS,
        lpVerb: verb.as_ptr(),
        lpFile: path.as_ptr(),
        lpParameters: arguments.as_ptr(),
        nShow: SW_HIDE,
        // SAFETY: Omitted optional fields accept zero/null defaults.
        ..unsafe { std::mem::zeroed() }
    };
    // SAFETY: UTF-16 buffers and the structure remain live throughout the call.
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context("管理员运行选项未保存，权限请求被取消或失败");
    }
    ensure!(!info.hProcess.is_null(), "无法等待管理员设置保存完成");
    let mut code = 1;
    // SAFETY: The shell returned an owned process handle; wait before querying its exit code.
    let ok = unsafe {
        WaitForSingleObject(info.hProcess, INFINITE) == WAIT_OBJECT_0
            && GetExitCodeProcess(info.hProcess, &mut code) != 0
    };
    // SAFETY: Close the owned handle exactly once.
    unsafe { CloseHandle(info.hProcess) };
    ensure!(ok && code == 0, "管理员运行选项保存失败");
    ensure!(is_enabled()? == enabled, "管理员运行选项保存后校验失败");
    Ok(())
}

/// Runs before any window, hotkey, or single-instance registration. Worker
/// processes inherit the GUI token; MCP and the copied updater do not use this.
pub fn relaunch_if_requested(silent: bool) -> Result<bool> {
    let executable = std::env::current_exe()?;
    let enabled = is_enabled()?;
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
    fn config_does_not_persist_installation_admin_marker() -> anyhow::Result<()> {
        let config: sightocr::config::Config = serde_json::from_str(r#"{"run_as_admin":true}"#)?;
        let config = sightocr::config::Config {
            run_as_admin: true,
            ..config
        };
        assert!(serde_json::to_value(config)?.get("run_as_admin").is_none());
        Ok(())
    }

    #[test]
    fn installer_marker_roundtrip_and_disable_removes_file() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("run-as-admin");
        assert!(!super::read_setting(&path)?);
        super::write_setting(&path, true)?;
        assert!(super::read_setting(&path)?);
        super::write_setting(&path, true)?;
        super::write_setting(&path, false)?;
        assert!(!path.exists());
        super::write_setting(&path, false)?;
        Ok(())
    }

    #[test]
    fn marker_write_failure_preserves_existing_setting() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("run-as-admin");
        std::fs::create_dir(&path)?;
        std::fs::write(path.join("sentinel"), b"keep")?;
        assert!(super::write_setting(&path, true).is_err());
        assert_eq!(std::fs::read(path.join("sentinel"))?, b"keep");
        Ok(())
    }
    #[test]
    fn current_process_token_can_be_queried() {
        super::is_elevated().expect("current process token");
    }
}
