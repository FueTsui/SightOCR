//! Debug-only, isolated smoke run of the real application UI.
//!
//! Images come from egui's framebuffer and our native child controls' own paint
//! paths; the desktop and other apps are never captured.

#[path = "ui/capture_handoff_smoke.rs"]
mod capture_handoff_smoke;
#[path = "ui/editing_smoke.rs"]
mod editing_smoke;
#[path = "ui/ime_live_smoke.rs"]
mod ime_live_smoke;
#[path = "ui/live_display_smoke.rs"]
mod live_display_smoke;
#[path = "ui/text_stability_smoke.rs"]
mod text_stability_smoke;
#[path = "ui/tray_lifecycle_smoke.rs"]
mod tray_lifecycle_smoke;
#[path = "ui/wheel_smoke.rs"]
mod wheel_smoke;

use super::{App, Event};
use anyhow::{bail, Context, Result};
use eframe::egui::{self, ViewportCommand};
use serde::Serialize;
use sightocr::{
    config::{Config, ProxyConfig, ProxyMode},
    platform,
    worker::{Output, Progress, ProgressStage, Task},
};
use std::{
    path::PathBuf,
    ptr::null,
    sync::{mpsc, Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetClientRect, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId,
    IsIconic, IsWindowVisible, PostMessageW, GWL_STYLE, WM_CLOSE, WM_PAINT, WS_MAXIMIZEBOX,
    WS_THICKFRAME,
};

const TITLE: &str = "SightOCR UI Smoke";
const ACCEPTED: &str = "后台唤醒成功\nHidden-window completion was delivered.";
const SENTINEL: &str = "取消后的原文必须保留";
const KOREAN_SOURCE: &str = "你好嗎?\n안녕하세요\n\n謝謝您。\n감사합니다.\n\n晚安（睡前）。\n안녕히 주무세요\n\n한글 자모: ㄱㄴㄷ 한\n日本語: こんにちは\nEnglish: Hello";
const MULTILINGUAL_SOURCE: &str = "العَرَبِيَّة: السَّلَامُ عَلَيْكُمْ 123\nفارسی: سلام دنیا ۱۲۳\nעברית: שלום עולם 123\nहिन्दी: नमस्ते दुनिया क्ष त्र ज्ञ\nภาษาไทย: สวัสดีครับ กำลังทดสอบ\nភាសាខ្មែរ: សួស្តី អរគុណ ខ្ញុំ\nᠮᠣᠩᠭᠣᠯ ᠪᠢᠴᠢᠭ\nTiếng Việt: Nguyễn Trường ắ ộ ự\nবাংলা: নমস্কার বিশ্ব\nதமிழ்: வணக்கம் உலகம்\n한국어: 안녕하세요 / 한\n中文、日本語、Українська: Ї Ґ Є\nMixed: العربية 123 English 中文";
const COUNTRY_SOURCE: &str = "Bahrain · البحرين\nJordan · الأردن\nOman · عُمان\nQatar · قطر\nKuwait · الكويت\nSaudi Arabia · المملكة العربية السعودية\nEgypt · مصر\nUnited Arab Emirates · الإمارات العربية المتحدة\nRépublique Centrafricaine · Côte d'Ivoire\nSénégal · Guinée · Guinée Équatoriale\nBotswana · Cameroun · Guinea-Bissau · India · Israel\nKenya · Madagascar · Mali · Maroc · Maurice\nMozambique · Niger · Nigeria · South Africa · Tunisie · Uganda";
const MIXED_CONTENT_SOURCE: &str = "日期 / Date: 2026-09-06 · 2026/9/6\n日期：2026年9月6日 · 06.09.2026\n时间 / Time: 14:43:23 · 09:05 AM\n18:30 UTC+08:00 · 00:00:01.250\n2026-09-06T14:43:23+08:00\n数字：0123456789 · ٠١٢٣٤٥٦٧٨٩\n金额：¥128.50 · $1,234.56 · €-12.30\n标点：[()] {[]} : ; , . ! ? % + - =\n项目\t日期\t金额\n收据\t2026-09-06\t¥128.50\n\\[\n\\frac{d}{dx}x^n=nx^{n-1}\n\\]\n# Heading **bold** `code`\n안녕하세요 · العربية · 中文";
const FORMULA_SOURCE: &str =
    "\\[\n\\frac{d}{dx} (x^{n}) = nx^{n-1}\n\\]\n\n\\[\na^{2} + b^{2} = c^{2}\n\\]";
const SAMPLE_SOURCE: &str = "让截图里的信息，成为可编辑的文字。\n\n截取屏幕上的产品说明、收据，或任意一段文字。识别完成后，你可以校对原文、复制内容，或直接翻译。\n\nQUICK START\n1. Capture an area of your screen.\n2. Review and edit the recognized text.\n3. Translate, then copy what you need.\n\n购买记录 · 2026/09/05\n笔记本 Notebook × 2    ¥ 48.00\n签字笔 Gel pen × 3      ¥ 18.00\n合计 Total                   ¥ 66.00";
const SAMPLE_TRANSLATION: &str = "Turn the information in screenshots into editable text.\n\nCapture a product guide, a receipt, or any text displayed on your screen. Once recognition finishes, review the source text, copy the content, or translate it directly.\n\nQUICK START\n1. Capture an area of your screen.\n2. Review and edit the recognized text.\n3. Translate, then copy what you need.\n\nPurchase record · September 5, 2026\nNotebook × 2                  CNY 48.00\nGel pen × 3                    CNY 18.00\nTotal                              CNY 66.00";

#[derive(Default, Serialize)]
struct Report {
    delayed_startup_hide: bool,
    initial_window_centered: bool,
    default_reference_window: bool,
    resized_window: bool,
    first_close_after_hiding_tray: bool,
    close_without_platform_keeps_running: bool,
    activation_without_platform: bool,
    close_frame_multipass: bool,
    capture_preparation_hides_window: bool,
    capture_preparation_restores_window: bool,
    main_screenshot: bool,
    korean_screenshot: bool,
    korean_dark_screenshot: bool,
    multilingual_screenshot: bool,
    multilingual_dark_screenshot: bool,
    countries_screenshot: bool,
    countries_translation_screenshot: bool,
    baidu_formula_screenshot: bool,
    tencent_formula_screenshot: bool,
    native_waiting_screenshot: bool,
    native_result_live: bool,
    native_language_menu_live: bool,
    native_language_changed_live: bool,
    text_stability: Vec<serde_json::Value>,
    wheel_scroll: Option<serde_json::Value>,
    native_editing: Vec<String>,
    empty_screenshot: bool,
    empty_translate_screenshot: bool,
    settings_screenshot: bool,
    hotkeys_screenshot: bool,
    hotkey_recording_screenshot: bool,
    registered_hotkey_recorded_without_capture: bool,
    hotkey_recording_deferred_for_focus: bool,
    hotkey_focus_checks: Vec<String>,
    services_screenshot: bool,
    mistral_screenshot: bool,
    openai_screenshot: bool,
    nvidia_screenshot: bool,
    proxy_screenshot: bool,
    proxy_compact_screenshot: bool,
    about_screenshot: bool,
    dark_screenshot: bool,
    compact_screenshot: bool,
    minimum_screenshot: bool,
    settings_compact_screenshot: bool,
    busy_screenshot: bool,
    translating_screenshot: bool,
    translating_compact_screenshot: bool,
    fallback_waiting_screenshot: bool,
    cancelling_screenshot: bool,
    progress_publishes_source: bool,
    cancelled_progress_discarded: bool,
    cancel_returns_idle: bool,
    cancel_allows_immediate_new_task: bool,
    cancel_drops_late_event_during_new_task: bool,
    cancellation_ui_ms: u128,
    accepted_started_hidden: bool,
    accepted_result_received: bool,
    accepted_result_shows_window: bool,
    cancelled_started_hidden: bool,
    cancelled_result_discarded: bool,
    cancelled_result_keeps_window_hidden: bool,
    show_event_wakes_window: bool,
    settings_event_opens_page: bool,
    capture_handoff_passed: bool,
    capture_handoff_cases: Vec<capture_handoff_smoke::Observation>,
    restart_event_requests_shutdown: bool,
    installer_event_requests_shutdown: bool,
    update_progress_and_retry: bool,
    silent_workflow_passed: bool,
    silent_started_from_tray: bool,
    closed_without_tray_keeps_running: bool,
    complete: bool,
    errors: Vec<String>,
}

#[derive(Clone, Copy)]
enum Stage {
    Startup,
    AwaitStartup,
    FirstClose,
    RestoreFirstClose,
    CloseWithoutPlatform,
    AwaitIndependentActivation,
    Main,
    Korean,
    KoreanDark,
    Multilingual,
    MultilingualDark,
    Countries,
    CountriesTranslation,
    BaiduFormula,
    TencentFormula,
    NativeWaiting,
    NativeResult,
    NativeLanguageMenu,
    NativeLanguageChanged,
    NativeSteady,
    NativeStress,
    NativeContent,
    NativeScrolling,
    NativeEditing,
    Empty,
    EmptyTranslate,
    Settings,
    Hotkeys,
    ArmHotkeyRecording,
    HotkeyRecording,
    CheckHotkeyRecording,
    Services,
    Mistral,
    OpenAI,
    Nvidia,
    Proxy,
    ProxyCompact,
    About,
    UpdateDownload,
    UpdateFailure,
    Dark,
    Compact,
    Minimum,
    SettingsCompact,
    Busy,
    Translating,
    TranslatingCompact,
    FallbackWaiting,
    Cancelling,
    Restore,
    AwaitCapturePreparation,
    AwaitAccepted,
    CheckAccepted,
    AwaitCancelled,
    CheckCancelled,
    CheckShow,
    CheckSettings,
    CaptureHandoff,
    AwaitTrayForSilent,
    SilentWorkflow,
    Finish,
}

pub(super) struct State {
    directory: PathBuf,
    stage: Stage,
    since: Instant,
    started: Instant,
    stage_frames: u32,
    startup_seen_visible: bool,
    pending_screenshot: Option<&'static str>,
    framebuffer_screenshot: Option<egui::ColorImage>,
    text_stability_run: Option<text_stability_smoke::Run>,
    wheel_run: Option<wheel_smoke::Run>,
    editing_run: Option<editing_smoke::Run>,
    stability_redraws: [u64; 2],
    stability_first_frame: u64,
    stability_input_frames: u64,
    stability_repaint_causes: std::collections::BTreeMap<String, u64>,
    queued_input: Vec<egui::Event>,
    report: Arc<Mutex<Report>>,
    silent_run: Option<super::silent_smoke::Run>,
    capture_handoff_run: Option<capture_handoff_smoke::Run>,
    close_discard_pending: bool,
    shutdown_evaluated: bool,
}

pub fn run(directory: PathBuf) -> Result<()> {
    if std::env::var("SIGHTOCR_SMOKE_UI_SCENARIO").as_deref() == Ok("tray") {
        return tray_lifecycle_smoke::run(directory);
    }
    if std::env::var("SIGHTOCR_SMOKE_UI_SCENARIO").as_deref() == Ok("ime-live") {
        return ime_live_smoke::run(directory);
    }
    if std::env::var("SIGHTOCR_SMOKE_UI_SCENARIO").as_deref() == Ok("editing") {
        return editing_smoke::run(directory);
    }
    std::fs::create_dir_all(&directory).context("无法创建 UI 冒烟测试输出目录")?;
    let directory = directory.canonicalize()?;
    // Keep test configuration outside every real user/app configuration directory.
    let scratch = tempfile::tempdir().context("无法创建隔离测试目录")?;
    let config = Config {
        hotkey: "Ctrl+Alt+Shift+F23".into(),
        translate_hotkey: "Ctrl+Alt+Shift+F24".into(),
        silent_hotkey: "Ctrl+Alt+Shift+F22".into(),
        target_lang: "en".into(),
        hide_tray_icon: false,
        ..Config::default()
    };
    let path = scratch.path().join("config.json");
    let report = Arc::new(Mutex::new(Report::default()));
    let app_report = report.clone();
    let app_directory = directory.clone();
    platform::set_dpi_awareness();
    let options = eframe::NativeOptions {
        viewport: super::main_viewport().with_title(TITLE),
        ..Default::default()
    };
    let (finished, watchdog_stop) = mpsc::channel();
    let timeout_report = report.clone();
    let shutdown_sender = Arc::new(Mutex::new(None::<mpsc::Sender<Event>>));
    let watchdog_sender = shutdown_sender.clone();
    let watchdog = thread::Builder::new()
        .name("ui-smoke-watchdog".into())
        .spawn(move || {
            if watchdog_stop
                .recv_timeout(Duration::from_secs(150))
                .is_err()
            {
                timeout_report
                    .lock()
                    .unwrap()
                    .errors
                    .push("UI smoke run timed out".into());
                if let Some(sender) = watchdog_sender.lock().unwrap().as_ref() {
                    // WM_CLOSE alone now hides into the background even without a tray icon.
                    let _ = sender.send(Event::Platform(platform::PlatformEvent::Exit));
                }
                if let Some(hwnd) = own_window() {
                    // SAFETY: The HWND was verified to belong to this process; scalar close only.
                    unsafe {
                        PostMessageW(hwnd, WM_CLOSE, 0, 0);
                        PostMessageW(hwnd, WM_PAINT, 0, 0);
                    }
                }
            }
        })?;
    let result = eframe::run_native(
        TITLE,
        options,
        Box::new(move |cc| {
            let mut app = App::new(cc, config, path)?;
            *shutdown_sender.lock().unwrap() = Some(app.sender.clone());
            app.smoke_copies = Some(std::cell::RefCell::new(Vec::new()));
            // Exercise the --silent frame ordering without changing real tray/startup settings.
            app.startup_hide_after_frames = 2;
            cc.egui_ctx.set_theme(egui::ThemePreference::Light);
            app.source = SAMPLE_SOURCE.into();
            app.translation = SAMPLE_TRANSLATION.into();
            app.config.last_ocr_selection = "Mistral_auto".into();
            app.config.last_translate_selection = "OpenAI".into();
            app.status = "识别完成 · 已生成示例译文".into();
            app.smoke = Some(State {
                directory: app_directory,
                stage: Stage::Startup,
                since: Instant::now(),
                started: Instant::now(),
                stage_frames: 0,
                startup_seen_visible: false,
                pending_screenshot: None,
                framebuffer_screenshot: None,
                text_stability_run: None,
                wheel_run: None,
                editing_run: None,
                stability_redraws: [0; 2],
                stability_first_frame: 0,
                stability_input_frames: 0,
                stability_repaint_causes: std::collections::BTreeMap::new(),
                queued_input: Vec::new(),
                report: app_report,
                silent_run: None,
                capture_handoff_run: None,
                close_discard_pending: false,
                shutdown_evaluated: false,
            });
            cc.egui_ctx.request_repaint();
            Ok(Box::new(app))
        }),
    )
    .map_err(|error| anyhow::anyhow!("UI smoke renderer failed: {error}"));
    let _ = finished.send(());
    let _ = watchdog.join();
    if let Err(error) = &result {
        report.lock().unwrap().errors.push(error.to_string());
    }
    let report = report.lock().unwrap();
    std::fs::write(
        directory.join("report.json"),
        serde_json::to_vec_pretty(&*report)?,
    )?;
    result?;
    if !report.complete || !report.errors.is_empty() {
        bail!(
            "UI 冒烟测试失败，请查看 {}",
            directory.join("report.json").display()
        );
    }
    Ok(())
}

impl State {
    pub(super) fn append_input(&mut self, raw: &mut egui::RawInput) {
        raw.events.append(&mut self.queued_input);
        if let Some(run) = self.wheel_run.as_mut() {
            run.append_input(raw);
        }
        if let Some(run) = self.editing_run.as_mut() {
            run.append_input(raw);
        }
    }

    pub(super) fn tick(&mut self, app: &mut App, ctx: &egui::Context) {
        if self.close_discard_pending && ctx.output(|output| output.num_completed_passes > 0) {
            self.report.lock().unwrap().close_frame_multipass = true;
            self.close_discard_pending = false;
        }
        if matches!(self.stage, Stage::FirstClose | Stage::CloseWithoutPlatform)
            && ctx.input(|input| input.viewport().close_requested())
        {
            self.since = Instant::now();
            self.close_discard_pending = true;
            ctx.request_discard("Exercise close cancellation across layout passes");
        }
        self.stage_frames += 1;
        if matches!(self.stage, Stage::Startup) {
            self.startup_seen_visible |= is_visible();
        }
        // Never permit a copy/cut command to escape this isolated test session.
        ctx.output_mut(|output| output.commands.clear());
        let screenshot = ctx.input(|input| {
            input.events.iter().find_map(|event| {
                if let egui::Event::Screenshot {
                    image, user_data, ..
                } = event
                {
                    let filename = user_data.data.as_ref()?.downcast_ref::<&'static str>()?;
                    (self.pending_screenshot == Some(*filename)).then(|| image.clone())
                } else {
                    None
                }
            })
        });
        if let Some(image) = screenshot {
            let visible = if matches!(
                self.stage,
                Stage::NativeResult | Stage::NativeLanguageMenu | Stage::NativeLanguageChanged
            ) {
                Some(self.verify_live_display(app, ctx, &image))
            } else if self.uses_visible_screenshot() {
                Some(live_display_smoke::capture(app.hwnd).and_then(|actual| {
                    anyhow::ensure!(
                        actual.size == image.size,
                        "Live language fixture dimensions differ"
                    );
                    Ok(actual)
                }))
            } else {
                None
            };
            self.framebuffer_screenshot = Some(match visible {
                Some(Ok(actual)) => actual,
                Some(Err(error)) => {
                    self.fail(
                        app,
                        ctx,
                        format!("Live result display verification failed: {error:#}"),
                    );
                    return;
                }
                None => (*image).clone(),
            });
        }
        if let (Some(mut image), Some(filename)) =
            (self.framebuffer_screenshot.take(), self.pending_screenshot)
        {
            let rendered = if self.uses_visible_screenshot() {
                // Keep the already displayed pixels exactly as captured. Native
                // diagnostic printing must never overwrite this visual evidence.
                Ok(true)
            } else {
                app.native_editors.composite_screenshot(&mut image)
            };
            match rendered {
                Ok(true) => self.pending_screenshot = None,
                Ok(false) => {
                    // Paint the native children after this parent WM_PAINT returns,
                    // then combine them with the already captured egui frame.
                    self.framebuffer_screenshot = Some(image);
                    ctx.request_repaint();
                    return;
                }
                Err(error) => {
                    self.fail(
                        app,
                        ctx,
                        format!("Cannot render native result editors: {error:#}"),
                    );
                    return;
                }
            }
            let bytes: Vec<u8> = image
                .pixels
                .iter()
                .flat_map(|pixel| pixel.to_array())
                .collect();
            let saved = image::save_buffer(
                self.directory.join(filename),
                &bytes,
                image.size[0] as u32,
                image.size[1] as u32,
                image::ColorType::Rgba8,
            );
            match saved {
                Ok(()) => match self.stage {
                    Stage::Main => {
                        self.report.lock().unwrap().main_screenshot = true;
                        app.source = KOREAN_SOURCE.into();
                        app.translation.clear();
                        app.config.last_ocr_selection = "默认".into();
                        app.status = "识别完成 · 韩文显示回归样例".into();
                        self.advance(Stage::Korean);
                    }
                    Stage::Korean => {
                        self.report.lock().unwrap().korean_screenshot = true;
                        ctx.set_theme(egui::ThemePreference::Dark);
                        self.advance(Stage::KoreanDark);
                    }
                    Stage::KoreanDark => {
                        self.report.lock().unwrap().korean_dark_screenshot = true;
                        ctx.set_theme(egui::ThemePreference::Light);
                        app.source = MULTILINGUAL_SOURCE.into();
                        app.status = "识别完成 · 多语言排版回归样例".into();
                        self.advance(Stage::Multilingual);
                    }
                    Stage::Multilingual => {
                        self.report.lock().unwrap().multilingual_screenshot = true;
                        ctx.set_theme(egui::ThemePreference::Dark);
                        ctx.send_viewport_cmd(ViewportCommand::InnerSize(egui::vec2(
                            1060.0, 680.0,
                        )));
                        app.translation = MULTILINGUAL_SOURCE.into();
                        self.advance(Stage::MultilingualDark);
                    }
                    Stage::MultilingualDark => {
                        self.report.lock().unwrap().multilingual_dark_screenshot = true;
                        ctx.set_theme(egui::ThemePreference::Light);
                        ctx.send_viewport_cmd(ViewportCommand::InnerSize(egui::vec2(740.0, 680.0)));
                        app.translation.clear();
                        app.source = COUNTRY_SOURCE.into();
                        app.compact_result_tab = 0;
                        app.status = "显示验证 · 阿拉伯文、法文重音与英语混排".into();
                        self.advance(Stage::Countries);
                    }
                    Stage::Countries => {
                        self.report.lock().unwrap().countries_screenshot = true;
                        ctx.set_theme(egui::ThemePreference::Dark);
                        app.translation = COUNTRY_SOURCE.into();
                        app.compact_result_tab = 1;
                        self.advance(Stage::CountriesTranslation);
                    }
                    Stage::CountriesTranslation => {
                        self.report.lock().unwrap().countries_translation_screenshot = true;
                        ctx.set_theme(egui::ThemePreference::Light);
                        app.translation.clear();
                        app.compact_result_tab = 0;
                        app.source = FORMULA_SOURCE.into();
                        app.config.last_ocr_selection = "Baidu_formula".into();
                        app.status = "识别完成 · 公式格式回归样例".into();
                        self.advance(Stage::BaiduFormula);
                    }
                    Stage::BaiduFormula => {
                        self.report.lock().unwrap().baidu_formula_screenshot = true;
                        app.config.last_ocr_selection = "Tencent_formula".into();
                        self.advance(Stage::TencentFormula);
                    }
                    Stage::TencentFormula => {
                        self.report.lock().unwrap().tencent_formula_screenshot = true;
                        app.config.last_ocr_selection = "默认".into();
                        if !app.begin() {
                            self.fail(app, ctx, "Cannot start display transition fixture".into());
                            return;
                        }
                        app.submit(Task::Recognize {
                            image: image::RgbaImage::new(1, 1),
                            translate: false,
                        });
                        app.pending_image_size = Some((1440, 800));
                        self.advance(Stage::NativeWaiting);
                    }
                    Stage::NativeWaiting => {
                        self.report.lock().unwrap().native_waiting_screenshot = true;
                        app.focus_source = true;
                        let _ = app.sender.send(Event::Completed(Output {
                            id: app.request_id,
                            recognized: Some(COUNTRY_SOURCE.into()),
                            ..Output::default()
                        }));
                        self.advance(Stage::NativeResult);
                    }
                    Stage::NativeResult => {
                        if !app.native_editors.is_focused(0) {
                            self.fail(
                                app,
                                ctx,
                                "Result editor did not receive editing focus".into(),
                            );
                            return;
                        }
                        if let Err(error) = self.click_fixture(ctx, "smoke_source_language_rect") {
                            self.fail(app, ctx, error.to_string());
                            return;
                        }
                        self.advance(Stage::NativeLanguageMenu);
                    }
                    Stage::NativeLanguageMenu => {
                        if let Err(error) = self.click_fixture(ctx, "smoke_source_language_en_rect")
                        {
                            self.fail(app, ctx, error.to_string());
                            return;
                        }
                        self.advance(Stage::NativeLanguageChanged);
                    }
                    Stage::NativeLanguageChanged => {
                        app.config.source_lang = "auto".into();
                        app.config.last_ocr_selection = "Mistral_auto".into();
                        app.source = MULTILINGUAL_SOURCE.into();
                        app.translation.clear();
                        app.native_editors.blur();
                        self.queued_input
                            .push(egui::Event::PointerMoved(egui::pos2(12.0, 100.0)));
                        app.status = "识别完成 · 多语言连续显示验证".into();
                        self.advance(Stage::NativeSteady);
                    }
                    Stage::Empty => {
                        self.report.lock().unwrap().empty_screenshot = true;
                        app.capture_translate = true;
                        app.config.target_lang = "en".into();
                        self.advance(Stage::EmptyTranslate);
                    }
                    Stage::EmptyTranslate => {
                        self.report.lock().unwrap().empty_translate_screenshot = true;
                        app.capture_translate = false;
                        app.settings_open = true;
                        app.settings_tab = 0;
                        app.draft = app.config.clone();
                        self.advance(Stage::Settings);
                    }
                    Stage::Settings => {
                        self.report.lock().unwrap().settings_screenshot = true;
                        app.settings_tab = super::settings_ui::HOTKEYS_TAB;
                        self.advance(Stage::Hotkeys);
                    }
                    Stage::Hotkeys => {
                        self.report.lock().unwrap().hotkeys_screenshot = true;
                        app.show(ctx);
                        self.advance(Stage::ArmHotkeyRecording);
                    }
                    Stage::HotkeyRecording => {
                        if !super::hotkey_input::is_recording(ctx) || !is_foreground(app.hwnd) {
                            self.defer_hotkey_recording(app, ctx);
                            return;
                        }
                        self.report.lock().unwrap().hotkey_recording_screenshot = true;
                        // Windows delivers our already-registered shortcut as WM_HOTKEY,
                        // so record the native event and suppress duplicate notifications.
                        let _ = app
                            .sender
                            .send(Event::Platform(platform::PlatformEvent::Ocr));
                        let _ = app
                            .sender
                            .send(Event::Platform(platform::PlatformEvent::SilentOcr));
                        self.advance(Stage::CheckHotkeyRecording);
                    }
                    Stage::CheckHotkeyRecording => {
                        // No screenshot belongs to this state.
                    }
                    Stage::Services => {
                        self.report.lock().unwrap().services_screenshot = true;
                        ctx.data_mut(|data| {
                            data.insert_temp(egui::Id::new("settings_service_provider"), 4_usize)
                        });
                        self.advance(Stage::Mistral);
                    }
                    Stage::Mistral => {
                        self.report.lock().unwrap().mistral_screenshot = true;
                        ctx.data_mut(|data| {
                            data.insert_temp(egui::Id::new("settings_service_provider"), 5_usize)
                        });
                        self.advance(Stage::OpenAI);
                    }
                    Stage::OpenAI => {
                        self.report.lock().unwrap().openai_screenshot = true;
                        ctx.data_mut(|data| {
                            data.insert_temp(egui::Id::new("settings_service_provider"), 6_usize)
                        });
                        // Exercise the longest model names at the minimum supported width.
                        ctx.send_viewport_cmd(ViewportCommand::InnerSize(egui::vec2(740.0, 580.0)));
                        self.advance(Stage::Nvidia);
                    }
                    Stage::Nvidia => {
                        self.report.lock().unwrap().nvidia_screenshot = true;
                        ctx.send_viewport_cmd(ViewportCommand::InnerSize(egui::vec2(
                            1060.0, 680.0,
                        )));
                        app.settings_tab = 3;
                        app.draft.proxy = ProxyConfig {
                            mode: ProxyMode::Manual,
                            url: "http://127.0.0.1:7890".into(),
                            username: "example-user".into(),
                            password: "synthetic-test-password".into(),
                        };
                        self.advance(Stage::Proxy);
                    }
                    Stage::Proxy => {
                        self.report.lock().unwrap().proxy_screenshot = true;
                        ctx.send_viewport_cmd(ViewportCommand::InnerSize(egui::vec2(740.0, 580.0)));
                        self.advance(Stage::ProxyCompact);
                    }
                    Stage::ProxyCompact => {
                        self.report.lock().unwrap().proxy_compact_screenshot = true;
                        ctx.send_viewport_cmd(ViewportCommand::InnerSize(egui::vec2(
                            1060.0, 680.0,
                        )));
                        app.settings_tab = 2;
                        self.advance(Stage::About);
                    }
                    Stage::About => {
                        self.report.lock().unwrap().about_screenshot = true;
                        app.update.busy = true;
                        let _ = app.sender.send(Event::UpdateProgress(
                            sightocr::updater::UpdateProgress::Downloading {
                                version: "9.9.9".into(),
                                downloaded: 50 * 1024 * 1024,
                                total: 100 * 1024 * 1024,
                            },
                        ));
                        ctx.send_viewport_cmd(ViewportCommand::InnerSize(egui::vec2(740.0, 580.0)));
                        self.advance(Stage::UpdateDownload);
                    }
                    Stage::UpdateDownload => {
                        if app.update.fraction != Some(0.5) || !app.update.busy {
                            self.fail(app, ctx, "Update progress was not delivered".into());
                            return;
                        }
                        let _ = app
                            .sender
                            .send(Event::UpdateCompleted(Err("测试网络中断".into())));
                        self.advance(Stage::UpdateFailure);
                    }
                    Stage::UpdateFailure => {
                        if app.update.busy || !app.update.failed || app.exiting {
                            self.fail(app, ctx, "Update error did not allow retry".into());
                            return;
                        }
                        app.update.busy = true;
                        let _ = app.sender.send(Event::UpdateProgress(
                            sightocr::updater::UpdateProgress::Checking,
                        ));
                        let _ = app.sender.send(Event::UpdateCompleted(Ok(None)));
                        app.events(ctx);
                        if app.update.busy
                            || app.update.failed
                            || !app.update.message.contains("最新版本")
                        {
                            self.fail(app, ctx, "Update retry did not complete".into());
                            return;
                        }
                        self.report.lock().unwrap().update_progress_and_retry = true;
                        app.settings_open = false;
                        ctx.send_viewport_cmd(ViewportCommand::InnerSize(egui::vec2(
                            1060.0, 680.0,
                        )));
                        app.source = SAMPLE_SOURCE.into();
                        app.translation = SAMPLE_TRANSLATION.into();
                        app.status = "识别完成 · 已生成示例译文".into();
                        ctx.set_theme(egui::ThemePreference::Dark);
                        self.advance(Stage::Dark);
                    }
                    Stage::Dark => {
                        self.report.lock().unwrap().dark_screenshot = true;
                        ctx.set_theme(egui::ThemePreference::Light);
                        ctx.send_viewport_cmd(ViewportCommand::InnerSize(egui::vec2(760.0, 640.0)));
                        self.advance(Stage::Compact);
                    }
                    Stage::Compact => {
                        self.report.lock().unwrap().compact_screenshot = true;
                        app.compact_result_tab = 1;
                        app.error =
                            "翻译服务暂时不可用，请检查网络连接后重试。已识别的原文会保留。".into();
                        app.status = "翻译失败，原文已保留".into();
                        ctx.send_viewport_cmd(ViewportCommand::InnerSize(egui::vec2(740.0, 580.0)));
                        self.advance(Stage::Minimum);
                    }
                    Stage::Minimum => {
                        self.report.lock().unwrap().minimum_screenshot = true;
                        app.error.clear();
                        app.settings_error.clear();
                        app.settings_open = true;
                        app.settings_tab = super::settings_ui::HOTKEYS_TAB;
                        app.draft = app.config.clone();
                        ctx.set_theme(egui::ThemePreference::Dark);
                        self.advance(Stage::SettingsCompact);
                    }
                    Stage::SettingsCompact => {
                        self.report.lock().unwrap().settings_compact_screenshot = true;
                        ctx.set_theme(egui::ThemePreference::Light);
                        app.settings_open = false;
                        app.compact_result_tab = 0;
                        // Exercise the visible pending state without submitting any real work.
                        app.busy = true;
                        app.capturing = false;
                        app.cancelled = false;
                        app.request_id = 100;
                        app.started = Instant::now() - Duration::from_secs(8);
                        app.job = Some(super::progress_ui::JobProgress::new(
                            &app.config,
                            &Task::Recognize {
                                image: image::RgbaImage::new(1, 1),
                                translate: true,
                            },
                        ));
                        app.status = "正在识别图片中的文字，请稍候…".into();
                        self.advance(Stage::Busy);
                    }
                    Stage::Busy => {
                        self.report.lock().unwrap().busy_screenshot = true;
                        let _ = app.sender.send(Event::Progress(Progress {
                            id: 100,
                            stage: ProgressStage::Translating,
                            recognized: Some(SAMPLE_SOURCE.into()),
                            warning: None,
                        }));
                        ctx.send_viewport_cmd(ViewportCommand::InnerSize(egui::vec2(
                            1060.0, 680.0,
                        )));
                        self.advance(Stage::Translating);
                    }
                    Stage::Translating => {
                        let mut report = self.report.lock().unwrap();
                        report.translating_screenshot = true;
                        report.progress_publishes_source = app.source == SAMPLE_SOURCE
                            && app.translation.is_empty()
                            && app.translating();
                        drop(report);
                        ctx.set_theme(egui::ThemePreference::Dark);
                        ctx.send_viewport_cmd(ViewportCommand::InnerSize(egui::vec2(740.0, 580.0)));
                        self.advance(Stage::TranslatingCompact);
                    }
                    Stage::TranslatingCompact => {
                        self.report.lock().unwrap().translating_compact_screenshot = true;
                        app.warning =
                            "云端识别失败，已使用本地引擎完成识别，原接口设置保留。".into();
                        app.started = Instant::now() - Duration::from_secs(45);
                        self.advance(Stage::FallbackWaiting);
                    }
                    Stage::FallbackWaiting => {
                        self.report.lock().unwrap().fallback_waiting_screenshot = true;
                        let cancel_started = Instant::now();
                        app.cancel_job();
                        {
                            let mut report = self.report.lock().unwrap();
                            report.cancellation_ui_ms = cancel_started.elapsed().as_millis();
                            report.cancel_returns_idle =
                                !app.busy && app.job.is_none() && app.cancelled;
                        }
                        let _ = app.sender.send(Event::Progress(Progress {
                            id: 100,
                            stage: ProgressStage::Translating,
                            recognized: Some("CANCELLED PROGRESS MUST NOT REPLACE SOURCE".into()),
                            warning: None,
                        }));
                        self.advance(Stage::Cancelling);
                    }
                    Stage::Cancelling => {
                        let mut report = self.report.lock().unwrap();
                        report.cancelling_screenshot = true;
                        report.cancelled_progress_discarded =
                            app.source == SAMPLE_SOURCE && app.cancelled;
                        drop(report);
                        let previous_id = app.request_id;
                        let accepted = app.begin();
                        if !accepted {
                            self.fail(
                                app,
                                ctx,
                                "Cancellation did not allow an immediate new task".into(),
                            );
                            return;
                        }
                        app.submit(Task::Translate(SAMPLE_SOURCE.into()));
                        self.report.lock().unwrap().cancel_allows_immediate_new_task =
                            accepted && app.busy;
                        let _ = app.sender.send(Event::Completed(Output {
                            id: previous_id,
                            recognized: Some("LATE CANCELLED RESULT".into()),
                            ..Output::default()
                        }));
                        app.events(ctx);
                        self.report
                            .lock()
                            .unwrap()
                            .cancel_drops_late_event_during_new_task = app.busy
                            && app.source == SAMPLE_SOURCE
                            && app.request_id != previous_id;
                        app.busy = false;
                        app.job = None;
                        app.capturing = false;
                        app.cancelled = false;
                        app.error.clear();
                        app.warning.clear();
                        app.status = "识别完成 · 已生成示例译文".into();
                        app.settings_tab = 0;
                        ctx.set_theme(egui::ThemePreference::Light);
                        ctx.send_viewport_cmd(ViewportCommand::InnerSize(egui::vec2(
                            1060.0, 680.0,
                        )));
                        self.advance(Stage::Restore);
                    }
                    _ => {}
                },
                Err(error) => self.fail(app, ctx, format!("Cannot save {filename}: {error}")),
            }
        }
        match self.stage {
            Stage::Startup if app.startup_hide_after_frames == 0 && self.startup_seen_visible => {
                // Synchronize with the app's actual hide frame. A timer starting in App::new
                // can fire before the second frame and Show would cancel the pending hide.
                self.check_startup_hide(app, ctx);
                self.advance(Stage::AwaitStartup);
            }
            Stage::Startup if self.since.elapsed() > Duration::from_secs(4) => {
                self.fail(
                    app,
                    ctx,
                    "Startup did not render and submit its delayed hide".into(),
                );
            }
            Stage::AwaitStartup if is_visible() => {
                let (hidden, failed) = {
                    let report = self.report.lock().unwrap();
                    (report.delayed_startup_hide, !report.errors.is_empty())
                };
                if failed {
                    app.exiting = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                } else if hidden {
                    let result = app
                        .platform
                        .as_ref()
                        .context("Platform missing before first-close check")
                        .and_then(|platform| {
                            platform.update(
                                &app.config.hotkey,
                                &app.config.translate_hotkey,
                                &app.config.silent_hotkey,
                                true,
                            )
                        })
                        .and_then(|()| close_owned_window(app, ctx));
                    if let Err(error) = result {
                        self.fail(app, ctx, format!("Cannot prepare first close: {error:#}"));
                        return;
                    }
                    app.config.hide_tray_icon = true;
                    app.draft.hide_tray_icon = true;
                    self.advance(Stage::FirstClose);
                }
            }
            Stage::FirstClose if self.since.elapsed() > Duration::from_millis(200) => {
                let passed = hidden_window_survives()
                    && app.background_hidden
                    && !app.exiting
                    && app
                        .platform
                        .as_ref()
                        .is_some_and(|platform| !platform.tray_visible());
                if !passed {
                    self.fail(
                        app,
                        ctx,
                        "First close after hiding tray did not retain the background app".into(),
                    );
                    return;
                }
                self.report.lock().unwrap().first_close_after_hiding_tray = true;
                app.show(ctx);
                self.advance(Stage::RestoreFirstClose);
            }
            Stage::RestoreFirstClose
                if is_visible() && self.since.elapsed() > Duration::from_millis(200) =>
            {
                // Remove the native endpoint too: reopening must not depend on a
                // functioning tray/hotkey thread after a platform initialization failure.
                drop(app.platform.take());
                if let Err(error) = close_owned_window(app, ctx) {
                    self.fail(
                        app,
                        ctx,
                        format!("Cannot close without platform: {error:#}"),
                    );
                    return;
                }
                self.advance(Stage::CloseWithoutPlatform);
            }
            Stage::CloseWithoutPlatform if self.since.elapsed() > Duration::from_millis(200) => {
                let passed = hidden_window_survives()
                    && app.background_hidden
                    && !app.exiting
                    && app.platform.is_none();
                if !passed {
                    self.fail(
                        app,
                        ctx,
                        "Close with unavailable platform integration exited the app".into(),
                    );
                    return;
                }
                self.report
                    .lock()
                    .unwrap()
                    .close_without_platform_keeps_running = true;
                let name: Vec<u16> = platform::MAIN_ACTIVATE_MESSAGE
                    .encode_utf16()
                    .chain(Some(0))
                    .collect();
                // SAFETY: Register a stable, NUL-terminated message name; post only to
                // this smoke process's own validated main window without pointer payloads.
                let posted = unsafe {
                    let message =
                        windows_sys::Win32::UI::WindowsAndMessaging::RegisterWindowMessageW(
                            name.as_ptr(),
                        );
                    message != 0
                        && own_window().is_some_and(|hwnd| PostMessageW(hwnd, message, 0, 0) != 0)
                };
                if !posted {
                    self.fail(
                        app,
                        ctx,
                        "Cannot send independent activation request".into(),
                    );
                    return;
                }
                self.advance(Stage::AwaitIndependentActivation);
            }
            Stage::AwaitIndependentActivation if is_visible() => {
                self.report.lock().unwrap().activation_without_platform =
                    app.platform.is_none() && !app.background_hidden;
                let (native_sender, native_receiver) = mpsc::channel();
                match platform::Platform::start(
                    &app.config.hotkey,
                    &app.config.translate_hotkey,
                    &app.config.silent_hotkey,
                    true,
                    native_sender,
                ) {
                    Ok(platform) => app.platform = Some(platform),
                    Err(error) => {
                        self.fail(
                            app,
                            ctx,
                            format!("Cannot restore isolated platform: {error:#}"),
                        );
                        return;
                    }
                }
                let (sender, context, hwnd) = (app.sender.clone(), ctx.clone(), app.hwnd);
                thread::spawn(move || {
                    while let Ok(event) = native_receiver.recv() {
                        if sender.send(Event::Platform(event)).is_err() {
                            break;
                        }
                        super::wake_ui(&context, hwnd);
                    }
                });
                self.advance(Stage::Main);
            }
            Stage::AwaitIndependentActivation if self.since.elapsed() > Duration::from_secs(3) => {
                self.fail(
                    app,
                    ctx,
                    "Independent activation did not reopen the app without a platform endpoint"
                        .into(),
                );
            }
            Stage::Main
                if self.stable()
                    && self.since.elapsed() > Duration::from_secs(1)
                    && is_visible() =>
            {
                let centered = own_window_is_centered();
                self.report.lock().unwrap().initial_window_centered = centered;
                if !centered {
                    self.fail(
                        app,
                        ctx,
                        "Initial window is not centered in the monitor work area".into(),
                    );
                    return;
                }
                let default_reference = own_window_matches_size(super::WINDOW_SIZE);
                self.report.lock().unwrap().default_reference_window = default_reference;
                if !default_reference {
                    self.fail(
                        app,
                        ctx,
                        "Production viewport does not match default size with resize/maximize enabled"
                            .into(),
                    );
                    return;
                }
                self.screenshot(ctx, "main.png");
            }
            Stage::Korean if self.stable() => self.screenshot(ctx, "korean.png"),
            Stage::KoreanDark if self.stable() => self.screenshot(ctx, "korean-dark.png"),
            Stage::Multilingual if self.stable() => self.screenshot(ctx, "multilingual.png"),
            Stage::MultilingualDark if self.stable() => {
                self.screenshot(ctx, "multilingual-dark.png")
            }
            Stage::Countries if self.stable() => self.screenshot(ctx, "countries.png"),
            Stage::CountriesTranslation if self.stable() => {
                self.screenshot(ctx, "countries-translation.png")
            }
            Stage::BaiduFormula if self.stable() => self.screenshot(ctx, "baidu-formula.png"),
            Stage::TencentFormula if self.stable() => self.screenshot(ctx, "tencent-formula.png"),
            Stage::NativeWaiting if self.stable() => self.screenshot(ctx, "native-waiting.png"),
            Stage::NativeResult if self.stable() => self.screenshot(ctx, "native-result.png"),
            Stage::NativeLanguageMenu if self.stable() => {
                if !ctx.memory(|m| m.any_popup_open()) {
                    self.fail(app, ctx, "Source language popup did not open".into());
                    return;
                }
                self.screenshot(ctx, "native-language-menu.png");
            }
            Stage::NativeLanguageChanged if self.stable() => {
                if app.config.source_lang != "en" || ctx.memory(|m| m.any_popup_open()) {
                    self.fail(app, ctx, "Source language selection did not finish".into());
                    return;
                }
                self.screenshot(ctx, "native-language-changed.png");
            }
            Stage::NativeSteady | Stage::NativeStress | Stage::NativeContent if self.stable() => {
                if let Err(error) = self.poll_text_stability(app, ctx) {
                    self.fail(
                        app,
                        ctx,
                        format!("Text stability verification failed: {error:#}"),
                    );
                    return;
                }
                if matches!(
                    self.stage,
                    Stage::NativeSteady | Stage::NativeStress | Stage::NativeContent
                ) {
                    if !matches!(self.stage, Stage::NativeSteady) {
                        let x = 12.0 + (self.stage_frames % 12) as f32;
                        self.queued_input
                            .push(egui::Event::PointerMoved(egui::pos2(x, 70.0)));
                        ctx.request_repaint_after(Duration::from_millis(16));
                    }
                    // The sampling thread wakes the truly idle UI when done.
                    // Do not let the usual smoke polling force idle repainting.
                    return;
                }
            }
            Stage::NativeScrolling if self.stable() => {
                if self.wheel_run.is_none() {
                    match wheel_smoke::Run::start(app, ctx, self.directory.join("wheel")) {
                        Ok(run) => self.wheel_run = Some(run),
                        Err(error) => {
                            self.fail(
                                app,
                                ctx,
                                format!("Wheel verification setup failed: {error:#}"),
                            );
                            return;
                        }
                    }
                }
                if let Some(result) = self.wheel_run.as_mut().unwrap().tick(app, ctx) {
                    self.wheel_run.take();
                    match result {
                        Ok(report) => {
                            let passed = report.passed;
                            self.report.lock().unwrap().wheel_scroll =
                                Some(serde_json::to_value(report).unwrap());
                            if !passed {
                                self.fail(
                                    app,
                                    ctx,
                                    "Native mouse wheel verification failed".into(),
                                );
                                return;
                            }
                        }
                        Err(error) => {
                            self.fail(app, ctx, format!("Wheel verification failed: {error:#}"));
                            return;
                        }
                    }
                    self.advance(Stage::NativeEditing);
                }
            }
            Stage::NativeEditing if self.stable() => {
                if self.editing_run.is_none() {
                    self.editing_run = Some(editing_smoke::Run::start(app, ctx));
                }
                if let Some(result) = self.editing_run.as_mut().unwrap().tick(app, ctx) {
                    self.editing_run.take();
                    match result {
                        Ok(checks) => {
                            self.report.lock().unwrap().native_editing =
                                checks.into_iter().map(str::to_owned).collect();
                        }
                        Err(error) => {
                            self.fail(
                                app,
                                ctx,
                                format!("Native editing verification failed: {error:#}"),
                            );
                            return;
                        }
                    }
                    ctx.set_theme(egui::ThemePreference::Light);
                    ctx.send_viewport_cmd(ViewportCommand::InnerSize(egui::vec2(740.0, 680.0)));
                    app.source.clear();
                    app.translation.clear();
                    app.status = "准备就绪 · 新建截图，开始识别".into();
                    self.advance(Stage::Empty);
                }
            }
            Stage::Empty if self.stable() => {
                self.screenshot(ctx, "empty.png");
            }
            Stage::EmptyTranslate if self.stable() => {
                self.screenshot(ctx, "empty-translate.png");
            }
            Stage::Settings if self.stable() => {
                self.screenshot(ctx, "settings.png");
            }
            Stage::Hotkeys if self.stable() => self.screenshot(ctx, "hotkeys.png"),
            Stage::ArmHotkeyRecording if self.stable() => {
                // A real recorder only accepts input while this app is foreground.
                // Let activation settle instead of arming against another app's focus.
                let foreground = is_foreground(app.hwnd);
                if !foreground {
                    if self.since.elapsed() > Duration::from_secs(3) {
                        self.defer_hotkey_recording(app, ctx);
                    }
                    ctx.request_repaint_after(Duration::from_millis(40));
                    return;
                }
                super::hotkey_input::begin(ctx, super::hotkey_input::Target::SilentCapture);
                self.record_hotkey_focus(app, ctx, "armed");
                self.advance(Stage::HotkeyRecording);
            }
            Stage::HotkeyRecording if self.stable() => {
                if !super::hotkey_input::is_recording(ctx) || !is_foreground(app.hwnd) {
                    self.defer_hotkey_recording(app, ctx);
                    return;
                }
                self.record_hotkey_focus(app, ctx, "settled");
                self.screenshot(ctx, "hotkey-recording.png");
            }
            Stage::CheckHotkeyRecording if self.since.elapsed() > Duration::from_millis(150) => {
                let passed =
                    app.draft.silent_hotkey == app.config.hotkey && app.settings_open && !app.busy;
                self.report
                    .lock()
                    .unwrap()
                    .registered_hotkey_recorded_without_capture = passed;
                if !passed {
                    if !is_foreground(app.hwnd) {
                        self.defer_hotkey_recording(app, ctx);
                        return;
                    }
                    self.fail(
                        app,
                        ctx,
                        "Registered shortcut triggered capture or failed to record in settings"
                            .into(),
                    );
                    return;
                }
                super::hotkey_input::clear(ctx);
                app.draft = app.config.clone();
                app.settings_tab = 1;
                self.advance(Stage::Services);
            }
            Stage::Services if self.stable() => {
                self.screenshot(ctx, "services.png");
            }
            Stage::Mistral if self.stable() => self.screenshot(ctx, "mistral.png"),
            Stage::OpenAI if self.stable() => self.screenshot(ctx, "openai.png"),
            Stage::Nvidia if self.stable() => self.screenshot(ctx, "nvidia.png"),
            Stage::Proxy if self.stable() => {
                if !own_window_matches_size([1060.0, 680.0]) {
                    self.fail(app, ctx, "Window did not resize to 1060x680".into());
                    return;
                }
                self.screenshot(ctx, "proxy.png");
            }
            Stage::ProxyCompact if self.stable() => {
                let resized = own_window_matches_size([740.0, 580.0]);
                self.report.lock().unwrap().resized_window = resized;
                if !resized {
                    self.fail(app, ctx, "Window did not resize to 740x580".into());
                    return;
                }
                self.screenshot(ctx, "proxy-compact.png");
            }
            Stage::About if self.stable() => {
                self.screenshot(ctx, "about.png");
            }
            Stage::UpdateDownload if self.stable() => self.screenshot(ctx, "update-download.png"),
            Stage::UpdateFailure if self.stable() => self.screenshot(ctx, "update-failure.png"),
            Stage::Dark if self.stable() => {
                self.screenshot(ctx, "dark.png");
            }
            Stage::Compact if self.stable() => {
                self.screenshot(ctx, "compact.png");
            }
            Stage::Minimum if self.stable() => {
                self.screenshot(ctx, "minimum.png");
            }
            Stage::SettingsCompact if self.stable() => {
                self.screenshot(ctx, "settings-compact.png");
            }
            Stage::Busy if self.stable() => {
                self.screenshot(ctx, "busy.png");
            }
            Stage::Translating if self.stable() => self.screenshot(ctx, "translating.png"),
            Stage::TranslatingCompact if self.stable() => {
                self.screenshot(ctx, "translating-compact.png")
            }
            Stage::FallbackWaiting if self.stable() => self.screenshot(ctx, "fallback-waiting.png"),
            Stage::Cancelling if self.stable() => self.screenshot(ctx, "cancelling.png"),
            Stage::Restore if self.stable() => {
                let sender = app.sender.clone();
                let context = ctx.clone();
                let hwnd = app.hwnd;
                let report = self.report.clone();
                thread::spawn(move || {
                    let prepared = super::window_chrome::prepare_capture(hwnd);
                    let hidden = own_window().is_some_and(|window| {
                        // SAFETY: own_window verified that the HWND belongs to this smoke process.
                        unsafe { IsWindowVisible(window) == 0 && IsIconic(window) != 0 }
                    });
                    {
                        let mut report = report.lock().unwrap();
                        report.capture_preparation_hides_window = prepared.is_ok() && hidden;
                        if let Err(error) = prepared {
                            report
                                .errors
                                .push(format!("Capture preparation failed: {error:#}"));
                        } else if !hidden {
                            report.errors.push(
                                "Capture preparation did not minimize and hide the main window"
                                    .into(),
                            );
                        }
                    }
                    // No desktop screenshot is taken: the test exercises only the gate.
                    context.send_viewport_cmd(ViewportCommand::Visible(true));
                    context.send_viewport_cmd(ViewportCommand::Minimized(false));
                    let _ = sender.send(Event::Platform(platform::PlatformEvent::Show));
                    super::wake_ui(&context, hwnd);
                });
                self.advance(Stage::AwaitCapturePreparation);
            }
            Stage::AwaitCapturePreparation
                if self.since.elapsed() > Duration::from_millis(700) && is_visible() =>
            {
                let restored = own_window().is_some_and(|window| {
                    // SAFETY: own_window only returns this process's own HWND.
                    unsafe { IsIconic(window) == 0 }
                });
                self.report
                    .lock()
                    .unwrap()
                    .capture_preparation_restores_window = restored;
                if !restored {
                    self.fail(
                        app,
                        ctx,
                        "Capture preparation did not restore the main window".into(),
                    );
                    return;
                }
                app.request_id = 41;
                app.busy = true;
                app.cancelled = false;
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                self.deliver_later(app, ctx, 41, ACCEPTED, false);
                self.advance(Stage::AwaitAccepted);
            }
            Stage::AwaitAccepted if !app.busy => {
                let passed = app.source == ACCEPTED && app.error.is_empty();
                self.report.lock().unwrap().accepted_result_received = passed;
                if !passed {
                    self.fail(app, ctx, "Accepted result was not delivered".into());
                } else {
                    self.advance(Stage::CheckAccepted);
                }
            }
            Stage::CheckAccepted if self.since.elapsed() > Duration::from_millis(300) => {
                let visible = is_visible();
                self.report.lock().unwrap().accepted_result_shows_window = visible;
                if !visible {
                    self.fail(
                        app,
                        ctx,
                        "Accepted result did not show hidden window".into(),
                    );
                } else {
                    app.request_id = 42;
                    app.busy = true;
                    app.cancelled = true;
                    app.source = SENTINEL.into();
                    app.translation.clear();
                    ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                    self.deliver_later(app, ctx, 42, "THIS CANCELLED RESULT MUST NOT APPEAR", true);
                    self.advance(Stage::AwaitCancelled);
                }
            }
            Stage::AwaitCancelled if !app.busy => {
                let discarded = app.source == SENTINEL && app.translation.is_empty();
                self.report.lock().unwrap().cancelled_result_discarded = discarded;
                if !discarded {
                    self.fail(app, ctx, "Cancelled result replaced text".into());
                } else {
                    self.advance(Stage::CheckCancelled);
                }
            }
            Stage::CheckCancelled if self.since.elapsed() > Duration::from_millis(300) => {
                let hidden = !is_visible();
                self.report
                    .lock()
                    .unwrap()
                    .cancelled_result_keeps_window_hidden = hidden;
                if !hidden {
                    self.fail(app, ctx, "Cancelled result reopened hidden window".into());
                } else {
                    let _ = app
                        .sender
                        .send(Event::Platform(platform::PlatformEvent::Show));
                    super::wake_ui(ctx, app.hwnd);
                    self.advance(Stage::CheckShow);
                }
            }
            Stage::CheckShow if self.since.elapsed() > Duration::from_millis(300) => {
                let visible = is_visible();
                self.report.lock().unwrap().show_event_wakes_window = visible;
                if !visible {
                    self.fail(app, ctx, "Show event did not wake hidden window".into());
                } else {
                    app.settings_open = false;
                    app.settings_tab = 2;
                    ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                    let sender = app.sender.clone();
                    let context = ctx.clone();
                    let hwnd = app.hwnd;
                    thread::spawn(move || {
                        thread::sleep(Duration::from_millis(200));
                        let _ = sender.send(Event::Platform(platform::PlatformEvent::Settings));
                        super::wake_ui(&context, hwnd);
                    });
                    self.advance(Stage::CheckSettings);
                }
            }
            Stage::CheckSettings if self.since.elapsed() > Duration::from_millis(650) => {
                let passed = is_visible() && app.settings_open && app.settings_tab == 0;
                self.report.lock().unwrap().settings_event_opens_page = passed;
                if passed {
                    match capture_handoff_smoke::Run::new(app, ctx) {
                        Ok(run) => {
                            self.capture_handoff_run = Some(run);
                            self.advance(Stage::CaptureHandoff);
                        }
                        Err(error) => self.fail(
                            app,
                            ctx,
                            format!("Cannot start capture handoff checks: {error:#}"),
                        ),
                    }
                } else {
                    self.fail(
                        app,
                        ctx,
                        "Settings event did not open general settings from hidden window".into(),
                    );
                }
            }
            Stage::CaptureHandoff => {
                let result = self
                    .capture_handoff_run
                    .as_mut()
                    .expect("capture handoff initialized")
                    .tick(app, ctx);
                match result {
                    Ok(true) => {
                        let observations = self
                            .capture_handoff_run
                            .as_ref()
                            .expect("capture handoff initialized")
                            .observations();
                        {
                            let mut report = self.report.lock().unwrap();
                            report.capture_handoff_cases = observations;
                            report.capture_handoff_passed = true;
                        }
                        app.smoke_capture_probe = None;
                        self.capture_handoff_run = None;
                        match close_owned_window(app, ctx) {
                            Ok(()) => self.advance(Stage::AwaitTrayForSilent),
                            Err(error) => self.fail(
                                app,
                                ctx,
                                format!(
                                    "Cannot prepare silent smoke after capture handoff: {error:#}"
                                ),
                            ),
                        }
                    }
                    Ok(false) => {}
                    Err(error) => {
                        self.report.lock().unwrap().capture_handoff_cases = self
                            .capture_handoff_run
                            .as_ref()
                            .expect("capture handoff initialized")
                            .observations();
                        app.smoke_capture_probe = None;
                        self.capture_handoff_run = None;
                        self.fail(app, ctx, format!("Capture handoff failed: {error:#}"));
                    }
                }
            }
            Stage::AwaitTrayForSilent if self.since.elapsed() > Duration::from_millis(200) => {
                let tray_state = own_window().is_some_and(|window| {
                    // SAFETY: own_window restricts these read-only queries to this process.
                    unsafe { IsWindowVisible(window) == 0 && IsIconic(window) == 0 }
                });
                let background_running = app.config.hide_tray_icon
                    && !app.exiting
                    && app.background_hidden
                    && app
                        .platform
                        .as_ref()
                        .is_some_and(|platform| !platform.tray_visible());
                if !tray_state || !background_running {
                    self.fail(
                        app,
                        ctx,
                        "Closing with the tray icon disabled did not retain a hidden background app"
                            .into(),
                    );
                    return;
                }
                self.report.lock().unwrap().silent_started_from_tray = true;
                self.report
                    .lock()
                    .unwrap()
                    .closed_without_tray_keeps_running = true;
                self.silent_run = Some(super::silent_smoke::Run::new(app, ctx));
                self.advance(Stage::SilentWorkflow);
            }
            Stage::SilentWorkflow => {
                let result = self
                    .silent_run
                    .as_mut()
                    .expect("silent workflow initialized")
                    .tick(app, ctx);
                match result {
                    Ok(true) => {
                        self.report.lock().unwrap().silent_workflow_passed = true;
                        self.silent_run = None;
                        self.advance(Stage::Finish);
                    }
                    Ok(false) => {}
                    Err(error) => {
                        self.silent_run = None;
                        self.fail(app, ctx, format!("Silent workflow failed: {error:#}"));
                    }
                }
            }
            Stage::Finish
                if !self.shutdown_evaluated && self.started.elapsed() > Duration::from_secs(5) =>
            {
                self.shutdown_evaluated = true;
                // run_smoke does not use app::run, so the restart flag is observed without
                // spawning a second process or acquiring the user's single-instance mutex.
                let _ = app
                    .sender
                    .send(Event::Platform(platform::PlatformEvent::Restart));
                app.events(ctx);
                let mut report = self.report.lock().unwrap();
                report.restart_event_requests_shutdown =
                    app.restart.load(std::sync::atomic::Ordering::Acquire)
                        && app.exiting
                        && app.cancelled;
                // Exercise the installer's native shutdown route on this isolated app.
                // It must take precedence over an activation queued in the same frame.
                app.exiting = false;
                app.cancelled = false;
                app.activation_requested
                    .store(true, std::sync::atomic::Ordering::Release);
                let name: Vec<u16> = platform::MAIN_EXIT_MESSAGE
                    .encode_utf16()
                    .chain(Some(0))
                    .collect();
                // SAFETY: Send the registered scalar shutdown message only to this
                // smoke process's owned live main window; the string is NUL-terminated.
                unsafe {
                    let message =
                        windows_sys::Win32::UI::WindowsAndMessaging::RegisterWindowMessageW(
                            name.as_ptr(),
                        );
                    windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW(
                        app.hwnd as _,
                        message,
                        0,
                        0,
                    );
                }
                app.consume_native_requests(ctx);
                report.installer_event_requests_shutdown = app.exiting
                    && app.cancelled
                    && !app.restart.load(std::sync::atomic::Ordering::Acquire)
                    && app.raise_after_frame.is_none();
                report.complete = report.delayed_startup_hide
                    && report.initial_window_centered
                    && report.default_reference_window
                    && report.resized_window
                    && report.first_close_after_hiding_tray
                    && report.close_without_platform_keeps_running
                    && report.activation_without_platform
                    && report.close_frame_multipass
                    && report.capture_preparation_hides_window
                    && report.capture_preparation_restores_window
                    && report.main_screenshot
                    && report.korean_screenshot
                    && report.korean_dark_screenshot
                    && report.multilingual_screenshot
                    && report.multilingual_dark_screenshot
                    && report.countries_screenshot
                    && report.countries_translation_screenshot
                    && report.baidu_formula_screenshot
                    && report.tencent_formula_screenshot
                    && report.native_waiting_screenshot
                    && report.native_result_live
                    && report.native_language_menu_live
                    && report.native_language_changed_live
                    && report.text_stability.len() == 3
                    && report.wheel_scroll.is_some()
                    && report.native_editing.len() == 6
                    && report.empty_screenshot
                    && report.empty_translate_screenshot
                    && report.settings_screenshot
                    && report.hotkeys_screenshot
                    && (report.hotkey_recording_deferred_for_focus
                        || (report.hotkey_recording_screenshot
                            && report.registered_hotkey_recorded_without_capture))
                    && report.services_screenshot
                    && report.mistral_screenshot
                    && report.openai_screenshot
                    && report.nvidia_screenshot
                    && report.proxy_screenshot
                    && report.proxy_compact_screenshot
                    && report.about_screenshot
                    && report.dark_screenshot
                    && report.compact_screenshot
                    && report.minimum_screenshot
                    && report.settings_compact_screenshot
                    && report.busy_screenshot
                    && report.translating_screenshot
                    && report.translating_compact_screenshot
                    && report.fallback_waiting_screenshot
                    && report.cancelling_screenshot
                    && report.progress_publishes_source
                    && report.cancelled_progress_discarded
                    && report.cancel_returns_idle
                    && report.cancel_allows_immediate_new_task
                    && report.cancel_drops_late_event_during_new_task
                    && report.accepted_started_hidden
                    && report.accepted_result_received
                    && report.accepted_result_shows_window
                    && report.cancelled_started_hidden
                    && report.cancelled_result_discarded
                    && report.cancelled_result_keeps_window_hidden
                    && report.show_event_wakes_window
                    && report.settings_event_opens_page
                    && report.capture_handoff_passed
                    && report.silent_workflow_passed
                    && report.silent_started_from_tray
                    && report.closed_without_tray_keeps_running
                    && report.restart_event_requests_shutdown
                    && report.installer_event_requests_shutdown
                    && report.update_progress_and_retry;
                app.exiting = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
            _ => {}
        }
        ctx.request_repaint_after(Duration::from_millis(40));
    }

    fn poll_text_stability(&mut self, app: &mut App, ctx: &egui::Context) -> Result<()> {
        let stress = !matches!(self.stage, Stage::NativeSteady);
        let phase = match self.stage {
            Stage::NativeSteady => "light-idle",
            Stage::NativeStress => "dark-repaint",
            Stage::NativeContent => "mixed-content-repaint",
            _ => unreachable!(),
        };
        if self.text_stability_run.is_none() {
            anyhow::ensure!(
                !app.native_editors.is_focused(0) && !app.native_editors.is_focused(1),
                "Steady text capture must not include a blinking caret"
            );
            let mut body = ctx
                .data(|data| data.get_temp::<egui::Rect>(egui::Id::new("smoke_source_body_rect")))
                .context("Missing stable source body geometry")?;
            if stress {
                body = body.union(
                    ctx.data(|data| {
                        data.get_temp::<egui::Rect>(egui::Id::new("smoke_translation_body_rect"))
                    })
                    .context("Missing stable translation body geometry")?,
                );
            }
            let scale = ctx.pixels_per_point();
            let bounds = [
                (body.min.x * scale).ceil() as i32,
                (body.min.y * scale).ceil() as i32,
                (body.max.x * scale).floor() as i32 - (body.min.x * scale).ceil() as i32,
                (body.max.y * scale).floor() as i32 - (body.min.y * scale).ceil() as i32,
            ];
            let wake = ctx.clone();
            self.stability_redraws = app.native_editors.native_redraw_requests();
            self.stability_first_frame = app.ui_frame;
            self.stability_input_frames = 0;
            self.stability_repaint_causes.clear();
            self.text_stability_run = Some(text_stability_smoke::Run::start(
                app.hwnd,
                bounds,
                self.directory.join("stability").join(phase),
                move || wake.request_repaint(),
            )?);
            return Ok(());
        }
        self.stability_input_frames += u64::from(ctx.input(|input| !input.raw.events.is_empty()));
        for cause in ctx.repaint_causes() {
            *self
                .stability_repaint_causes
                .entry(cause.to_string())
                .or_default() += 1;
        }
        let Some(finished) = self.text_stability_run.as_mut().unwrap().try_finish() else {
            return Ok(());
        };
        self.text_stability_run.take();
        let observed = finished?;
        let panel = super::theme::Palette::get(ctx).panel.to_array();
        anyhow::ensure!(
            observed.background == Some([panel[0], panel[1], panel[2]])
                && observed.baseline_ink_pixels > 1_000,
            "Text stability baseline is blank, black or has the wrong theme"
        );
        let current_redraws = app.native_editors.native_redraw_requests();
        let redraws = [
            current_redraws[0].saturating_sub(self.stability_redraws[0]),
            current_redraws[1].saturating_sub(self.stability_redraws[1]),
        ];
        let parent_frames = app.ui_frame - self.stability_first_frame;
        let mut evidence = serde_json::to_value(&observed)?;
        evidence["phase"] = serde_json::json!(phase);
        evidence["native_redraw_requests"] = serde_json::json!(redraws);
        evidence["parent_frames"] = serde_json::json!(parent_frames);
        evidence["input_frames"] = serde_json::json!(self.stability_input_frames);
        evidence["repaint_causes"] = serde_json::json!(self.stability_repaint_causes);
        self.report.lock().unwrap().text_stability.push(evidence);
        anyhow::ensure!(
            observed.max_changed_pixels <= 8 && observed.missing_ink_pixels <= 8,
            "Visible text flickered: changed={}, missing ink={}",
            observed.max_changed_pixels,
            observed.missing_ink_pixels
        );
        anyhow::ensure!(
            redraws == [0, 0],
            "Unchanged text triggered native redraws: {redraws:?}"
        );
        anyhow::ensure!(
            !stress || parent_frames >= 30,
            "Parent repaint stress did not run: {parent_frames} frames"
        );
        anyhow::ensure!(
            stress || self.stability_input_frames > 0 || parent_frames <= 10,
            "Idle result window is still repainting without input: {parent_frames} frames"
        );
        if matches!(self.stage, Stage::NativeContent) {
            self.advance(Stage::NativeScrolling);
        } else if stress {
            ctx.set_theme(egui::ThemePreference::Light);
            app.source = MIXED_CONTENT_SOURCE.into();
            app.translation = MIXED_CONTENT_SOURCE.into();
            app.native_editors.blur();
            self.advance(Stage::NativeContent);
        } else {
            ctx.set_theme(egui::ThemePreference::Dark);
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(egui::vec2(1060.0, 680.0)));
            app.translation = MULTILINGUAL_SOURCE.into();
            app.native_editors.blur();
            self.advance(Stage::NativeStress);
        }
        Ok(())
    }

    fn click_fixture(&mut self, ctx: &egui::Context, key: &'static str) -> Result<()> {
        let rect = ctx
            .data(|data| data.get_temp::<egui::Rect>(egui::Id::new(key)))
            .with_context(|| format!("Missing live fixture widget: {key}"))?;
        let pos = rect.center();
        self.queued_input.extend([
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        ctx.request_repaint();
        Ok(())
    }

    fn uses_visible_screenshot(&self) -> bool {
        matches!(
            self.stage,
            Stage::Korean
                | Stage::KoreanDark
                | Stage::Multilingual
                | Stage::MultilingualDark
                | Stage::Countries
                | Stage::CountriesTranslation
                | Stage::BaiduFormula
                | Stage::TencentFormula
                | Stage::NativeResult
                | Stage::NativeLanguageMenu
                | Stage::NativeLanguageChanged
        )
    }

    fn verify_live_display(
        &mut self,
        app: &App,
        ctx: &egui::Context,
        expected: &egui::ColorImage,
    ) -> Result<egui::ColorImage> {
        // Read the previous committed visible frame before requesting any native
        // diagnostic repaint. A black/stale front buffer must remain observable.
        let actual = live_display_smoke::capture(app.hwnd)?;
        let filename = match self.stage {
            Stage::NativeResult => "native-result-live.png",
            Stage::NativeLanguageMenu => "native-language-menu-live.png",
            Stage::NativeLanguageChanged => "native-language-changed-live.png",
            _ => unreachable!(),
        };
        let bytes: Vec<u8> = actual
            .pixels
            .iter()
            .flat_map(|pixel| pixel.to_array())
            .collect();
        image::save_buffer(
            self.directory.join(filename),
            &bytes,
            actual.width() as u32,
            actual.height() as u32,
            image::ColorType::Rgba8,
        )?;
        anyhow::ensure!(
            actual.size == expected.size,
            "Visible/framebuffer dimensions differ: {:?} / {:?}",
            actual.size,
            expected.size
        );
        let body = ctx
            .data(|data| data.get_temp::<egui::Rect>(egui::Id::new("smoke_source_body_rect")))
            .context("Missing source body geometry")?;
        let scale = ctx.pixels_per_point();
        let x0 = (body.min.x * scale).ceil().max(0.0) as usize;
        let y0 = (body.min.y * scale).ceil().max(0.0) as usize;
        let x1 = ((body.max.x * scale).floor() as usize).min(actual.width());
        let y1 = ((body.max.y * scale).floor() as usize).min(actual.height());
        anyhow::ensure!(x1 > x0 && y1 > y0, "Source body is empty");
        let p = super::theme::Palette::get(ctx);
        let distance = |a: egui::Color32, b: egui::Color32| {
            a.r()
                .abs_diff(b.r())
                .max(a.g().abs_diff(b.g()))
                .max(a.b().abs_diff(b.b()))
        };
        let mut background = 0usize;
        let mut left_ink = 0usize;
        let mut mismatched = 0usize;
        let mut popup_area = 0usize;
        for y in y0..y1 {
            for x in x0..x1 {
                let pixel = actual[(x, y)];
                background += usize::from(distance(pixel, p.panel) <= 3);
                let point = egui::pos2((x as f32 + 0.5) / scale, (y as f32 + 0.5) / scale);
                let in_popup = app
                    .native_editors
                    .popup_rects()
                    .iter()
                    .any(|rect| rect.contains(point));
                left_ink +=
                    usize::from(!in_popup && x < (x0 + x1) / 2 && distance(pixel, p.text) < 80);
                if in_popup {
                    popup_area += 1;
                    mismatched += usize::from(distance(pixel, expected[(x, y)]) > 12);
                }
            }
        }
        let area = (x1 - x0) * (y1 - y0);
        if matches!(self.stage, Stage::NativeLanguageMenu) {
            anyhow::ensure!(
                !app.native_editors.is_focused(0) && !app.native_editors.is_focused(1),
                "Language menu left keyboard focus in the result editor"
            );
            anyhow::ensure!(
                popup_area > 1_000,
                "Language menu has no native occlusion region"
            );
            anyhow::ensure!(mismatched * 100 < popup_area, "Visible menu differs from the GL frame: {mismatched}/{popup_area} pixels; native child may obscure the menu");
            anyhow::ensure!(
                left_ink > 1_000,
                "Opening the language menu hid the result body: left text={left_ink}"
            );
            self.report.lock().unwrap().native_language_menu_live = true;
        } else {
            anyhow::ensure!(background * 4 > area * 3 && left_ink > 1_000, "Result body is black, blank or stale: background={background}/{area}, left text={left_ink}");
            anyhow::ensure!(
                !app.busy && app.source == COUNTRY_SOURCE,
                "Recognition completion did not publish the expected result"
            );
            if matches!(self.stage, Stage::NativeResult) {
                self.report.lock().unwrap().native_result_live = true;
            } else {
                self.report.lock().unwrap().native_language_changed_live = true;
            }
        }
        Ok(actual)
    }

    fn screenshot(&mut self, ctx: &egui::Context, filename: &'static str) {
        if self.pending_screenshot.is_none() {
            self.pending_screenshot = Some(filename);
            ctx.send_viewport_cmd(ViewportCommand::Screenshot(egui::UserData::new(filename)));
        }
    }

    fn defer_hotkey_recording(&mut self, app: &mut App, ctx: &egui::Context) {
        // Foreground ownership can change while the user works. Do not steal it or
        // weaken production focus checks for a screenshot fixture. Unit tests cover
        // recording; retain an explicit deferred result for this foreground-only check.
        self.report
            .lock()
            .unwrap()
            .hotkey_recording_deferred_for_focus = true;
        super::hotkey_input::clear(ctx);
        app.draft = app.config.clone();
        app.settings_tab = 1;
        self.advance(Stage::Services);
        ctx.request_repaint();
    }

    fn stable(&self) -> bool {
        self.stage_frames >= 3 && self.since.elapsed() > Duration::from_millis(600)
    }

    fn record_hotkey_focus(&self, app: &App, ctx: &egui::Context, phase: &str) {
        // SAFETY: Compare an opaque foreground HWND to this smoke process's own window.
        let foreground =
            unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow() }
                == app.hwnd as _;
        let focused = ctx.input(|input| input.focused);
        let target = egui::Id::new("silent_capture_hotkey_recorder");
        let (widget, previous) = ctx.memory(|memory| {
            (
                memory.has_focus(target),
                memory.had_focus_last_frame(target),
            )
        });
        let recording = super::hotkey_input::is_recording(ctx);
        self.report.lock().unwrap().hotkey_focus_checks.push(format!("{phase}: egui={focused}, foreground={foreground}, widget={widget}, previous={previous}, recording={recording}"));
    }

    fn advance(&mut self, stage: Stage) {
        self.stage = stage;
        self.since = Instant::now();
        self.stage_frames = 0;
    }

    fn fail(&mut self, app: &mut App, ctx: &egui::Context, message: String) {
        self.report.lock().unwrap().errors.push(message);
        app.exiting = true;
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }

    fn check_startup_hide(&self, app: &App, ctx: &egui::Context) {
        let activation = app.sender.clone();
        let hwnd = app.hwnd;
        let context = ctx.clone();
        let report = self.report.clone();
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut hidden_since = None;
            let hidden = loop {
                // A missing window must not count as a successful hide.
                let hidden = own_window().is_some_and(|window| {
                    // SAFETY: own_window verified that the HWND belongs to this process.
                    unsafe { IsWindowVisible(window) == 0 }
                });
                if hidden {
                    let since = hidden_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= Duration::from_millis(120) {
                        break true;
                    }
                } else {
                    hidden_since = None;
                }
                if Instant::now() >= deadline {
                    break false;
                }
                thread::sleep(Duration::from_millis(20));
            };
            {
                let mut report = report.lock().unwrap();
                report.delayed_startup_hide = hidden;
                if !hidden {
                    report
                        .errors
                        .push("Delayed startup hide did not hide the rendered window".into());
                }
            }
            let _ = activation.send(Event::Platform(platform::PlatformEvent::Show));
            super::wake_ui(&context, hwnd);
        });
    }

    fn deliver_later(
        &self,
        app: &App,
        ctx: &egui::Context,
        id: u64,
        text: &'static str,
        cancelled: bool,
    ) {
        let sender = app.sender.clone();
        let hwnd = app.hwnd;
        let ctx = ctx.clone();
        let report = self.report.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(350));
            let hidden = !is_visible();
            if cancelled {
                report.lock().unwrap().cancelled_started_hidden = hidden;
            } else {
                report.lock().unwrap().accepted_started_hidden = hidden;
            }
            let _ = sender.send(Event::Completed(Output {
                id,
                recognized: Some(text.into()),
                ..Output::default()
            }));
            // Same wake-up path as the actual worker completion callback.
            super::wake_ui(&ctx, hwnd);
            // Give the hidden cancellation assertion its own timer-driven update.
            thread::sleep(Duration::from_millis(400));
            super::wake_ui(&ctx, hwnd);
        });
    }
}

fn own_window() -> Option<windows_sys::Win32::Foundation::HWND> {
    let title: Vec<u16> = TITLE.encode_utf16().chain(Some(0)).collect();
    // SAFETY: Title is NUL-terminated; querying an HWND does not dereference application pointers.
    let hwnd = unsafe { FindWindowW(null(), title.as_ptr()) };
    if hwnd.is_null() {
        return None;
    }
    let mut process = 0;
    // SAFETY: HWND was returned by FindWindowW and process is writable out-storage.
    unsafe { GetWindowThreadProcessId(hwnd, &mut process) };
    (process == std::process::id()).then_some(hwnd)
}

fn close_owned_window(app: &App, ctx: &egui::Context) -> Result<()> {
    let hwnd = own_window().context("Test window no longer exists")?;
    // SAFETY: own_window checked that this is the smoke process's main HWND.
    let posted = unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
    anyhow::ensure!(posted != 0, "Cannot post WM_CLOSE");
    let (context, handle) = (ctx.clone(), app.hwnd);
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(240));
        super::wake_ui(&context, handle);
    });
    Ok(())
}

fn hidden_window_survives() -> bool {
    own_window().is_some_and(|hwnd| {
        // SAFETY: Read-only state checks on the smoke process's live window.
        unsafe { IsWindowVisible(hwnd) == 0 && IsIconic(hwnd) == 0 }
    })
}

fn own_window_matches_size(expected: [f32; 2]) -> bool {
    let Some(hwnd) = own_window() else {
        return false;
    };
    // SAFETY: RECT is POD; Win32 writes its client rectangle to initialized storage.
    let mut client = unsafe { std::mem::zeroed() };
    // SAFETY: These read-only queries target the smoke process's live main HWND.
    unsafe {
        if GetClientRect(hwnd, &mut client) == 0 {
            return false;
        }
        let scale = windows_sys::Win32::UI::HiDpi::GetDpiForWindow(hwnd) as f32 / 96.0;
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        scale > 0.0
            && ((client.right - client.left) as f32 - expected[0] * scale).abs() <= 1.0
            && ((client.bottom - client.top) as f32 - expected[1] * scale).abs() <= 1.0
            && style & (WS_THICKFRAME | WS_MAXIMIZEBOX) == (WS_THICKFRAME | WS_MAXIMIZEBOX)
    }
}

fn is_visible() -> bool {
    own_window().is_some_and(|hwnd| {
        // SAFETY: The handle was verified as this process's smoke test window.
        unsafe { IsWindowVisible(hwnd) != 0 }
    })
}

fn is_foreground(hwnd: usize) -> bool {
    // SAFETY: Only compare the opaque foreground handle with this smoke window.
    unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow() == hwnd as _ }
}

fn own_window_is_centered() -> bool {
    let Some(hwnd) = own_window() else {
        return false;
    };
    // SAFETY: RECT/MONITORINFO are POD and their output sizes are initialized below.
    let mut rect = unsafe { std::mem::zeroed() };
    // SAFETY: MONITORINFO is POD; cbSize is set before passing it to user32.
    let mut monitor: MONITORINFO = unsafe { std::mem::zeroed() };
    monitor.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    // SAFETY: hwnd belongs to this test process and both out-parameters are live.
    unsafe {
        if GetWindowRect(hwnd, &mut rect) == 0
            || GetMonitorInfoW(
                MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST),
                &mut monitor,
            ) == 0
        {
            return false;
        }
    }
    let work = monitor.rcWork;
    let expected_x = work.left + ((work.right - work.left - (rect.right - rect.left)) / 2).max(0);
    let expected_y = work.top + ((work.bottom - work.top - (rect.bottom - rect.top)) / 2).max(0);
    (rect.left - expected_x).abs() <= 2 && (rect.top - expected_y).abs() <= 2
}
