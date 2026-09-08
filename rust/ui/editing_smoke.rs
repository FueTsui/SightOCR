//! Exercise the real App layout, native focus, and queued keyboard input.
//! No desktop input, system clipboard, or production configuration is changed.

use super::App;
use anyhow::{ensure, Context, Result};
use eframe::egui::{self, ViewportCommand};
use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::HWND,
    UI::{
        Input::KeyboardAndMouse::GetFocus,
        WindowsAndMessaging::{
            GetDlgItem, IsWindowVisible, PostMessageW, SendMessageW, WM_CHAR, WM_CLEAR, WM_KEYDOWN,
            WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP,
        },
    },
};

/// Run the same production App without the full suite's foreground screenshots.
/// This isolates input regressions while another app legitimately has foreground.
pub(super) fn run(directory: PathBuf) -> Result<()> {
    use sightocr::config::Config;

    std::fs::create_dir_all(&directory)?;
    let scratch = tempfile::tempdir()?;
    let path = scratch.path().join("config.json");
    let output = Rc::new(RefCell::new(None));
    let result_output = output.clone();
    sightocr::platform::set_dpi_awareness();
    eframe::run_native(
        "SightOCR Editing Smoke",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size([1060.0, 680.0]),
            ..Default::default()
        },
        Box::new(move |cc| {
            let config = Config {
                hotkey: "Ctrl+Alt+Shift+F23".into(),
                translate_hotkey: "Ctrl+Alt+Shift+F24".into(),
                silent_hotkey: "Ctrl+Alt+Shift+F22".into(),
                ..Config::default()
            };
            Ok(Box::new(EditingApp {
                app: App::new(cc, config, path)?,
                run: None,
                output: result_output,
                started: Instant::now(),
            }))
        }),
    )
    .map_err(|error| anyhow::anyhow!("Cannot run editing smoke: {error}"))?;
    let outcome = output
        .borrow_mut()
        .take()
        .context("Editing smoke closed before completion")?;
    let report = match &outcome {
        Ok(checks) => {
            serde_json::json!({"scope": "native-editing", "passed": true, "checks": checks})
        }
        Err(error) => {
            serde_json::json!({"scope": "native-editing", "passed": false, "error": error})
        }
    };
    std::fs::write(
        directory.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    outcome.map(|_| ()).map_err(anyhow::Error::msg)
}

type EditingOutcome = Rc<RefCell<Option<std::result::Result<Vec<&'static str>, String>>>>;

struct EditingApp {
    app: App,
    run: Option<Run>,
    output: EditingOutcome,
    started: Instant,
}

impl eframe::App for EditingApp {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, input: &mut egui::RawInput) {
        if let Some(run) = &mut self.run {
            run.append_input(input);
        }
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.app.update(ctx, frame);
        if self.output.borrow().is_some() {
            return;
        }
        if self.run.is_none() {
            self.run = Some(Run::start(&mut self.app, ctx));
        }
        let result = if self.started.elapsed() > Duration::from_secs(20) {
            Some(Err(anyhow::anyhow!("Editing smoke timed out")))
        } else {
            self.run.as_mut().unwrap().tick(&mut self.app, ctx)
        };
        if let Some(result) = result {
            *self.output.borrow_mut() = Some(result.map_err(|error| format!("{error:#}")));
            self.app.exiting = true;
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
        ctx.request_repaint_after(Duration::from_millis(20));
    }
}

pub(super) struct Run {
    phase: usize,
    since: Instant,
    queued: Vec<egui::Event>,
    checks: Vec<&'static str>,
}

impl Run {
    pub(super) fn start(app: &mut App, ctx: &egui::Context) -> Self {
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(egui::vec2(1060.0, 680.0)));
        ctx.memory_mut(|memory| memory.close_popup());
        app.native_editors.blur();
        app.busy = false;
        app.capturing = false;
        app.settings_open = false;
        app.editing_text = false;
        app.focus_source = false;
        app.focus_translation = false;
        app.source.clear();
        app.translation = "existing translation".into();
        Self {
            phase: 0,
            since: Instant::now(),
            queued: Vec::new(),
            checks: Vec::new(),
        }
    }

    pub(super) fn append_input(&mut self, input: &mut egui::RawInput) {
        input.events.append(&mut self.queued);
    }

    pub(super) fn tick(
        &mut self,
        app: &mut App,
        ctx: &egui::Context,
    ) -> Option<Result<Vec<&'static str>>> {
        if !self.queued.is_empty() {
            ctx.request_repaint();
            return None;
        }
        // A click is checked in the very frame that consumes the queued egui
        // event; waiting an extra frame would hide the empty-state focus bug.
        let immediate = matches!(self.phase, 1 | 4);
        if !immediate && self.since.elapsed() < Duration::from_millis(200) {
            ctx.request_repaint_after(Duration::from_millis(20));
            return None;
        }
        let result = self.step(app, ctx);
        self.since = Instant::now();
        self.phase += 1;
        ctx.request_repaint();
        match result {
            Err(error) => Some(Err(error)),
            Ok(true) => Some(Ok(std::mem::take(&mut self.checks))),
            Ok(false) => None,
        }
    }

    fn step(&mut self, app: &mut App, ctx: &egui::Context) -> Result<bool> {
        match self.phase {
            0 => self.click(ctx, "smoke_source_body_rect")?,
            1 => {
                ensure!(
                    app.native_editors.is_focused(0),
                    "Source placeholder did not focus its editor in the click frame"
                );
                self.checks.push("source_placeholder_focus_in_click_frame");
                queue_text(focused_editor(app, 0)?, "Manual 原文\nsecond line")?;
            }
            2 => {
                ensure!(
                    app.source == "Manual 原文\nsecond line",
                    "Source keyboard input did not reach the App model: {:?}",
                    app.source
                );
                self.checks.push("source_keyboard_newline_and_unicode_sync");
                app.native_editors.blur();
                app.translation.clear();
                app.editing_text = false;
            }
            3 => self.click(ctx, "smoke_translation_body_rect")?,
            4 => {
                ensure!(
                    app.native_editors.is_focused(1),
                    "Translation placeholder did not focus its editor in the click frame"
                );
                self.checks
                    .push("translation_placeholder_focus_in_click_frame");
                queue_text(focused_editor(app, 1)?, "Manual 译文\nالعربية")?;
            }
            5 => {
                ensure!(
                    app.translation == "Manual 译文\nالعربية",
                    "Translation keyboard input did not reach the App model: {:?}",
                    app.translation
                );
                self.checks
                    .push("translation_keyboard_newline_and_unicode_sync");
                let child = focused_editor(app, 1)?;
                // SAFETY: Select and clear only this smoke App's document.
                unsafe {
                    SendMessageW(child, 0x00B1, 0, -1);
                    SendMessageW(child, WM_CLEAR, 0, 0);
                }
                app.native_editors.blur();
            }
            6 => {
                ensure!(
                    app.translation.is_empty(),
                    "Clearing translation did not reach the model"
                );
                let child = editor(app, 1)?;
                // SAFETY: Query and post input only to the verified smoke child.
                unsafe {
                    ensure!(
                        IsWindowVisible(child) != 0,
                        "An empty edited translation collapsed after losing focus"
                    );
                    PostMessageW(child, WM_LBUTTONDOWN, 1, 8 | (8 << 16));
                    PostMessageW(child, WM_LBUTTONUP, 0, 8 | (8 << 16));
                }
                self.checks
                    .push("empty_translation_remains_editable_after_blur");
            }
            7 => queue_text(focused_editor(app, 1)?, "Retyped 重新输入")?,
            8 => {
                ensure!(
                    app.translation == "Retyped 重新输入",
                    "Cleared translation could not be edited again"
                );
                self.checks.push("native_click_retyping_after_clear");
                app.native_editors.blur();
                app.editing_text = false;
                return Ok(true);
            }
            _ => unreachable!(),
        }
        Ok(false)
    }

    fn click(&mut self, ctx: &egui::Context, key: &'static str) -> Result<()> {
        let pos = ctx
            .data(|data| data.get_temp::<egui::Rect>(egui::Id::new(key)))
            .with_context(|| format!("Missing editing fixture geometry: {key}"))?
            .center();
        self.queued.extend([
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
        Ok(())
    }
}

fn editor(app: &App, index: usize) -> Result<HWND> {
    // SAFETY: App owns this parent and NativeEditors' fixed child identifiers.
    let child = unsafe { GetDlgItem(app.hwnd as HWND, 0x5340 + index as i32) };
    ensure!(!child.is_null(), "Missing native editor {index}");
    Ok(child)
}

fn focused_editor(app: &App, index: usize) -> Result<HWND> {
    let child = editor(app, index)?;
    // SAFETY: GetFocus reads the current smoke UI thread only.
    let focused = unsafe { GetFocus() };
    ensure!(focused == child, "Keyboard focus is not editor {index}");
    Ok(child)
}

fn queue_text(child: HWND, text: &str) -> Result<()> {
    // The real winit message pump performs keyboard translation and dispatch.
    // RichEdit handles Return on WM_KEYDOWN; Unicode text arrives as WM_CHAR.
    for unit in text.encode_utf16() {
        // SAFETY: Input is scoped to the verified focused smoke editor. Nothing
        // is injected into the desktop queue or another application's window.
        unsafe {
            if unit == b'\n' as u16 {
                ensure!(
                    PostMessageW(child, WM_KEYDOWN, 0x0D, 1 | (0x1C << 16)) != 0,
                    "Cannot queue Return"
                );
                PostMessageW(child, WM_KEYUP, 0x0D, 1 | (0x1C << 16) | (3isize << 30));
            } else {
                ensure!(
                    PostMessageW(child, WM_CHAR, unit as usize, 1) != 0,
                    "Cannot queue text input"
                );
            }
        }
    }
    Ok(())
}
