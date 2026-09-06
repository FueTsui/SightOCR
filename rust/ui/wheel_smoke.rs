//! Exercise wheel routing in the existing, actually visible application window.
#![cfg(any(debug_assertions, test))]

use super::{live_display_smoke, App};
use anyhow::{ensure, Context, Result};
use eframe::egui::{self, ColorImage, Pos2, Rect, ViewportCommand};
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{HWND, POINT},
    Graphics::Gdi::ClientToScreen,
    System::Threading::GetCurrentProcessId,
    UI::{
        Input::KeyboardAndMouse::GetFocus,
        WindowsAndMessaging::{
            GetForegroundWindow, GetWindowThreadProcessId, IsIconic, IsWindowVisible, SendMessageW,
            WM_MOUSEWHEEL,
        },
    },
};

const SETTLE: Duration = Duration::from_millis(600);
const TIMEOUT: Duration = Duration::from_secs(20);
const DELTA: i16 = -360;

#[derive(Clone, Debug, Serialize)]
pub(super) struct Observation {
    pub positions: [(i32, i32); 2],
    pub counts: [u64; 2],
    pub parent_focused: bool,
    pub source_focused: bool,
    pub translation_focused: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct Step {
    pub name: &'static str,
    pub before: Observation,
    pub after: Observation,
    pub delivery_delta: [u64; 2],
    pub expected_editor: Option<usize>,
    pub recipient: usize,
    pub pointer_logical: [f32; 2],
    pub pointer_screen: [i32; 2],
    pub wheel_delta: i16,
    pub screenshot: PathBuf,
    pub passed: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct Report {
    pub passed: bool,
    pub duration_ms: u64,
    pub directory: PathBuf,
    pub before: PathBuf,
    pub initial: Option<Observation>,
    pub steps: Vec<Step>,
    pub errors: Vec<String>,
}

#[derive(Clone, Copy)]
enum Stage {
    Initial,
    Source,
    FocusSource,
    Translation,
    MenuReady,
    Menu,
    ResumeReady,
    Resumed,
    Finished,
}

struct Pending {
    name: &'static str,
    before: Observation,
    expected_editor: Option<usize>,
    recipient: usize,
    pointer_logical: [f32; 2],
    pointer_screen: [i32; 2],
    require_parent_focus: bool,
}

pub(super) struct Run {
    stage: Stage,
    since: Instant,
    started: Instant,
    queued_input: Vec<egui::Event>,
    pending: Option<Pending>,
    report: Option<Report>,
}

impl Run {
    pub(super) fn start(app: &mut App, ctx: &egui::Context, directory: PathBuf) -> Result<Self> {
        verify_window(app.hwnd as HWND)?;
        std::fs::create_dir_all(&directory).context("无法创建滚轮检查证据目录")?;
        ctx.set_theme(egui::ThemePreference::Light);
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(egui::vec2(1060.0, 680.0)));
        ctx.memory_mut(|memory| memory.close_popup());
        app.settings_open = false;
        app.busy = false;
        app.capturing = false;
        app.cancelled = false;
        app.job = None;
        app.editing_text = false;
        app.focus_source = false;
        app.focus_translation = false;
        app.compact_result_tab = 0;
        app.capture_translate = true;
        app.config.source_lang = "auto".into();
        app.config.target_lang = "en".into();
        app.source = fixture("原文 Source");
        app.translation = fixture("译文 Translation");
        app.warning.clear();
        app.error.clear();
        app.status = "滚轮检查 · 原文和译文各 100 行".into();
        app.native_editors.blur();
        ctx.request_repaint();
        let now = Instant::now();
        Ok(Self {
            stage: Stage::Initial,
            since: now,
            started: now,
            queued_input: Vec::new(),
            pending: None,
            report: Some(Report {
                passed: false,
                duration_ms: 0,
                before: directory.join("before.png"),
                directory,
                initial: None,
                steps: Vec::new(),
                errors: Vec::new(),
            }),
        })
    }

    /// Merge only into the existing smoke application's raw input hook.
    pub(super) fn append_input(&mut self, raw: &mut egui::RawInput) {
        raw.events.extend(self.take_input());
    }

    pub(super) fn take_input(&mut self) -> Vec<egui::Event> {
        std::mem::take(&mut self.queued_input)
    }

    /// Every action waits for subsequent painted frames; never sleeps or pumps
    /// messages recursively. The owning smoke driver may poll every 40 ms.
    pub(super) fn tick(&mut self, app: &mut App, ctx: &egui::Context) -> Option<Result<Report>> {
        if matches!(self.stage, Stage::Finished) {
            return None;
        }
        if self.started.elapsed() > TIMEOUT {
            return Some(self.finish(Some(anyhow::anyhow!("滚轮检查阶段超时"))));
        }
        if self.since.elapsed() < SETTLE {
            return None;
        }
        match self.advance(app, ctx) {
            Ok(false) => None,
            Ok(true) => Some(self.finish(None)),
            Err(error) => Some(self.finish(Some(error))),
        }
    }

    fn advance(&mut self, app: &mut App, ctx: &egui::Context) -> Result<bool> {
        verify_window(app.hwnd as HWND)?;
        match self.stage {
            Stage::Initial => {
                let source = body_rect(ctx, "smoke_source_body_rect")?;
                let translation = body_rect(ctx, "smoke_translation_body_rect")?;
                ensure!(
                    source.max.x < translation.min.x && source.height() > 100.0,
                    "滚轮检查未形成可见双栏正文"
                );
                let initial = observe(app);
                ensure!(initial.parent_focused, "初始键盘焦点未回到父窗口");
                let report = self.report.as_mut().unwrap();
                save_capture(app.hwnd, &report.before)?;
                report.initial = Some(initial);
                self.send_wheel(
                    app,
                    ctx,
                    "source",
                    app.hwnd as HWND,
                    source.center(),
                    Some(0),
                )?;
                self.next(Stage::Source);
            }
            Stage::Source => {
                self.record_step(app, "source.png")?;
                app.focus_source = true;
                ctx.request_repaint();
                self.next(Stage::FocusSource);
            }
            Stage::FocusSource => {
                ensure!(app.native_editors.is_focused(0), "无法建立原文编辑器焦点");
                // SAFETY: GetFocus only queries the calling UI thread; verify ownership
                // again before delivering a scalar message to that HWND.
                let focused = unsafe { GetFocus() };
                ensure!(focused != app.hwnd as HWND, "跨栏用例未聚焦原文子窗口");
                let translation = body_rect(ctx, "smoke_translation_body_rect")?;
                self.send_wheel(
                    app,
                    ctx,
                    "translation",
                    focused,
                    translation.center(),
                    Some(1),
                )?;
                self.next(Stage::Translation);
            }
            Stage::Translation => {
                self.record_step(app, "translation.png")?;
                let button = body_rect(ctx, "smoke_source_language_rect")?;
                self.queued_input.extend(click_events(button.center()));
                ctx.request_repaint();
                self.next(Stage::MenuReady);
            }
            Stage::MenuReady => {
                ensure!(
                    ctx.memory(|memory| memory.any_popup_open()),
                    "原文语言菜单未打开"
                );
                ensure!(
                    observe(app).parent_focused,
                    "语言菜单打开后焦点未回到父窗口"
                );
                let source = body_rect(ctx, "smoke_source_body_rect")?;
                let intersection = app
                    .native_editors
                    .popup_rects()
                    .iter()
                    .map(|popup| popup.intersect(source))
                    .find(|rect| rect.width() > 8.0 && rect.height() > 8.0)
                    .context("语言菜单与原文正文没有可测试的遮挡交集")?;
                self.send_wheel(
                    app,
                    ctx,
                    "menu",
                    app.hwnd as HWND,
                    intersection.center(),
                    None,
                )?;
                self.next(Stage::Menu);
            }
            Stage::Menu => {
                ensure!(
                    ctx.memory(|memory| memory.any_popup_open()),
                    "滚轮导致语言菜单意外关闭"
                );
                self.record_step(app, "menu.png")?;
                self.queued_input.extend(escape_events());
                ctx.request_repaint();
                self.next(Stage::ResumeReady);
            }
            Stage::ResumeReady => {
                ensure!(
                    !ctx.memory(|memory| memory.any_popup_open())
                        && app.native_editors.popup_rects().is_empty(),
                    "Escape 未关闭语言菜单或清除正文遮挡"
                );
                let source = body_rect(ctx, "smoke_source_body_rect")?;
                self.send_wheel(
                    app,
                    ctx,
                    "resumed",
                    app.hwnd as HWND,
                    source.center(),
                    Some(0),
                )?;
                self.next(Stage::Resumed);
            }
            Stage::Resumed => {
                self.record_step(app, "resumed.png")?;
                return Ok(true);
            }
            Stage::Finished => return Ok(true),
        }
        Ok(false)
    }

    fn next(&mut self, stage: Stage) {
        self.stage = stage;
        self.since = Instant::now();
    }

    fn send_wheel(
        &mut self,
        app: &App,
        ctx: &egui::Context,
        name: &'static str,
        recipient: HWND,
        pos: Pos2,
        expected_editor: Option<usize>,
    ) -> Result<()> {
        verify_owned(recipient)?;
        let scale = ctx.pixels_per_point();
        let mut screen = POINT {
            x: (pos.x * scale).round() as i32,
            y: (pos.y * scale).round() as i32,
        };
        // The app UI thread uses physical-pixel DPI awareness. WM_MOUSEWHEEL
        // requires signed screen coordinates, even when sent to a child control.
        ensure!(
            // SAFETY: Convert a local point using our verified application's HWND.
            unsafe { ClientToScreen(app.hwnd as HWND, &mut screen) } != 0,
            "无法换算滚轮屏幕坐标"
        );
        let lparam = wheel_lparam(screen.x, screen.y)?;
        let before = observe(app);
        self.pending = Some(Pending {
            name,
            before,
            expected_editor,
            recipient: recipient as usize,
            pointer_logical: [pos.x, pos.y],
            pointer_screen: [screen.x, screen.y],
            require_parent_focus: recipient == app.hwnd as HWND,
        });
        // SAFETY: Only this verified application's UI HWND receives one synchronous wheel
        // message. No global input, cursor movement or other window is involved.
        unsafe {
            SendMessageW(
                recipient,
                WM_MOUSEWHEEL,
                (DELTA as u16 as usize) << 16,
                lparam,
            );
        }
        Ok(())
    }

    fn record_step(&mut self, app: &App, filename: &str) -> Result<()> {
        let pending = self.pending.take().context("滚轮检查缺少发送前状态")?;
        let after = observe(app);
        let delta = [
            after.counts[0].wrapping_sub(pending.before.counts[0]),
            after.counts[1].wrapping_sub(pending.before.counts[1]),
        ];
        let passed = expected_change(&pending.before, &after, pending.expected_editor)
            && (!pending.require_parent_focus
                || (pending.before.parent_focused && after.parent_focused));
        let report = self.report.as_mut().unwrap();
        let screenshot = report.directory.join(filename);
        save_capture(app.hwnd, &screenshot)?;
        if !passed {
            report.errors.push(format!(
                "{} 滚轮路由失败：位置 {:?} -> {:?}；投递增量 {:?}；父窗口焦点 {} -> {}",
                pending.name,
                pending.before.positions,
                after.positions,
                delta,
                pending.before.parent_focused,
                after.parent_focused
            ));
        }
        report.steps.push(Step {
            name: pending.name,
            before: pending.before,
            after,
            delivery_delta: delta,
            expected_editor: pending.expected_editor,
            recipient: pending.recipient,
            pointer_logical: pending.pointer_logical,
            pointer_screen: pending.pointer_screen,
            wheel_delta: DELTA,
            screenshot,
            passed,
        });
        Ok(())
    }

    fn finish(&mut self, error: Option<anyhow::Error>) -> Result<Report> {
        self.stage = Stage::Finished;
        self.queued_input.clear();
        let mut report = self.report.take().context("滚轮报告已提取")?;
        if let Some(error) = error {
            report.errors.push(format!("{error:#}"));
        }
        report.duration_ms = self.started.elapsed().as_millis() as u64;
        report.passed = report.errors.is_empty()
            && report.steps.len() == 4
            && report.steps.iter().all(|step| step.passed);
        let path = report.directory.join("report.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&report)?)
            .with_context(|| format!("无法保存滚轮报告 {}", path.display()))?;
        ensure!(
            report.passed,
            "滚轮检查未通过：{}；证据：{}",
            report.errors.join("；"),
            path.display()
        );
        Ok(report)
    }
}

fn fixture(label: &str) -> String {
    (1..=100).map(|line| format!(
        "{line:03} · {label} · 2026-09-06 14:43:{:02} · 안녕하세요 / 한 · العربية عُمان · Français Côte d’Ivoire · हिन्दी नमस्ते · ภาษาไทย สวัสดี · 中文 日期与多语言滚动测试。\n",
        line % 60
    )).collect()
}

fn body_rect(ctx: &egui::Context, key: &'static str) -> Result<Rect> {
    ctx.data(|data| data.get_temp::<Rect>(egui::Id::new(key)))
        .with_context(|| format!("滚轮检查缺少已绘制控件坐标：{key}"))
}

fn observe(app: &App) -> Observation {
    Observation {
        positions: app.native_editors.scroll_positions(),
        counts: app.native_editors.wheel_delivery_counts(),
        // SAFETY: Read only the calling UI thread's focused HWND.
        parent_focused: unsafe { GetFocus() } == app.hwnd as HWND,
        source_focused: app.native_editors.is_focused(0),
        translation_focused: app.native_editors.is_focused(1),
    }
}

fn expected_change(before: &Observation, after: &Observation, expected: Option<usize>) -> bool {
    (0..2).all(|index| {
        let delivery = after.counts[index].wrapping_sub(before.counts[index]);
        if expected == Some(index) {
            delivery == 1
                && after.positions[index].0 == before.positions[index].0
                && after.positions[index].1 > before.positions[index].1
        } else {
            delivery == 0 && after.positions[index] == before.positions[index]
        }
    })
}

fn wheel_lparam(x: i32, y: i32) -> Result<isize> {
    let x = i16::try_from(x).context("滚轮 X 坐标超出 Win32 消息范围")?;
    let y = i16::try_from(y).context("滚轮 Y 坐标超出 Win32 消息范围")?;
    Ok(((x as u16 as u32) | ((y as u16 as u32) << 16)) as isize)
}

fn click_events(pos: Pos2) -> [egui::Event; 3] {
    [
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
    ]
}

fn escape_events() -> [egui::Event; 2] {
    [true, false].map(|pressed| egui::Event::Key {
        key: egui::Key::Escape,
        physical_key: Some(egui::Key::Escape),
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    })
}

fn verify_owned(hwnd: HWND) -> Result<()> {
    ensure!(!hwnd.is_null(), "滚轮检查窗口不存在");
    let mut pid = 0;
    // SAFETY: A window query writes only to this local process-ID storage.
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut pid);
    }
    ensure!(
        // SAFETY: This scalar query returns the current process identity.
        pid == unsafe { GetCurrentProcessId() },
        "滚轮检查拒绝操作其他进程的窗口"
    );
    Ok(())
}

fn verify_window(hwnd: HWND) -> Result<()> {
    verify_owned(hwnd)?;
    // SAFETY: Query only visibility and minimized state of the verified window.
    let visible = unsafe { IsWindowVisible(hwnd) != 0 && IsIconic(hwnd) == 0 };
    ensure!(visible, "滚轮检查需要本程序可见且未最小化的窗口");
    // SAFETY: Read the foreground handle, then verify its ownership before any use.
    verify_owned(unsafe { GetForegroundWindow() })?;
    Ok(())
}

fn save_capture(hwnd: usize, path: &Path) -> Result<()> {
    let image: ColorImage = live_display_smoke::capture(hwnd)?;
    let bytes: Vec<u8> = image
        .pixels
        .iter()
        .flat_map(|pixel| pixel.to_array())
        .collect();
    image::save_buffer_with_format(
        path,
        &bytes,
        image.width() as u32,
        image.height() as u32,
        image::ColorType::Rgba8,
        image::ImageFormat::Png,
    )
    .with_context(|| format!("无法保存滚轮实窗截图 {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_delivery_requires_only_hovered_editor_to_scroll_once() {
        let before = Observation {
            positions: [(0, 25), (0, 50)],
            counts: [7, 11],
            parent_focused: true,
            source_focused: false,
            translation_focused: false,
        };
        let mut after = before.clone();
        after.positions[1].1 += 100;
        after.counts[1] += 1;
        assert!(expected_change(&before, &after, Some(1)));
        assert!(!expected_change(&before, &after, Some(0)));
        assert!(!expected_change(&before, &after, None));
        after.counts[1] += 1;
        assert!(!expected_change(&before, &after, Some(1)));
        assert!(expected_change(&before, &before, None));
    }

    #[test]
    fn wheel_coordinates_keep_negative_monitor_origins_and_fixture_has_100_lines() {
        let packed = wheel_lparam(-320, 144).unwrap();
        assert_eq!(packed as u16 as i16, -320);
        assert_eq!((packed >> 16) as u16 as i16, 144);
        assert!(wheel_lparam(40_000, 0).is_err());
        let text = fixture("原文");
        assert_eq!(text.lines().count(), 100);
        assert!(text.starts_with("001 ·"));
        assert!(text.lines().last().unwrap().starts_with("100 ·"));
    }
}
