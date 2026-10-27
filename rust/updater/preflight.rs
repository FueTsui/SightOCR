//! Read-only check before the GUI exits or hands an update to the helper.

use anyhow::{ensure, Result};
use std::path::Path;

/// Check only MCP/legacy CLI executables in this installation. Setup repeats
/// the check after handover, since a client can start another server meanwhile.
pub fn check_installation_ready(directory: &Path) -> Result<()> {
    ensure!(
        directory.is_absolute() && directory.is_dir(),
        "无法确定 SightOCR 的安装目录"
    );
    #[cfg(windows)]
    {
        let blockers = running_consoles(directory)?;
        ensure!(
            blockers.is_empty(),
            "此安装目录的 SightOCR MCP 服务正在运行（PID：{}）。请在 MCP 客户端停止服务后重试更新",
            blockers
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join("、")
        );
    }
    Ok(())
}

#[cfg(windows)]
fn console_in_directory(directory: &Path, image: &Path) -> bool {
    let Some(name) = image.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if !["sightocr-mcp.exe", "sightocr-cli.exe"]
        .iter()
        .any(|wanted| name.eq_ignore_ascii_case(wanted))
    {
        return false;
    }
    match (
        directory.canonicalize(),
        image.parent().map(Path::canonicalize),
    ) {
        (Ok(directory), Some(Ok(parent))) => directory
            .to_string_lossy()
            .eq_ignore_ascii_case(&parent.to_string_lossy()),
        _ => false,
    }
}

#[cfg(windows)]
fn running_consoles(directory: &Path) -> Result<Vec<u32>> {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, WAIT_OBJECT_0},
        System::{
            ProcessStatus::K32EnumProcesses,
            Threading::{
                OpenProcess, QueryFullProcessImageNameW, WaitForSingleObject,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            },
        },
    };
    let mut ids = vec![0u32; 8192];
    let mut bytes = 0;
    // SAFETY: The vector is writable for the specified byte count.
    let ok = unsafe { K32EnumProcesses(ids.as_mut_ptr(), (ids.len() * 4) as u32, &mut bytes) };
    ensure!(
        ok != 0 && (bytes as usize) < ids.len() * 4,
        "无法检查正在运行的 MCP 服务，请稍后重试"
    );
    let mut blockers = Vec::new();
    let mut name = vec![0u16; 32768];
    for &pid in &ids[..bytes as usize / 4] {
        // SAFETY: Read/synchronize access only. No process is stopped or changed.
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
        if handle.is_null() {
            continue;
        }
        let mut length = name.len() as u32;
        // SAFETY: The handle is valid, and the buffer is writable for `length` units.
        let queried =
            unsafe { QueryFullProcessImageNameW(handle, 0, name.as_mut_ptr(), &mut length) };
        if queried != 0 {
            let image = std::path::PathBuf::from(OsString::from_wide(&name[..length as usize]));
            // SAFETY: The open handle includes synchronization access; zero never blocks.
            let exited = unsafe { WaitForSingleObject(handle, 0) } == WAIT_OBJECT_0;
            if !exited && console_in_directory(directory, &image) {
                blockers.push(pid);
            }
        }
        // SAFETY: Close this owned process handle exactly once.
        unsafe { CloseHandle(handle) };
    }
    blockers.sort_unstable();
    Ok(blockers)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command, Stdio};

    struct TestProcess(Child);
    impl Drop for TestProcess {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn preflight_blocks_only_the_running_console_in_this_installation() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let installed = directory.path().join("安装 目录");
        let other = directory.path().join("另一安装");
        std::fs::create_dir_all(&installed)?;
        std::fs::create_dir_all(&other)?;
        check_installation_ready(&installed)?;
        let console = installed.join("sightocr-mcp.exe");
        // A renamed Windows command interpreter waits for piped stdin. It never
        // reads SightOCR configuration, opens a service or launches child work.
        let cmd = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32/cmd.exe");
        std::fs::copy(cmd, &console)?;
        let process = TestProcess(
            Command::new(&console)
                .args(["/d", "/q"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(0x0800_0000)
                .spawn()?,
        );
        let error = check_installation_ready(&installed)
            .unwrap_err()
            .to_string();
        assert!(error.contains("MCP 服务正在运行"));
        assert!(error.contains(&process.0.id().to_string()));
        check_installation_ready(&other)?;
        assert!(!console_in_directory(
            &installed,
            &installed.join("SightOCR.exe")
        ));
        assert!(console_in_directory(
            &installed,
            &installed.join("sightocr-cli.exe")
        ));
        drop(process);
        check_installation_ready(&installed)?;
        Ok(())
    }
}
