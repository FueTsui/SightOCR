//! Drive the production App through its actual Windows tray endpoint.
//! The driver posts messages only to this isolated smoke process's own HWNDs.

use super::App;
use anyhow::{ensure, Context, Result};
use eframe::egui;
use serde::Serialize;
use sightocr::config::Config;
use std::{
    path::PathBuf,
    ptr::{null, null_mut},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{
        FindWindowExW, GetGUIThreadInfo, GetWindowThreadProcessId, IsWindow, IsWindowVisible,
        PostMessageW, RegisterWindowMessageW, SendMessageTimeoutW, GUITHREADINFO, GUI_INMENUMODE,
        SMTO_ABORTIFHUNG, SMTO_BLOCK, WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_KEYDOWN, WM_KEYUP,
        WM_LBUTTONDBLCLK, WM_LBUTTONUP,
    },
};

const CYCLES: usize = 24;
const WM_TRAY: u32 = WM_APP + 2;
const SOURCE: &str = "Tray lifecycle: 原文 한국어 العربية";
const TRANSLATION: &str = "Preserve this text across every hide and restore.";

#[derive(Default, Serialize)]
struct Report {
    scope: &'static str,
    passed: bool,
    cycles: usize,
    checks: Vec<String>,
    frames: Vec<FrameObservation>,
    error: Option<String>,
}

#[derive(Serialize)]
struct FrameObservation {
    frame: u64,
    pass: usize,
    close_requested: bool,
    visible: bool,
    background_hidden: bool,
    exiting: bool,
    pending_raise: bool,
    text_preserved: bool,
}

type SharedReport = Arc<Mutex<Report>>;

pub(super) fn run(directory: PathBuf) -> Result<()> {
    std::fs::create_dir_all(&directory)?;
    let scratch = tempfile::tempdir()?;
    let path = scratch.path().join("config.json");
    let report = Arc::new(Mutex::new(Report {
        scope: "native-tray-lifecycle",
        ..Report::default()
    }));
    let app_report = report.clone();
    let (ready, receiver) = mpsc::channel::<(usize, Arc<AtomicBool>, egui::Context)>();
    let stop = Arc::new(AtomicBool::new(false));
    let driver_stop = stop.clone();
    let driver_report = report.clone();
    let driver = thread::Builder::new()
        .name("tray-lifecycle-smoke-driver".into())
        .spawn(move || {
            let Ok((hwnd, exit_requested, context)) =
                receiver.recv_timeout(Duration::from_secs(15))
            else {
                driver_report.lock().unwrap().error = Some("App did not initialize".into());
                return;
            };
            let result = exercise(hwnd, &driver_stop, &driver_report);
            if result.is_err() && owned(hwnd) {
                let tray = platform_window().ok();
                driver_report.lock().unwrap().checks.push(format!(
                    "failure_probe_main_alive=true,tray_alive={},menu_open={:?}",
                    tray.is_some(),
                    tray.and_then(|tray| menu_open(tray).ok())
                ));
                let name: Vec<u16> = sightocr::platform::MAIN_ACTIVATE_MESSAGE
                    .encode_utf16()
                    .chain(Some(0))
                    .collect();
                // SAFETY: Register this program's fixed activation message name.
                let message = unsafe { RegisterWindowMessageW(name.as_ptr()) };
                let recovered = message != 0
                    && post(hwnd, message, 0, 0).is_ok()
                    && wait_visible(hwnd, true, &driver_stop).is_ok();
                driver_report.lock().unwrap().checks.push(format!(
                    "failure_probe_independent_activation_recovered={recovered}"
                ));
            }
            {
                let mut report = driver_report.lock().unwrap();
                report.passed = result.is_ok();
                report.error = result.err().map(|error| format!("{error:#}"));
            }
            // Exercise the same explicit exit flag used by installer shutdown.
            exit_requested.store(true, Ordering::Release);
            super::super::wake_ui(&context, hwnd);
        })?;
    sightocr::platform::set_dpi_awareness();
    let result = eframe::run_native(
        "SightOCR Tray Lifecycle Smoke",
        eframe::NativeOptions {
            viewport: super::super::main_viewport(),
            ..Default::default()
        },
        Box::new(move |cc| {
            let config = Config {
                hotkey: "Ctrl+Alt+Shift+F23".into(),
                translate_hotkey: "Ctrl+Alt+Shift+F24".into(),
                silent_hotkey: "Ctrl+Alt+Shift+F22".into(),
                hide_tray_icon: false,
                ..Config::default()
            };
            let mut app = App::new(cc, config, path)?;
            app.platform
                .as_ref()
                .context("Isolated tray failed to initialize")?;
            app.source = SOURCE.into();
            app.translation = TRANSLATION.into();
            ready.send((app.hwnd, app.exit_requested.clone(), cc.egui_ctx.clone()))?;
            Ok(Box::new(TrayApp {
                app,
                report: app_report,
            }))
        }),
    )
    .map_err(|error| anyhow::anyhow!("Tray smoke renderer failed: {error}"));
    stop.store(true, Ordering::Release);
    let joined = driver.join();
    let mut report = report.lock().unwrap();
    if let Err(error) = &result {
        report.passed = false;
        report.error = Some(error.to_string());
    }
    if joined.is_err() {
        report.passed = false;
        report.error = Some("Tray driver panicked".into());
    }
    std::fs::write(
        directory.join("report.json"),
        serde_json::to_vec_pretty(&*report)?,
    )?;
    result?;
    ensure!(report.passed, "Tray lifecycle failed: {:?}", report.error);
    Ok(())
}

struct TrayApp {
    app: App,
    report: SharedReport,
}

impl eframe::App for TrayApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.app.update(ctx, frame);
        let observation = FrameObservation {
            frame: self.app.ui_frame,
            pass: ctx.output(|output| output.num_completed_passes),
            close_requested: ctx.input(|input| input.viewport().close_requested()),
            // SAFETY: App owns this HWND throughout its update callback.
            visible: unsafe { IsWindowVisible(self.app.hwnd as HWND) != 0 },
            background_hidden: self.app.background_hidden,
            exiting: self.app.exiting,
            pending_raise: self.app.raise_after_frame.is_some(),
            text_preserved: self.app.source == SOURCE && self.app.translation == TRANSLATION,
        };
        self.report.lock().unwrap().frames.push(observation);
        // Do not schedule repaint here: restore must wake an actually idle hidden App.
    }
}

fn owned(hwnd: usize) -> bool {
    let mut process = 0;
    // SAFETY: Win32 accepts opaque/stale HWNDs and writes only the local process id.
    unsafe {
        IsWindow(hwnd as HWND) != 0
            && GetWindowThreadProcessId(hwnd as HWND, &mut process) != 0
            && process == std::process::id()
    }
}

fn post(hwnd: usize, message: u32, wparam: usize, lparam: isize) -> Result<()> {
    ensure!(owned(hwnd), "Smoke HWND disappeared or changed ownership");
    ensure!(
        // SAFETY: Only scalar messages target this smoke process's verified HWND.
        unsafe { PostMessageW(hwnd as HWND, message, wparam, lparam) } != 0,
        "Cannot post smoke message {message:#x}"
    );
    Ok(())
}

fn close_delivered(hwnd: usize) -> Result<()> {
    ensure!(owned(hwnd), "Smoke main window disappeared");
    let mut result = 0;
    // Synchronize with native delivery, not an arbitrary cross-thread delay.
    // SAFETY: WM_CLOSE has no pointer payload and targets this smoke's own HWND.
    let delivered = unsafe {
        SendMessageTimeoutW(
            hwnd as HWND,
            WM_CLOSE,
            0,
            0,
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            4000,
            &mut result,
        )
    };
    ensure!(delivered != 0, "Main window did not receive close");
    Ok(())
}

fn platform_window() -> Result<usize> {
    let class: Vec<u16> = "SightOCR.PlatformWindow"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut previous = null_mut();
    loop {
        // SAFETY: Enumerate matching top-level classes and verify ownership before use.
        previous = unsafe { FindWindowExW(null_mut(), previous, class.as_ptr(), null()) };
        ensure!(
            !previous.is_null(),
            "Cannot find the isolated platform endpoint"
        );
        if owned(previous as usize) {
            return Ok(previous as usize);
        }
    }
}

fn wait_until(
    stop: &AtomicBool,
    label: &str,
    mut predicate: impl FnMut() -> Result<bool>,
) -> Result<()> {
    let start = Instant::now();
    loop {
        ensure!(!stop.load(Ordering::Acquire), "App exited while {label}");
        if predicate()? {
            return Ok(());
        }
        ensure!(
            start.elapsed() < Duration::from_secs(4),
            "Timed out while {label}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_visible(hwnd: usize, visible: bool, stop: &AtomicBool) -> Result<()> {
    let mut since = None;
    wait_until(stop, if visible { "restoring" } else { "hiding" }, || {
        ensure!(owned(hwnd), "Main window was destroyed");
        // SAFETY: The preceding ownership check validates this HWND.
        if unsafe { IsWindowVisible(hwnd as HWND) != 0 } != visible {
            since = None;
            return Ok(false);
        }
        Ok(since.get_or_insert_with(Instant::now).elapsed() >= Duration::from_millis(90))
    })
}

fn menu_open(hwnd: usize) -> Result<bool> {
    ensure!(owned(hwnd), "Platform HWND was destroyed");
    // SAFETY: GUITHREADINFO is POD, cbSize is initialized, and its thread owns our HWND.
    unsafe {
        let mut info: GUITHREADINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
        let thread = GetWindowThreadProcessId(hwnd as HWND, null_mut());
        ensure!(
            GetGUIThreadInfo(thread, &mut info) != 0,
            "Cannot inspect menu thread"
        );
        Ok(info.flags & GUI_INMENUMODE != 0)
    }
}

fn exercise(hwnd: usize, stop: &AtomicBool, report: &SharedReport) -> Result<()> {
    let tray = platform_window()?;
    wait_visible(hwnd, true, stop)?;
    for cycle in 0..CYCLES {
        report
            .lock()
            .unwrap()
            .checks
            .push(format!("cycle_{cycle:02}_start"));
        post(hwnd, WM_CLOSE, 0, 0)?;
        wait_visible(hwnd, false, stop)?;
        let events: &[u32] = match cycle % 3 {
            0 => &[WM_LBUTTONUP],
            1 => &[WM_LBUTTONUP, WM_LBUTTONDBLCLK, WM_LBUTTONUP],
            _ => &[WM_LBUTTONUP; 12],
        };
        for &event in events {
            post(tray, WM_TRAY, 1, event as isize)?;
        }
        wait_visible(hwnd, true, stop).with_context(|| format!("Cycle {cycle} tray click"))?;
        if cycle % 4 == 0 {
            // Confirm that the main HWND received close before the platform
            // thread sends Show; posts to two different queues have no ordering.
            close_delivered(hwnd)?;
            post(tray, WM_TRAY, 1, WM_LBUTTONUP as isize)?;
            thread::sleep(Duration::from_millis(70));
            ensure!(owned(hwnd), "Adjacent close/show destroyed the main window");
            post(tray, WM_TRAY, 1, WM_LBUTTONDBLCLK as isize)?;
            wait_visible(hwnd, true, stop)
                .with_context(|| format!("Cycle {cycle} adjacent close/show"))?;
        }
        if cycle % 6 == 0 {
            post(tray, WM_TRAY, 1, WM_CONTEXTMENU as isize)?;
            wait_until(stop, "opening native keyboard menu", || menu_open(tray))?;
            post(tray, WM_KEYDOWN, 0x1b, 0)?;
            post(tray, WM_KEYUP, 0x1b, 0)?;
            wait_until(stop, "dismissing native menu with Escape", || {
                Ok(!menu_open(tray)?)
            })?;
            post(tray, WM_TRAY, 1, WM_LBUTTONUP as isize)?;
            wait_visible(hwnd, true, stop)?;
        }
        let mut report = report.lock().unwrap();
        ensure!(
            report
                .frames
                .last()
                .is_some_and(|frame| frame.text_preserved),
            "Cycle {cycle} changed the native editors' text"
        );
        report.cycles += 1;
        report
            .checks
            .push(format!("cycle_{cycle:02}_close_hide_tray_restore"));
    }
    let mut report = report.lock().unwrap();
    ensure!(
        report.frames.iter().all(|frame| !frame.exiting),
        "App requested exit before the smoke driver completed"
    );
    ensure!(
        report.frames.iter().all(|frame| frame.text_preserved),
        "Hide/restore altered source or translation text"
    );
    report.checks.extend([
        "single_click_double_click_and_rapid_clicks".into(),
        "adjacent_close_show_survives_and_restores".into(),
        "native_context_menu_escape_and_restore".into(),
        "hidden_window_wakes_without_smoke_repaint_timer".into(),
    ]);
    Ok(())
}
