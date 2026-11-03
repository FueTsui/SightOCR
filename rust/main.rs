#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod elevation;
mod legacy_cache;

use sightocr::worker;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // A fixed-purpose helper changes only this executable's installation marker.
    if args.len() == 2 && args[0] == "--set-run-as-admin" {
        let enabled = match args[1].as_str() {
            "on" => true,
            "off" => false,
            _ => std::process::exit(2),
        };
        if elevation::write_current_setting(enabled).is_err() {
            std::process::exit(1);
        }
        return;
    }
    // A private child owns blocking network/native OCR work. Never attach a
    // console, start another UI or write credentials/diagnostics on this path.
    if args.len() == 2 && args[0] == worker::SUBPROCESS_ARG {
        if worker::run_subprocess(PathBuf::from(&args[1])).is_err() {
            std::process::exit(1);
        }
        return;
    }
    // The copied updater runs outside the installation directory, so waiting
    // for Setup never keeps the application's executable locked in place.
    if args.len() == 2 && args[0] == "--apply-update" {
        if let Err(error) = sightocr::updater::apply_update(std::path::Path::new(&args[1])) {
            show_error(&format!("自动更新未完成：{error:#}"));
            std::process::exit(1);
        }
        return;
    }
    #[cfg(debug_assertions)]
    if args.len() == 2 && args[0] == "--smoke-ui" {
        if let Err(error) = app::run_smoke(PathBuf::from(&args[1])) {
            eprintln!("UI smoke test failed: {error:#}");
            std::process::exit(1);
        }
        return;
    }
    if !args.is_empty() && args != ["--silent"] {
        std::process::exit(2);
    }
    match elevation::relaunch_if_requested(args == ["--silent"]) {
        Ok(true) => return,
        Ok(false) => {}
        Err(error) => {
            show_error(&format!("无法以管理员身份启动：{error:#}"));
            std::process::exit(1);
        }
    }
    if let Err(error) = app::run(args == ["--silent"]) {
        show_error(&format!("{error:#}"));
        std::process::exit(1);
    }
}

fn show_error(message: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let message: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    let title: Vec<u16> = "SightOCR".encode_utf16().chain(Some(0)).collect();
    // SAFETY: Both UTF-16 strings are NUL-terminated and live for the call.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}
