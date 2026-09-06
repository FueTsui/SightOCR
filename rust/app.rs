use anyhow::{Context, Result};
use eframe::egui::{self, ViewportCommand};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use sightocr::{
    config::{Config, LANGUAGES},
    platform::{self, Platform, PlatformEvent, SingleInstance},
    updater::{PreparedUpdate, UpdateProgress},
    worker::{self, Output, Progress, ProgressStage, Request, Task, Worker},
};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};

#[path = "ui/fonts.rs"]
mod fonts;
#[path = "ui/hotkey_input.rs"]
mod hotkey_input;
#[path = "ui/native_text.rs"]
mod native_text;
#[path = "ui/progress.rs"]
mod progress_ui;
#[path = "ui/settings.rs"]
mod settings_ui;
#[path = "ui/theme.rs"]
mod theme;
#[path = "ui/update.rs"]
mod update_ui;
#[path = "ui/window_chrome.rs"]
mod window_chrome;
#[path = "ui/workspace.rs"]
mod workspace;

#[cfg(debug_assertions)]
#[path = "ui/silent_smoke.rs"]
mod silent_smoke;
#[cfg(debug_assertions)]
#[path = "ui_smoke.rs"]
mod ui_smoke;
#[cfg(debug_assertions)]
pub use ui_smoke::run as run_smoke;

const OCR_MODES: &[(&str, &str)] = &[
    ("默认", "本地 · 文本"),
    ("默认_table", "本地 · 表格"),
    ("Baidu_auto", "百度 · 文本"),
    ("Baidu_accurate_basic", "百度 · 高精度"),
    ("Baidu_accurate", "百度 · 高精度含位置"),
    ("Baidu_general_basic", "百度 · 通用"),
    ("Baidu_general", "百度 · 通用含位置"),
    ("Baidu_table", "百度 · 表格"),
    ("Baidu_formula", "百度 · 公式"),
    ("Tencent_auto", "腾讯 · 文本"),
    ("Tencent_general_basic", "腾讯 · 通用"),
    ("Tencent_general_accurate", "腾讯 · 高精度"),
    ("Tencent_table", "腾讯 · 表格"),
    ("Tencent_formula", "腾讯 · 公式"),
];
const TRANSLATORS: &[(&str, &str)] = &[
    ("默认", "Bing"),
    ("Baidu", "百度"),
    ("Tencent", "腾讯"),
    ("OpenAI", "OpenAI"),
    ("Nvidia", "NVIDIA"),
];

const WINDOW_SIZE: [f32; 2] = [740.0, 680.0];

fn main_viewport() -> egui::ViewportBuilder {
    // The reference's 1110 × 1020 client area is 740 × 680 logical pixels at 150% DPI.
    egui::ViewportBuilder::default()
        .with_inner_size(WINDOW_SIZE)
        .with_min_inner_size([740.0, 580.0])
        .with_resizable(true)
        .with_maximize_button(true)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CaptureMode {
    Recognize,
    Translate,
    Silent,
}

struct PendingCapture {
    frame: u64,
    id: u64,
    translate: bool,
    transitions: window_chrome::CaptureTransitions,
}

enum Event {
    UpdateProgress(UpdateProgress),
    UpdateCompleted(Result<Option<PreparedUpdate>, String>),
    Platform(PlatformEvent),
    Capture {
        id: u64,
        translate: bool,
        result: Result<Option<image::RgbaImage>, String>,
    },
    Completed(Output),
    Progress(Progress),
}

pub fn run(silent: bool) -> Result<()> {
    platform::set_dpi_awareness();
    let Some(instance) = SingleInstance::acquire()? else {
        return Ok(());
    };
    let (mut config, path) = Config::load()?;
    config.autostart = platform::is_autostart_enabled()?;
    let icon = image::load_from_memory(include_bytes!("../assets/icon.png"))?.into_rgba8();
    let options = eframe::NativeOptions {
        viewport: main_viewport()
            .with_title("SightOCR")
            .with_icon(egui::IconData {
                width: icon.width(),
                height: icon.height(),
                rgba: icon.into_raw(),
            }),
        ..Default::default()
    };
    let restart = Arc::new(AtomicBool::new(false));
    let restart_request = restart.clone();
    eframe::run_native(
        "SightOCR",
        options,
        Box::new(move |cc| {
            let mut app = App::new(cc, config, path)?;
            app.restart = restart_request;
            if silent {
                // eframe forcibly shows the first rendered frame. Hide on the next one.
                app.startup_hide_after_frames = 2;
                cc.egui_ctx.request_repaint();
            }
            Ok(Box::new(app))
        }),
    )
    .map_err(|error| anyhow::anyhow!("无法启动界面：{error}"))?;
    // Drop the GUI, worker and global hotkeys, then release the single-instance mutex
    // before starting the replacement process so it cannot activate the old instance.
    drop(instance);
    if restart.load(Ordering::Acquire) {
        std::process::Command::new(std::env::current_exe()?)
            .spawn()
            .context("无法重新启动 SightOCR")?;
    }
    Ok(())
}

struct App {
    logo: egui::TextureHandle,
    config: Config,
    path: PathBuf,
    draft: Config,
    settings_open: bool,
    settings_error: String,
    settings_tab: usize,
    compact_result_tab: usize,
    capture_translate: bool,
    silent_job: bool,
    editing_text: bool,
    focus_source: bool,
    focus_translation: bool,
    source_image_size: Option<(u32, u32)>,
    pending_image_size: Option<(u32, u32)>,
    source: String,
    translation: String,
    status: String,
    warning: String,
    error: String,
    busy: bool,
    capturing: bool,
    cancelled: bool,
    request_id: u64,
    started: Instant,
    job: Option<progress_ui::JobProgress>,
    always_on_top: bool,
    exiting: bool,
    restart: Arc<AtomicBool>,
    update: update_ui::State,
    sender: mpsc::Sender<Event>,
    receiver: mpsc::Receiver<Event>,
    platform: Option<Platform>,
    worker: Worker,
    hwnd: usize,
    native_editors: native_text::NativeEditors,
    activation_requested: Arc<AtomicBool>,
    exit_requested: Arc<AtomicBool>,
    _activation_guard: window_chrome::MainWindowActivation,
    chrome_dark: Option<bool>,
    startup_hide_after_frames: u8,
    background_hidden: bool,
    ui_frame: u64,
    raise_after_frame: Option<u64>,
    pending_capture: Option<PendingCapture>,
    #[cfg(debug_assertions)]
    smoke: Option<ui_smoke::State>,
    #[cfg(debug_assertions)]
    smoke_copies: Option<std::cell::RefCell<Vec<String>>>,
    #[cfg(debug_assertions)]
    smoke_capture_probe: Option<mpsc::Sender<Result<(), String>>>,
}

impl App {
    fn consume_native_requests(&mut self, ctx: &egui::Context) {
        if self.exit_requested.swap(false, Ordering::AcqRel) {
            self.request_exit(ctx, false);
        }
        if self.activation_requested.swap(false, Ordering::AcqRel) && !self.exiting {
            self.show(ctx);
        }
    }

    fn new(cc: &eframe::CreationContext<'_>, config: Config, path: PathBuf) -> Result<Self> {
        let RawWindowHandle::Win32(handle) = cc.window_handle()?.as_raw() else {
            anyhow::bail!("无法取得 Windows 主窗口句柄");
        };
        let hwnd = handle.hwnd.get() as usize;
        let activation_requested = Arc::new(AtomicBool::new(false));
        let exit_requested = Arc::new(AtomicBool::new(false));
        let activation_guard = window_chrome::MainWindowActivation::install(
            hwnd,
            activation_requested.clone(),
            exit_requested.clone(),
            cc.egui_ctx.clone(),
        )?;
        window_chrome::center_on_work_area(hwnd);
        fonts::install(&cc.egui_ctx);
        theme::install(&cc.egui_ctx);
        let logo_image =
            image::load_from_memory(include_bytes!("../assets/icon.png"))?.into_rgba8();
        let logo = cc.egui_ctx.load_texture(
            "sightocr-logo",
            egui::ColorImage::from_rgba_unmultiplied(
                [logo_image.width() as usize, logo_image.height() as usize],
                logo_image.as_raw(),
            ),
            egui::TextureOptions::LINEAR,
        );
        if let Some(preference) = config
            .extra
            .get("appearance")
            .and_then(|value| value.as_str())
        {
            cc.egui_ctx.set_theme(match preference {
                "light" => egui::ThemePreference::Light,
                "dark" => egui::ThemePreference::Dark,
                _ => egui::ThemePreference::System,
            });
        }
        let (sender, receiver) = mpsc::channel();
        let ctx = cc.egui_ctx.clone();
        let deliver = sender.clone();
        let progress_sender = sender.clone();
        let progress_ctx = ctx.clone();
        let worker = Worker::start_with_progress(
            worker::resources_dir(),
            move |output| {
                let _ = deliver.send(Event::Completed(output));
                wake_ui(&ctx, hwnd);
            },
            move |progress| {
                let _ = progress_sender.send(Event::Progress(progress));
                wake_ui(&progress_ctx, hwnd);
            },
        )?;
        let (native_sender, native_receiver) = mpsc::channel();
        let mut error = String::new();
        let platform = match Platform::start(
            &config.hotkey,
            &config.translate_hotkey,
            &config.silent_hotkey,
            config.hide_tray_icon,
            native_sender,
        ) {
            Ok(platform) => Some(platform),
            Err(e) => {
                error = format!("系统托盘/热键不可用：{e:#}");
                None
            }
        };
        let ctx = cc.egui_ctx.clone();
        let deliver = sender.clone();
        thread::Builder::new()
            .name("sightocr-events".into())
            .spawn(move || {
                while let Ok(event) = native_receiver.recv() {
                    // Forward intent only. A later capture or exit may supersede a Show
                    // in the same frame; queued native restoration would race selection.
                    if deliver.send(Event::Platform(event)).is_err() {
                        break;
                    }
                    wake_ui(&ctx, hwnd);
                }
            })?;
        Ok(Self {
            logo,
            draft: config.clone(),
            config,
            path,
            settings_open: false,
            settings_error: String::new(),
            settings_tab: 0,
            compact_result_tab: 0,
            capture_translate: false,
            silent_job: false,
            editing_text: false,
            focus_source: false,
            focus_translation: false,
            source_image_size: None,
            pending_image_size: None,
            source: String::new(),
            translation: String::new(),
            status: "就绪 · 新建截图或输入文字".into(),
            warning: String::new(),
            error,
            busy: false,
            capturing: false,
            cancelled: false,
            request_id: 0,
            started: Instant::now(),
            job: None,
            always_on_top: false,
            exiting: false,
            restart: Arc::new(AtomicBool::new(false)),
            update: update_ui::State::default(),
            sender,
            receiver,
            platform,
            worker,
            hwnd,
            native_editors: native_text::NativeEditors::new(hwnd, cc.egui_ctx.clone())?,
            activation_requested,
            exit_requested,
            _activation_guard: activation_guard,
            chrome_dark: None,
            startup_hide_after_frames: 0,
            background_hidden: false,
            ui_frame: 0,
            raise_after_frame: None,
            pending_capture: None,
            #[cfg(debug_assertions)]
            smoke: None,
            #[cfg(debug_assertions)]
            smoke_copies: None,
            #[cfg(debug_assertions)]
            smoke_capture_probe: None,
        })
    }

    fn show(&mut self, ctx: &egui::Context) {
        // A second-instance activation or tray action must not cover the selection.
        // Normal captures restore after selection; silent captures stay in the background.
        if self.capturing || self.exiting {
            return;
        }
        // Explicit activation or an error takes precedence over delayed --silent hiding.
        self.startup_hide_after_frames = 0;
        self.background_hidden = false;
        self.raise_after_frame = Some(self.ui_frame);
        ctx.request_repaint();
    }

    fn hide_in_background(&mut self, ctx: &egui::Context) {
        self.startup_hide_after_frames = 0;
        self.background_hidden = true;
        self.raise_after_frame = None;
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
    }

    fn begin(&mut self) -> bool {
        if self.busy {
            return false;
        }
        self.request_id = self.request_id.wrapping_add(1);
        self.busy = true;
        self.silent_job = false;
        self.cancelled = false;
        self.started = Instant::now();
        self.pending_image_size = None;
        self.job = None;
        self.error.clear();
        self.warning.clear();
        true
    }

    fn capture(&mut self, ctx: &egui::Context, translate: bool) {
        self.start_capture(
            ctx,
            if translate {
                CaptureMode::Translate
            } else {
                CaptureMode::Recognize
            },
        );
    }

    fn start_capture(&mut self, ctx: &egui::Context, mode: CaptureMode) {
        if !self.begin() {
            return;
        }
        self.silent_job = mode == CaptureMode::Silent;
        let translate = mode == CaptureMode::Translate;
        if !self.silent_job {
            self.capture_translate = translate;
        }
        self.settings_open = false;
        let transitions = match window_chrome::CaptureTransitions::disable(self.hwnd) {
            Ok(transitions) => transitions,
            Err(error) => {
                self.busy = false;
                self.error = format!("{error:#}");
                self.status = "截图失败".into();
                if !self.silent_job {
                    self.show(ctx);
                }
                return;
            }
        };
        self.capturing = true;
        // Let winit own both state changes. A native minimize racing this frame's
        // queued restore commands can otherwise re-show the HWND during DwmFlush.
        if window_chrome::is_visible(self.hwnd) {
            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
        }
        self.hide_in_background(ctx);
        self.status = "拖动选择区域 · Esc 取消".into();
        #[cfg(debug_assertions)]
        if self.smoke_copies.is_some() && self.smoke_capture_probe.is_none() {
            // Legacy lifecycle scenarios inject events directly. Handoff regression
            // scenarios use the real scheduling/native preparation path below.
            return;
        }
        self.pending_capture = Some(PendingCapture {
            frame: self.ui_frame,
            id: self.request_id,
            translate,
            transitions,
        });
        // Hidden windows need a posted paint, not only a scheduled winit redraw.
        wake_ui(ctx, self.hwnd);
    }

    fn launch_pending_capture(&mut self, ctx: &egui::Context) {
        let Some(pending) = &self.pending_capture else {
            return;
        };
        if self.exiting || !self.capturing || !self.busy || pending.id != self.request_id {
            self.pending_capture = None;
            return;
        }
        if self.ui_frame <= pending.frame {
            return;
        }
        let PendingCapture {
            id,
            translate,
            transitions,
            ..
        } = self.pending_capture.take().unwrap();
        let sender = self.sender.clone();
        let capture_ctx = ctx.clone();
        let hwnd = self.hwnd;
        #[cfg(debug_assertions)]
        let probe = self.smoke_capture_probe.clone();
        let spawn = thread::Builder::new()
            .name("sightocr-capture".into())
            .spawn(move || {
                // Every toolbar, tray and hotkey entry uses the same preparation gate.
                // Freeze the desktop only once the main HWND is actually out of the way.
                let prepared = transitions.prepare();
                #[cfg(debug_assertions)]
                if let Some(probe) = probe {
                    // Test only replaces desktop pixels; the production frame barrier,
                    // capture thread, native checks and compositor synchronization run.
                    let _ = probe.send(prepared.map_err(|error| format!("{error:#}")));
                    wake_ui(&capture_ctx, hwnd);
                    return;
                }
                let result = prepared
                    .and_then(|()| platform::capture_region())
                    .map_err(|error| format!("{error:#}"));
                let _ = sender.send(Event::Capture {
                    id,
                    translate,
                    result,
                });
                // Window activation belongs to the matching request on the UI thread.
                wake_ui(&capture_ctx, hwnd);
            });
        if let Err(error) = spawn {
            self.busy = false;
            self.capturing = false;
            self.error = error.to_string();
            if !self.silent_job {
                self.show(ctx);
            }
        }
    }

    fn submit(&mut self, task: Task) {
        self.started = Instant::now();
        self.job = Some(progress_ui::JobProgress::new(&self.config, &task));
        self.status = if matches!(&task, Task::Translate(_)) {
            "正在翻译…"
        } else {
            "正在识别…"
        }
        .into();
        if matches!(&task, Task::Translate(_)) {
            self.translation.clear();
            self.compact_result_tab = 1;
        }
        #[cfg(debug_assertions)]
        if self.smoke_copies.is_some() {
            return;
        }
        if let Err(error) = self.worker.submit(Request {
            id: self.request_id,
            config: self.config.clone(),
            task,
        }) {
            self.busy = false;
            self.job = None;
            self.error = format!("{error:#}");
        }
    }

    fn events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.receiver.try_recv() {
            if self.exiting {
                break;
            }
            match event {
                Event::UpdateProgress(progress) => self.update.progress(progress),
                Event::UpdateCompleted(result) => self.finish_update(ctx, result),
                Event::Progress(progress) => {
                    if progress.id != self.request_id || !self.busy || self.cancelled {
                        continue;
                    }
                    if let Some(job) = &mut self.job {
                        job.stage = progress.stage;
                    }
                    self.status = match progress.stage {
                        ProgressStage::Recognizing => "正在识别…",
                        ProgressStage::LocalFallback => "正在使用本地引擎识别…",
                        ProgressStage::Translating => "正在翻译…",
                    }
                    .into();
                    if let Some(source) = progress.recognized {
                        self.source = source;
                        self.source_image_size = self.pending_image_size;
                        self.translation.clear();
                    }
                    if progress.stage == ProgressStage::Translating {
                        self.compact_result_tab = 1;
                    }
                    if let Some(warning) = progress.warning {
                        self.warning = warning;
                    }
                    // Progress must not reactivate a window the user chose to hide.
                }
                Event::Platform(PlatformEvent::Ocr) => {
                    let shortcut = self.config.hotkey.clone();
                    if !hotkey_input::record_registered(ctx, &mut self.draft, &shortcut) {
                        self.capture(ctx, false);
                    }
                }
                Event::Platform(PlatformEvent::SilentOcr) => {
                    let shortcut = self.config.silent_hotkey.clone();
                    if !hotkey_input::record_registered(ctx, &mut self.draft, &shortcut) {
                        self.start_capture(ctx, CaptureMode::Silent);
                    }
                }
                Event::Platform(PlatformEvent::Translate) => {
                    let shortcut = self.config.translate_hotkey.clone();
                    if !hotkey_input::record_registered(ctx, &mut self.draft, &shortcut) {
                        self.capture(ctx, true);
                    }
                }
                Event::Platform(PlatformEvent::Show) => self.show(ctx),
                Event::Platform(PlatformEvent::Settings) => {
                    hotkey_input::clear(ctx);
                    self.open_preferences(0);
                    self.show(ctx);
                }
                Event::Platform(PlatformEvent::Restart) => {
                    self.request_exit(ctx, true);
                }
                Event::Platform(PlatformEvent::Exit) => {
                    self.request_exit(ctx, false);
                }
                Event::Platform(PlatformEvent::Error(error)) => {
                    self.error = error;
                    self.show(ctx);
                }
                Event::Capture {
                    id,
                    translate,
                    result,
                } => {
                    if id != self.request_id || !self.busy || !self.capturing {
                        continue;
                    }
                    self.capturing = false;
                    if !self.silent_job {
                        self.show(ctx);
                    }
                    if self.cancelled {
                        self.busy = false;
                        self.status = "已取消".into();
                        continue;
                    }
                    match result {
                        Ok(Some(image)) => {
                            self.pending_image_size = Some(image.dimensions());
                            self.submit(Task::Recognize {
                                image,
                                translate: translate && !self.silent_job,
                            });
                        }
                        Ok(None) => {
                            self.busy = false;
                            self.status = "已取消截图".into();
                        }
                        Err(error) => {
                            self.busy = false;
                            self.error = error;
                            self.status = "截图失败".into();
                        }
                    }
                }
                Event::Completed(output) => {
                    if output.id != self.request_id || !self.busy {
                        continue;
                    }
                    self.busy = false;
                    self.job = None;
                    let image_size = self.pending_image_size.take();
                    if self.cancelled {
                        self.status = "已取消".into();
                        continue;
                    }
                    if let Some(text) = output.recognized {
                        self.compact_result_tab = 0;
                        self.source = text;
                        self.source_image_size = image_size;
                        self.translation.clear();
                    }
                    if let Some(text) = output.translated {
                        self.compact_result_tab = 1;
                        self.translation = text;
                    }
                    self.warning = output.warning.unwrap_or_default();
                    self.error = output.error.unwrap_or_default();
                    if self.error.is_empty() {
                        let text = if self.translation.is_empty() {
                            &self.source
                        } else {
                            &self.translation
                        };
                        self.copy_output(ctx, text.clone());
                        self.status = format!(
                            "完成 · {:.1} 秒 · 结果已复制",
                            self.started.elapsed().as_secs_f32()
                        );
                    } else {
                        self.status = "处理失败，可重试或切换接口".into();
                    }
                    if !self.silent_job {
                        self.show(ctx);
                    }
                }
            }
        }
    }

    fn save_draft(&mut self) -> Result<()> {
        #[cfg(debug_assertions)]
        anyhow::ensure!(self.smoke_copies.is_none(), "冒烟测试不修改系统或用户设置");
        // The modeless settings window may be open while the main selections change.
        self.draft.last_ocr_selection = self.config.last_ocr_selection.clone();
        self.draft.normalize()?;
        let previous = self.config.clone();
        let platform = self
            .platform
            .as_ref()
            .context("系统热键服务不可用，请重新启动后再保存设置")?;
        let startup_snapshot = platform::snapshot_autostart()?;
        platform.update(
            &self.draft.hotkey,
            &self.draft.translate_hotkey,
            &self.draft.silent_hotkey,
            self.draft.hide_tray_icon,
        )?;
        let result = (|| {
            platform::set_autostart(self.draft.autostart)?;
            self.draft.save(&self.path)
        })();
        if let Err(error) = result {
            let rollback = platform.update(
                &previous.hotkey,
                &previous.translate_hotkey,
                &previous.silent_hotkey,
                previous.hide_tray_icon,
            );
            let startup_rollback = startup_snapshot.restore();
            rollback.context("保存失败，且热键回滚失败")?;
            startup_rollback.context("保存失败，且开机启动回滚失败")?;
            return Err(error);
        }
        self.config = self.draft.clone();
        self.status = "设置已保存".into();
        Ok(())
    }

    fn copy_output(&self, ctx: &egui::Context, text: String) {
        #[cfg(debug_assertions)]
        if let Some(copies) = &self.smoke_copies {
            copies.borrow_mut().push(text);
            return;
        }
        ctx.copy_text(text);
    }
}

/// Winit's scheduled RedrawWindow requests are suppressed for hidden Windows HWNDs.
/// A posted paint runs the normal egui event path without making the window visible.
fn wake_ui(ctx: &egui::Context, hwnd: usize) {
    ctx.request_repaint();
    wake_window(hwnd);
}

fn wake_window(hwnd: usize) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowThreadProcessId, PostMessageW, WM_PAINT,
    };
    let hwnd = hwnd as windows_sys::Win32::Foundation::HWND;
    let mut process = 0;
    // SAFETY: Querying an HWND accepts stale handles; the process check avoids other apps.
    unsafe { GetWindowThreadProcessId(hwnd, &mut process) };
    if process == std::process::id() {
        // SAFETY: Posts only a scalar standard message to our own window; no pointers escape.
        unsafe { PostMessageW(hwnd, WM_PAINT, 0, 0) };
    }
}

impl eframe::App for App {
    #[cfg(debug_assertions)]
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        if let Some(smoke) = &mut self.smoke {
            smoke.append_input(raw);
        }
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // egui may call update repeatedly before submitting ANY viewport commands.
        // Count actual frames, not layout passes, for native capture/activation handoff.
        let first_pass = ctx.output(|output| output.num_completed_passes == 0);
        if first_pass {
            self.ui_frame += 1;
        }
        self.native_editors
            .begin_frame(&mut self.source, &mut self.translation);
        self.consume_native_requests(ctx);
        if !self.settings_open || self.settings_tab != settings_ui::HOTKEYS_TAB {
            hotkey_input::clear(ctx);
        }
        hotkey_input::handle_input(ctx, &mut self.draft);
        self.events(ctx);
        if first_pass && self.startup_hide_after_frames > 0 {
            self.startup_hide_after_frames -= 1;
            if self.startup_hide_after_frames == 0 {
                self.hide_in_background(ctx);
            } else {
                ctx.request_repaint_after(Duration::from_millis(30));
            }
        }
        if self.busy {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if close_requested && !self.exiting {
            self.hide_in_background(ctx);
        }
        let previous_selection = (
            self.config.last_ocr_selection.clone(),
            self.config.last_translate_selection.clone(),
            self.config.source_lang.clone(),
            self.config.target_lang.clone(),
        );
        self.toolbar(ctx);
        let dark = ctx.style().visuals.dark_mode;
        if self.chrome_dark != Some(dark) {
            window_chrome::apply(self.hwnd, dark);
            self.chrome_dark = Some(dark);
        }
        let palette = theme::Palette::get(ctx);
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(palette.canvas).inner_margin(24.0))
            .show(ctx, |ui| {
                if self.settings_open {
                    self.settings_page(ui);
                } else {
                    self.workbench(ui, ctx);
                }
            });
        let changed = previous_selection
            != (
                self.config.last_ocr_selection.clone(),
                self.config.last_translate_selection.clone(),
                self.config.source_lang.clone(),
                self.config.target_lang.clone(),
            );
        if changed {
            if let Err(error) = self.config.save(&self.path) {
                self.error = format!("设置保存失败：{error:#}");
            }
        }
        self.native_editors.end_frame(
            ctx,
            self.settings_open || self.capturing || self.background_hidden || self.exiting,
        );
        #[cfg(debug_assertions)]
        if let Some(mut smoke) = self.smoke.take() {
            smoke.tick(self, ctx);
            self.smoke = Some(smoke);
        }
        if close_requested && !self.exiting {
            // Closing the title bar never exits, including the first close, hidden
            // tray icons, or unavailable platform services. Only Exit/Restart can exit.
            // Sending any viewport command requests another frame, so an idle
            // window must not emit CancelClose without an actual close request.
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
        }
        if self.exiting || self.capturing || self.background_hidden {
            self.raise_after_frame = None;
        }
        if (self.background_hidden || self.capturing) && !self.exiting {
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        } else if let Some(frame) = self.raise_after_frame {
            if self.ui_frame > frame {
                window_chrome::bring_to_front(self.hwnd, self.always_on_top);
                self.raise_after_frame = None;
            } else {
                // Emit only the final visibility intent after this frame's events and UI.
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(ViewportCommand::Focus);
                ctx.request_repaint();
            }
        }
        // Only a subsequent frame proves the previous frame's accumulated commands
        // (including discarded passes and eframe's initial show) have been applied.
        self.launch_pending_capture(ctx);
    }
}
