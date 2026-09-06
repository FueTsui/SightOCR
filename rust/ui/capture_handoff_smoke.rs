//! Exercise the production capture handoff without reading desktop pixels.
use super::{App, Event};
use anyhow::{anyhow, ensure, Context, Result};
use eframe::egui;
use serde::Serialize;
use sightocr::{platform::PlatformEvent, worker::Output};
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

const ORIGINAL: &str = "Synthetic capture handoff original";
const TRANSLATION: &str = "Synthetic capture handoff translation";

#[derive(Clone, Copy)]
enum Action {
    Translate,
    Silent,
    Recognize,
}

#[derive(Clone, Copy)]
struct Case {
    name: &'static str,
    action: Action,
    hidden: bool,
    queued_show: bool,
    cancel: bool,
}

const CASES: [Case; 6] = [
    Case {
        name: "F2 visible",
        action: Action::Translate,
        hidden: false,
        queued_show: false,
        cancel: false,
    },
    Case {
        name: "F2 background",
        action: Action::Translate,
        hidden: true,
        queued_show: false,
        cancel: false,
    },
    Case {
        name: "F2 after Show in discarded pass",
        action: Action::Translate,
        hidden: true,
        queued_show: true,
        cancel: false,
    },
    Case {
        name: "F2 selection cancelled",
        action: Action::Translate,
        hidden: false,
        queued_show: false,
        cancel: true,
    },
    Case {
        name: "F4 silent background",
        action: Action::Silent,
        hidden: true,
        queued_show: false,
        cancel: false,
    },
    Case {
        name: "F5 ordinary visible",
        action: Action::Recognize,
        hidden: false,
        queued_show: false,
        cancel: false,
    },
];

#[derive(Clone, Serialize)]
pub(super) struct Observation {
    case: &'static str,
    request_frame: u64,
    probe_received_frame: u64,
    same_frame_second_pass_seen: bool,
    show_during_capture_kept_hidden: bool,
    terminal_visibility_correct: bool,
}

enum Step {
    Settle,
    ShowDiscard,
    AwaitPrepared,
    Holding,
    AwaitFinish,
}

pub(super) struct Run {
    index: usize,
    step: Step,
    since: Instant,
    started: Instant,
    setup_frame: u64,
    request_frame: u64,
    probe_received_frame: u64,
    request_id: u64,
    saw_second_pass: bool,
    prior_source: String,
    prior_translation: String,
    prior_copies: usize,
    wake_stop: Arc<AtomicBool>,
    monitor: Option<Monitor>,
    observations: Vec<Observation>,
}

impl Run {
    pub(super) fn new(app: &mut App, ctx: &egui::Context) -> Result<Self> {
        ensure!(
            app.smoke_copies.is_some(),
            "Capture handoff requires the smoke copy sink"
        );
        ensure!(
            !app.busy,
            "Capture handoff started with an unfinished request"
        );
        super::super::hotkey_input::clear(ctx);
        app.settings_open = false;
        let wake_stop = Arc::new(AtomicBool::new(false));
        let stopped = wake_stop.clone();
        let (context, hwnd) = (ctx.clone(), app.hwnd);
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            while !stopped.load(Ordering::Acquire) && Instant::now() < deadline {
                super::super::wake_ui(&context, hwnd);
                thread::sleep(Duration::from_millis(20));
            }
        });
        let mut run = Self {
            index: 0,
            step: Step::Settle,
            since: Instant::now(),
            started: Instant::now(),
            setup_frame: app.ui_frame,
            request_frame: 0,
            probe_received_frame: 0,
            request_id: 0,
            saw_second_pass: false,
            prior_source: String::new(),
            prior_translation: String::new(),
            prior_copies: 0,
            wake_stop,
            monitor: None,
            observations: Vec::new(),
        };
        run.setup(app, ctx);
        Ok(run)
    }

    pub(super) fn observations(&self) -> Vec<Observation> {
        self.observations.clone()
    }

    pub(super) fn tick(&mut self, app: &mut App, ctx: &egui::Context) -> Result<bool> {
        let case = CASES[self.index];
        ensure!(
            self.started.elapsed() < Duration::from_secs(18),
            "Capture handoff timed out in {}",
            case.name
        );
        ensure!(
            self.since.elapsed() < Duration::from_secs(4),
            "Capture handoff stage stalled in {}",
            case.name
        );
        if let Some(monitor) = &self.monitor {
            ensure!(
                !monitor.reappeared.load(Ordering::Acquire),
                "{}: main HWND reappeared after native preparation",
                case.name
            );
            ensure!(
                !monitor.duplicate.load(Ordering::Acquire),
                "{}: capture preparation ran more than once",
                case.name
            );
        }
        match self.step {
            Step::Settle => {
                if self.since.elapsed() < Duration::from_millis(140)
                    || app.ui_frame < self.setup_frame + 2
                    || ctx.output(|output| output.num_completed_passes) != 0
                {
                    return Ok(false);
                }
                let (visible, minimized) = window_state(app.hwnd)?;
                ensure!(
                    visible != case.hidden && (case.hidden || !minimized),
                    "{}: initial window state did not settle",
                    case.name
                );
                if case.queued_show {
                    dispatch(app, ctx, Event::Platform(PlatformEvent::Show))?;
                    self.request_frame = app.ui_frame;
                    self.advance(Step::ShowDiscard);
                    ctx.request_discard("Queue Show before F2 in another pass of the same frame");
                } else {
                    self.start(app, ctx)?;
                    ctx.request_discard(
                        "Capture must not start in a second pass of the request frame",
                    );
                }
            }
            Step::ShowDiscard => {
                ensure!(
                    app.ui_frame == self.request_frame,
                    "Discarded Show pass advanced the real UI frame"
                );
                ensure!(
                    ctx.output(|output| output.num_completed_passes) > 0,
                    "Show/F2 regression did not execute a second layout pass"
                );
                self.start(app, ctx)?;
            }
            Step::AwaitPrepared => {
                if app.ui_frame == self.request_frame {
                    ensure!(
                        app.pending_capture.is_some(),
                        "{}: capture thread launched during the request frame",
                        case.name
                    );
                }
                if app.ui_frame == self.request_frame
                    && ctx.output(|output| output.num_completed_passes) > 0
                {
                    self.saw_second_pass = true;
                }
                let monitor = self
                    .monitor
                    .as_ref()
                    .context("Missing capture preparation monitor")?;
                match monitor.prepared.try_recv() {
                    Ok(result) => {
                        result.map_err(|error| anyhow!(error))?;
                        ensure!(
                            app.ui_frame > self.request_frame,
                            "{}: native capture prepared within its request frame",
                            case.name
                        );
                        ensure!(
                            self.saw_second_pass,
                            "{}: request did not span multiple layout passes",
                            case.name
                        );
                        ensure!(
                            !window_state(app.hwnd)?.0,
                            "{}: prepared HWND is visible",
                            case.name
                        );
                        ensure!(
                            app.busy && app.capturing && app.request_id == self.request_id,
                            "{}: production capture state was not retained",
                            case.name
                        );
                        self.probe_received_frame = app.ui_frame;
                        // A queued activation while selecting must not surface the main window.
                        dispatch(app, ctx, Event::Platform(PlatformEvent::Show))?;
                        self.advance(Step::Holding);
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                    Err(error) => {
                        return Err(error).context("Native preparation probe disconnected")
                    }
                }
            }
            Step::Holding => {
                ensure!(
                    !window_state(app.hwnd)?.0 && app.capturing,
                    "{}: Show restored the window during capture",
                    case.name
                );
                if self.since.elapsed() < Duration::from_millis(260)
                    || app.ui_frame < self.probe_received_frame + 3
                {
                    return Ok(false);
                }
                self.monitor
                    .as_ref()
                    .expect("monitor initialized")
                    .stop
                    .store(true, Ordering::Release);
                app.smoke_capture_probe = None;
                let translate = matches!(case.action, Action::Translate);
                dispatch(
                    app,
                    ctx,
                    Event::Capture {
                        id: self.request_id,
                        translate,
                        result: Ok((!case.cancel).then(|| image::RgbaImage::new(2, 2))),
                    },
                )?;
                if !case.cancel {
                    ensure!(
                        app.job
                            .as_ref()
                            .is_some_and(|job| job.translate_after_ocr == translate),
                        "{}: capture mode did not reach recognition",
                        case.name
                    );
                    dispatch(
                        app,
                        ctx,
                        Event::Completed(Output {
                            id: self.request_id,
                            recognized: Some(ORIGINAL.into()),
                            translated: translate.then(|| TRANSLATION.into()),
                            ..Output::default()
                        }),
                    )?;
                }
                self.advance(Step::AwaitFinish);
            }
            Step::AwaitFinish => {
                if self.since.elapsed() < Duration::from_millis(180) {
                    return Ok(false);
                }
                let silent = matches!(case.action, Action::Silent);
                let (visible, minimized) = window_state(app.hwnd)?;
                ensure!(
                    visible != silent && (silent || !minimized),
                    "{}: result/cancel restored the wrong window state",
                    case.name
                );
                ensure!(
                    !app.busy && !app.capturing && app.job.is_none() && app.error.is_empty(),
                    "{}: terminal capture state is not clean",
                    case.name
                );
                let copies = app
                    .smoke_copies
                    .as_ref()
                    .context("Copy sink was removed")?
                    .borrow();
                if case.cancel {
                    ensure!(
                        app.source == self.prior_source
                            && app.translation == self.prior_translation
                            && copies.len() == self.prior_copies,
                        "Cancelled selection changed text or copied a result"
                    );
                } else {
                    let expected = if matches!(case.action, Action::Translate) {
                        TRANSLATION
                    } else {
                        ORIGINAL
                    };
                    ensure!(
                        copies.len() == self.prior_copies + 1
                            && copies.last().is_some_and(|copy| copy == expected),
                        "{}: synthetic result did not use the isolated copy sink",
                        case.name
                    );
                    ensure!(
                        app.source_image_size == Some((2, 2)),
                        "{}: synthetic dimensions were lost",
                        case.name
                    );
                }
                drop(copies);
                self.observations.push(Observation {
                    case: case.name,
                    request_frame: self.request_frame,
                    probe_received_frame: self.probe_received_frame,
                    same_frame_second_pass_seen: self.saw_second_pass,
                    show_during_capture_kept_hidden: true,
                    terminal_visibility_correct: true,
                });
                self.monitor = None;
                self.index += 1;
                if self.index == CASES.len() {
                    return Ok(true);
                }
                self.setup(app, ctx);
            }
        }
        ctx.request_repaint_after(Duration::from_millis(25));
        Ok(false)
    }

    fn setup(&mut self, app: &mut App, ctx: &egui::Context) {
        self.setup_frame = app.ui_frame;
        if CASES[self.index].hidden {
            app.hide_in_background(ctx);
        } else {
            app.show(ctx);
        }
        self.advance(Step::Settle);
    }

    fn start(&mut self, app: &mut App, ctx: &egui::Context) -> Result<()> {
        let case = CASES[self.index];
        self.prior_source = app.source.clone();
        self.prior_translation = app.translation.clone();
        self.prior_copies = app
            .smoke_copies
            .as_ref()
            .context("Copy sink missing")?
            .borrow()
            .len();
        self.request_frame = app.ui_frame;
        self.saw_second_pass = ctx.output(|output| output.num_completed_passes) > 0;
        let (sender, receiver) = mpsc::channel();
        self.monitor = Some(Monitor::new(receiver, app.hwnd, ctx.clone()));
        app.smoke_capture_probe = Some(sender);
        let action = match case.action {
            Action::Translate => PlatformEvent::Translate,
            Action::Silent => PlatformEvent::SilentOcr,
            Action::Recognize => PlatformEvent::Ocr,
        };
        dispatch(app, ctx, Event::Platform(action))?;
        ensure!(
            app.busy && app.capturing,
            "{}: shortcut did not enter production start_capture",
            case.name
        );
        self.request_id = app.request_id;
        ensure!(
            app.pending_capture
                .as_ref()
                .is_some_and(|pending| pending.frame == self.request_frame),
            "{}: production capture was not deferred to another real frame",
            case.name
        );
        ensure!(
            matches!(
                self.monitor
                    .as_ref()
                    .expect("monitor initialized")
                    .prepared
                    .try_recv(),
                Err(mpsc::TryRecvError::Empty)
            ),
            "{}: preparation ran synchronously in the request pass",
            case.name
        );
        self.advance(Step::AwaitPrepared);
        Ok(())
    }

    fn advance(&mut self, step: Step) {
        self.step = step;
        self.since = Instant::now();
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        self.wake_stop.store(true, Ordering::Release);
    }
}

struct Monitor {
    prepared: mpsc::Receiver<std::result::Result<(), String>>,
    stop: Arc<AtomicBool>,
    reappeared: Arc<AtomicBool>,
    duplicate: Arc<AtomicBool>,
}

impl Monitor {
    fn new(
        probe: mpsc::Receiver<std::result::Result<(), String>>,
        hwnd: usize,
        ctx: egui::Context,
    ) -> Self {
        let (sender, prepared) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let reappeared = Arc::new(AtomicBool::new(false));
        let duplicate = Arc::new(AtomicBool::new(false));
        let (stopped, visible, extra) = (stop.clone(), reappeared.clone(), duplicate.clone());
        thread::spawn(move || {
            let result = probe
                .recv_timeout(Duration::from_secs(3))
                .map_err(|error| error.to_string())
                .and_then(|result| result)
                .and_then(|()| window_state(hwnd).map_err(|error| error.to_string()))
                .and_then(|(visible, _)| {
                    if visible {
                        Err("Native capture preparation left its HWND visible".into())
                    } else {
                        Ok(())
                    }
                });
            let success = result.is_ok();
            let _ = sender.send(result);
            super::super::wake_ui(&ctx, hwnd);
            let deadline = Instant::now() + Duration::from_secs(5);
            while success && !stopped.load(Ordering::Acquire) && Instant::now() < deadline {
                if !matches!(window_state(hwnd), Ok((false, _))) && !stopped.load(Ordering::Acquire)
                {
                    visible.store(true, Ordering::Release);
                }
                if probe.try_recv().is_ok() {
                    extra.store(true, Ordering::Release);
                }
                thread::sleep(Duration::from_millis(3));
            }
        });
        Self {
            prepared,
            stop,
            reappeared,
            duplicate,
        }
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn dispatch(app: &mut App, ctx: &egui::Context, event: Event) -> Result<()> {
    app.sender
        .send(event)
        .context("Capture handoff event channel closed")?;
    app.events(ctx);
    Ok(())
}

fn window_state(hwnd: usize) -> Result<(bool, bool)> {
    let hwnd = hwnd as HWND;
    let mut owner = 0;
    // SAFETY: Queries validate the opaque handle; only this smoke process's HWND is accepted.
    unsafe {
        ensure!(
            GetWindowThreadProcessId(hwnd, &mut owner) != 0 && owner == std::process::id(),
            "Capture handoff lost its owned HWND"
        );
        Ok((IsWindowVisible(hwnd) != 0, IsIconic(hwnd) != 0))
    }
}
