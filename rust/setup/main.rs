//! SightOCR's own installer and uninstaller (replaces the former Inno Setup package).
//!
//! One executable plays every role, chosen by the data appended to it:
//! - with program files appended it is `SightOCR-Setup-<version>.exe`;
//! - with only the file list appended it is the installed `unins000.exe`;
//! - `--pack` builds a setup package during release builds.
//!
//! The command line keeps the Inno Setup switches that published SightOCR
//! versions pass when updating themselves:
//! `/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /UPDATE /DIR=<dir> /LOG=<file>
//! [/NOLAUNCH]`, with exit code 0 meaning success and the installer starting
//! SightOCR again after a successful update.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod args;
mod engine;
mod env;
mod log;
mod payload;
mod ui;
mod win;

use args::{Command, Options, Quiet};
use std::path::{Path, PathBuf};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Exit codes. 0 is the only value updaters treat as success; the others
/// follow Inno Setup's meanings so scripts written for it keep working.
pub mod exit {
    pub const OK: i32 = 0;
    pub const INIT_FAILED: i32 = 1;
    pub const CANCELLED: i32 = 2;
    pub const FATAL: i32 = 3;
    pub const PREPARE_FAILED: i32 = 7;
}

fn main() {
    let raw: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let code = match args::parse(&raw) {
        Ok(Command::Pack { source, output }) => pack(&source, &output),
        Ok(Command::Cleanup { dir }) => {
            engine::cleanup_after_uninstall(&dir);
            exit::OK
        }
        Ok(Command::Run(options)) => run(options),
        Err(error) => {
            if !args::quiet_requested(&raw) {
                win::message_box(&format!("{error:#}"), true);
            }
            exit::INIT_FAILED
        }
    };
    std::process::exit(code);
}

fn pack(source: &Path, output: &Path) -> i32 {
    match std::env::current_exe()
        .map_err(anyhow::Error::from)
        .and_then(|exe| payload::pack(&exe, source, output, VERSION))
    {
        Ok(()) => {
            println!("Setup package: {}", output.display());
            exit::OK
        }
        Err(error) => {
            eprintln!("Packing failed: {error:#}");
            exit::INIT_FAILED
        }
    }
}

fn run(options: Options) -> i32 {
    let env = env::Env::detect();
    // Elevate before anything is shown or changed. The window and every change
    // then belong to the elevated process, while this original process keeps
    // the user's normal rights to start SightOCR afterwards.
    if !options.elevated_child && env.needs_elevation() {
        return relaunch_elevated(&options);
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(_) => return fail_early(&options, "无法确定安装程序位置。"),
    };
    let package = match payload::Package::open(&exe) {
        Ok(Some(package)) => package,
        Ok(None) | Err(_) => {
            return fail_early(
                &options,
                "安装包不完整或已损坏，请重新下载 SightOCR 安装包。",
            )
        }
    };
    let uninstalling = package.index.uninstaller;
    log::init(
        options.log.clone(),
        if uninstalling { "Uninstall" } else { "Setup" },
    );
    logln!(
        "SightOCR {} {} {:?}",
        if uninstalling { "uninstaller" } else { "setup" },
        VERSION,
        options.forward
    );
    let launcher = Launcher {
        result_file: options.result_file.clone(),
    };
    let code = if uninstalling {
        let dir = exe
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        ui::uninstall(env, package, options, dir)
    } else {
        ui::install(env, package, options, launcher)
    };
    logln!("Exit code {code}");
    code
}

fn fail_early(options: &Options, message: &str) -> i32 {
    if options.quiet != Quiet::VerySilent && !options.suppress_msgboxes {
        win::message_box(message, true);
    }
    exit::INIT_FAILED
}

/// Run this executable again with administrator rights and wait for it. The
/// elevated copy reports where SightOCR was installed when it should be
/// started; this process then starts it without administrator rights.
fn relaunch_elevated(options: &Options) -> i32 {
    let Ok(exe) = std::env::current_exe() else {
        return fail_early(options, "无法确定安装程序位置。");
    };
    let result_file = std::env::temp_dir().join(format!(
        "SightOCR-Setup-{}-{}.result",
        std::process::id(),
        chrono::Local::now().format("%H%M%S%3f")
    ));
    let _ = std::fs::remove_file(&result_file);
    let mut arguments = options.forward.clone();
    arguments.push("--elevated-child".into());
    let mut result_argument = std::ffi::OsString::from("--result-file=");
    result_argument.push(&result_file);
    arguments.push(result_argument);
    let show = options.quiet != Quiet::VerySilent;
    let code = match win::run_elevated(&exe, &arguments, show) {
        Ok(code) => code,
        Err(win::ElevationError::Cancelled) => exit::CANCELLED,
        Err(win::ElevationError::Failed(error)) => {
            let _ = std::fs::remove_file(&result_file);
            return fail_early(options, &format!("无法获得管理员权限：{error}"));
        }
    };
    if code == exit::OK {
        if let Ok(dir) = std::fs::read_to_string(&result_file) {
            let dir = PathBuf::from(dir.trim());
            if dir.is_absolute() {
                engine::launch_app(&dir);
            }
        }
    }
    let _ = std::fs::remove_file(&result_file);
    code
}

/// Starts SightOCR after setup, or hands that over to the original
/// non-elevated process so SightOCR does not inherit administrator rights.
pub struct Launcher {
    result_file: Option<PathBuf>,
}

impl Launcher {
    pub fn launch(&self, dir: &Path) {
        match self
            .result_file
            .as_deref()
            .filter(|file| handover_path(file))
        {
            Some(file) => {
                if std::fs::write(file, dir.as_os_str().to_string_lossy().as_bytes()).is_err() {
                    logln!("Could not hand over the launch; starting SightOCR directly.");
                    engine::launch_app(dir);
                }
            }
            None => engine::launch_app(dir),
        }
    }
}

/// The elevated process writes only a hand-over file this program named, in
/// the user's own temp folder; any other path on the command line is ignored.
fn handover_path(file: &Path) -> bool {
    let name_ok = file
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.starts_with("SightOCR-Setup-") && name.ends_with(".result") && name.len() < 80
        });
    let parent_ok = match (
        file.parent().map(std::fs::canonicalize),
        std::fs::canonicalize(std::env::temp_dir()),
    ) {
        (Some(Ok(parent)), Ok(temp)) => parent == temp,
        _ => false,
    };
    name_ok && parent_ok
}
