//! Silent OCR lifecycle checks with synthetic pixels and an in-memory copy sink.
use super::{App, Event, Output, PlatformEvent, Progress, ProgressStage};
use anyhow::{anyhow, ensure, Context, Result};
use eframe::egui;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{GetWindowThreadProcessId, IsIconic, IsWindowVisible},
};

const SILENT: &str = "静默识别 smoke · only this text is copied";
const NORMAL: &str = "普通识别 smoke · window restored";
const POISON: &str = "STALE OR CANCELLED RESULT MUST NOT ESCAPE";
const CAPTURE_ERROR: &str = "synthetic capture failure";
const OCR_ERROR: &str = "synthetic OCR failure";

#[derive(Clone, Copy)]
enum Step {
    Preparing,
    Recognizing,
    Success,
    Escape,
    CaptureError,
    Cancelled,
    OcrError,
    Normal,
}

pub(super) struct Run {
    step: Step,
    since: Instant,
    started: Instant,
    prepared: mpsc::Receiver<std::result::Result<(), String>>,
    stop: mpsc::Sender<()>,
    watch: Arc<AtomicBool>,
    popup: Arc<AtomicBool>,
}

impl Run {
    pub(super) fn new(app: &mut App, ctx: &egui::Context) -> Self {
        super::hotkey_input::clear(ctx);
        app.source = "previous original".into();
        app.translation = "previous translation".into();
        app.capture_translate = true; // Silent must not change the ordinary toolbar mode.
        let initial = if let Some(copies) = &app.smoke_copies {
            copies.borrow_mut().clear();
            // Both intents are drained together: the later capture must revoke Show
            // before any restoration/focus command can reach the native window.
            let _ = app.sender.send(Event::Platform(PlatformEvent::Show));
            dispatch(app, ctx, Event::Platform(PlatformEvent::SilentOcr))
        } else {
            Err(anyhow!(
                "Silent smoke requires the independent clipboard sink"
            ))
        };
        let (ready, prepared) = mpsc::channel();
        let (stop, stopped) = mpsc::channel();
        let watch = Arc::new(AtomicBool::new(false));
        let popup = Arc::new(AtomicBool::new(false));
        let (watcher, detected) = (watch.clone(), popup.clone());
        let (context, hwnd) = (ctx.clone(), app.hwnd);
        thread::spawn(move || {
            let result = initial.and_then(|()| super::window_chrome::prepare_capture(hwnd));
            watcher.store(result.is_ok(), Ordering::Release);
            let _ = ready.send(result.map_err(|error| format!("{error:#}")));
            let deadline = Instant::now() + Duration::from_secs(12);
            loop {
                if watcher.load(Ordering::Acquire)
                    && !window_state(hwnd, true)
                    && watcher.load(Ordering::Acquire)
                {
                    detected.store(true, Ordering::Release);
                }
                super::wake_ui(&context, hwnd);
                if Instant::now() >= deadline
                    || !matches!(
                        stopped.recv_timeout(Duration::from_millis(15)),
                        Err(mpsc::RecvTimeoutError::Timeout)
                    )
                {
                    break;
                }
            }
        });
        Self {
            step: Step::Preparing,
            since: Instant::now(),
            started: Instant::now(),
            prepared,
            stop,
            watch,
            popup,
        }
    }

    pub(super) fn tick(&mut self, app: &mut App, ctx: &egui::Context) -> Result<bool> {
        ensure!(
            self.started.elapsed() < Duration::from_secs(10),
            "Silent smoke timed out"
        );
        ensure!(
            !self.popup.load(Ordering::Acquire),
            "A silent event displayed or restored the window"
        );
        if !matches!(self.step, Step::Preparing)
            && self.since.elapsed() < Duration::from_millis(180)
        {
            return Ok(false);
        }
        match self.step {
            Step::Preparing => {
                match self.prepared.try_recv() {
                    Ok(result) => result.map_err(|error| anyhow!(error))?,
                    Err(mpsc::TryRecvError::Empty) => return Ok(false),
                    Err(error) => return Err(error).context("Capture preparation watcher stopped"),
                }
                ensure!(
                    window_state(app.hwnd, true),
                    "Preparation did not keep the tray window hidden"
                );
                ensure!(
                    app.busy && app.capturing && app.silent_job,
                    "Silent platform action was not accepted"
                );
                let id = app.request_id;
                for action in [PlatformEvent::Ocr, PlatformEvent::Translate] {
                    dispatch(app, ctx, Event::Platform(action))?;
                }
                ensure!(
                    app.request_id == id && app.silent_job && app.capture_translate,
                    "Rejected busy action changed the silent request snapshot"
                );
                captured(app, ctx)?;
                progress(app, ctx, ProgressStage::Recognizing, None)?;
                self.advance(Step::Recognizing);
            }
            Step::Recognizing => {
                ensure!(
                    window_state(app.hwnd, true) && app.recognizing() && !app.capturing,
                    "Silent recognition did not stay hidden and busy"
                );
                ensure!(
                    app.source == "previous original" && app.translation == "previous translation",
                    "Recognition progress prematurely replaced text"
                );
                copies(app, &[])?;
                complete(app, ctx, Some(SILENT), None)?;
                self.advance(Step::Success);
            }
            Step::Success => {
                quiet(app)?;
                ensure!(
                    app.source_image_size == Some((2, 2)),
                    "Silent result lost capture dimensions"
                );
                let old_id = app.request_id;
                late_events(app, ctx, old_id)?; // Same-id duplicates after completion.
                quiet(app)?;
                dispatch(app, ctx, Event::Platform(PlatformEvent::SilentOcr))?;
                late_events(app, ctx, old_id)?; // Old request during a new capture.
                ensure!(
                    app.busy && app.capturing && app.silent_job && app.request_id != old_id,
                    "Stale event terminated the next silent capture"
                );
                capture_result(app, ctx, Ok(None), false)?;
                self.advance(Step::Escape);
            }
            Step::Escape => {
                quiet(app)?;
                ensure!(app.error.is_empty(), "Escape became an OCR error");
                dispatch(app, ctx, Event::Platform(PlatformEvent::SilentOcr))?;
                capture_result(app, ctx, Err(CAPTURE_ERROR.into()), false)?;
                self.advance(Step::CaptureError);
            }
            Step::CaptureError => {
                quiet(app)?;
                ensure!(
                    app.error == CAPTURE_ERROR,
                    "Silent capture error was not retained"
                );
                dispatch(app, ctx, Event::Platform(PlatformEvent::SilentOcr))?;
                captured(app, ctx)?;
                app.cancel_job();
                progress(app, ctx, ProgressStage::Translating, Some(POISON))?;
                complete(app, ctx, Some(POISON), None)?;
                self.advance(Step::Cancelled);
            }
            Step::Cancelled => {
                quiet(app)?;
                ensure!(app.cancelled, "Worker cancellation was lost");
                dispatch(app, ctx, Event::Platform(PlatformEvent::SilentOcr))?;
                captured(app, ctx)?;
                progress(app, ctx, ProgressStage::LocalFallback, None)?;
                ensure!(
                    app.job
                        .as_ref()
                        .is_some_and(|job| job.stage == ProgressStage::LocalFallback),
                    "Fallback progress did not reach the active job"
                );
                complete(app, ctx, None, Some(OCR_ERROR))?;
                self.advance(Step::OcrError);
            }
            Step::OcrError => {
                quiet(app)?;
                ensure!(app.error == OCR_ERROR, "Silent OCR error was not retained");
                let id = app.request_id;
                late_events(app, ctx, id)?;
                quiet(app)?;
                ensure!(
                    app.error == OCR_ERROR,
                    "Duplicate completion cleared the last error"
                );
                self.watch.store(false, Ordering::Release);
                dispatch(app, ctx, Event::Platform(PlatformEvent::Ocr))?;
                ensure!(
                    !app.silent_job && app.busy && app.capturing && !app.capture_translate,
                    "The next ordinary request inherited silent mode"
                );
                captured(app, ctx)?;
                complete(app, ctx, Some(NORMAL), None)?;
                self.advance(Step::Normal);
            }
            Step::Normal => {
                ensure!(
                    window_state(app.hwnd, false),
                    "Ordinary OCR failed to restore the hidden window"
                );
                ensure!(
                    !app.busy && !app.silent_job && app.job.is_none() && app.error.is_empty(),
                    "Ordinary OCR did not finish cleanly"
                );
                ensure!(
                    app.source == NORMAL && app.translation.is_empty(),
                    "Ordinary result was not applied"
                );
                copies(app, &[SILENT, NORMAL])?;
                let _ = self.stop.send(());
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn advance(&mut self, step: Step) {
        self.step = step;
        self.since = Instant::now();
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        // Do not join on the UI thread: preparation may still require it to pump messages.
        // The stop channel and deadline bound the detached helper's lifetime.
        let _ = self.stop.send(());
    }
}

fn dispatch(app: &mut App, ctx: &egui::Context, event: Event) -> Result<()> {
    app.sender
        .send(event)
        .context("Silent smoke event channel closed")?;
    app.events(ctx);
    Ok(())
}

fn captured(app: &mut App, ctx: &egui::Context) -> Result<()> {
    // A true translate flag must still be suppressed by the silent request snapshot.
    let translate = app.silent_job;
    capture_result(app, ctx, Ok(Some(image::RgbaImage::new(2, 2))), translate)?;
    ensure!(
        app.job.as_ref().is_some_and(|job| {
            job.stage == ProgressStage::Recognizing && (!app.silent_job || !job.translate_after_ocr)
        }),
        "Synthetic capture did not start recognition or silent mode requested translation"
    );
    Ok(())
}

fn capture_result(
    app: &mut App,
    ctx: &egui::Context,
    result: std::result::Result<Option<image::RgbaImage>, String>,
    translate: bool,
) -> Result<()> {
    let event = Event::Capture {
        id: app.request_id,
        translate,
        result,
    };
    dispatch(app, ctx, event)
}

fn progress(
    app: &mut App,
    ctx: &egui::Context,
    stage: ProgressStage,
    text: Option<&str>,
) -> Result<()> {
    let event = Event::Progress(Progress {
        id: app.request_id,
        stage,
        recognized: text.map(str::to_owned),
        warning: None,
    });
    dispatch(app, ctx, event)
}

fn complete(
    app: &mut App,
    ctx: &egui::Context,
    text: Option<&str>,
    error: Option<&str>,
) -> Result<()> {
    let event = Event::Completed(Output {
        id: app.request_id,
        recognized: text.map(str::to_owned),
        error: error.map(str::to_owned),
        ..Output::default()
    });
    dispatch(app, ctx, event)
}

fn late_events(app: &mut App, ctx: &egui::Context, id: u64) -> Result<()> {
    for event in [
        Event::Progress(Progress {
            id,
            stage: ProgressStage::Translating,
            recognized: Some(POISON.into()),
            warning: None,
        }),
        Event::Capture {
            id,
            translate: true,
            result: Err(POISON.into()),
        },
        Event::Completed(Output {
            id,
            recognized: Some(POISON.into()),
            ..Output::default()
        }),
    ] {
        dispatch(app, ctx, event)?;
    }
    Ok(())
}

fn copies(app: &App, expected: &[&str]) -> Result<()> {
    let copied = app
        .smoke_copies
        .as_ref()
        .context("Missing smoke clipboard sink")?
        .borrow();
    ensure!(
        copied.len() == expected.len()
            && copied.iter().zip(expected).all(|(a, b)| a.as_str() == *b),
        "Wrong copied text or copy count in silent flow"
    );
    Ok(())
}

fn quiet(app: &App) -> Result<()> {
    ensure!(
        window_state(app.hwnd, true),
        "Silent terminal event reopened the window"
    );
    ensure!(
        !app.busy && !app.capturing && app.job.is_none(),
        "Silent terminal event left a busy job"
    );
    ensure!(
        app.source == SILENT && app.translation.is_empty(),
        "Silent failure/cancel replaced the successful result"
    );
    copies(app, &[SILENT])
}

fn window_state(hwnd: usize, hidden: bool) -> bool {
    let hwnd = hwnd as HWND;
    let mut owner = 0;
    // SAFETY: Read-only HWND queries validate the handle, and owner is writable storage.
    // Every state check is restricted to this smoke process's own window.
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut owner) != 0
            && owner == std::process::id()
            && (IsWindowVisible(hwnd) == 0) == hidden
            && (hidden || IsIconic(hwnd) == 0)
    }
}
