#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;

use anyhow::{bail, Context, Result};
use sightocr::{
    config::Config,
    platform,
    worker::{self, Engine, Output, Request, Task},
};
use std::{io::Write, path::PathBuf};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
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
    let gui = args.is_empty() || args == ["--silent"];
    let result = if gui {
        app::run(args == ["--silent"])
    } else {
        // SAFETY: Attaches only to an existing parent console, creates no window.
        unsafe {
            windows_sys::Win32::System::Console::AttachConsole(
                windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
            );
        }
        cli(args)
    };
    if let Err(error) = result {
        eprintln!("SightOCR: {error:#}");
        if gui {
            show_error(&format!("{error:#}"));
        }
        std::process::exit(1);
    }
}

fn cli(mut args: Vec<String>) -> Result<()> {
    if args[0] == "--help" || args[0] == "-h" {
        println!("SightOCR {}\n\n双击启动桌面界面。\n  --ocr IMAGE [--table] [--output FILE]\n  --translate TEXT [--from LANG] [--to LANG] [--output FILE]\n  --version\n\n--ocr 始终在本机识别，不上传图片。LANG 使用 en、zh-Hans、ja 等语言代码。\n设置文件可通过 SIGHTOCR_CONFIG 指定；模型目录可通过 SIGHTOCR_RESOURCES 指定。", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args[0] == "--version" {
        println!("SightOCR {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let output_path = take_option(&mut args, "--output")?.map(PathBuf::from);
    let source = take_option(&mut args, "--from")?;
    let target = take_option(&mut args, "--to")?;
    let table = if let Some(index) = args.iter().position(|s| s == "--table") {
        args.remove(index);
        true
    } else {
        false
    };
    anyhow::ensure!(args.len() == 2, "参数错误，运行 --help 查看用法");
    let (mut config, _) = if args[0] == "--ocr" {
        (Config::default(), PathBuf::new())
    } else {
        Config::load()?
    };
    if let Some(source) = source {
        config.source_lang = source;
    }
    if let Some(target) = target {
        config.target_lang = target;
    }
    config.normalize()?;
    let task = match args[0].as_str() {
        "--ocr" => {
            config.last_ocr_selection = if table { "默认_table" } else { "默认" }.into();
            Task::Recognize {
                image: worker::load_image(&PathBuf::from(&args[1]))?,
                translate: false,
            }
        }
        "--translate" if !table => Task::Translate(args[1].clone()),
        _ => bail!("未知命令，运行 --help 查看用法"),
    };
    platform::set_dpi_awareness();
    let mut engine = Engine::new(worker::resources_dir());
    let mut output = Output::default();
    engine.execute(
        Request {
            id: 1,
            config,
            task,
        },
        &mut output,
    )?;
    let text = output
        .translated
        .or(output.recognized)
        .context("没有输出")?;
    if let Some(path) = output_path {
        std::fs::write(path, text.as_bytes()).context("无法写入输出文件")?;
    } else {
        writeln!(std::io::stdout().lock(), "{text}")?;
    }
    Ok(())
}

fn take_option(args: &mut Vec<String>, flag: &str) -> Result<Option<String>> {
    if let Some(index) = args.iter().position(|s| s == flag) {
        anyhow::ensure!(index + 1 < args.len(), "{flag} 缺少参数");
        args.remove(index);
        Ok(Some(args.remove(index)))
    } else {
        Ok(None)
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
