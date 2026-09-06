//! Foreground-only shortcut recording. No keyboard hook or background key polling.

use super::theme::Palette;
use eframe::egui::{self, Context, Event, FontId, Id, Key, Modifiers, Rect, Sense, Stroke};
use sightocr::config::Config;
use std::time::{Duration, Instant};

const RELEASE_DELAY: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Target {
    Capture,
    Translate,
    SilentCapture,
}

impl Target {
    fn id(self) -> Id {
        Id::new(match self {
            Self::Capture => "capture_hotkey_recorder",
            Self::Translate => "translate_hotkey_recorder",
            Self::SilentCapture => "silent_capture_hotkey_recorder",
        })
    }

    fn set(self, config: &mut Config, value: String) {
        match self {
            Self::Capture => config.hotkey = value,
            Self::Translate => config.translate_hotkey = value,
            Self::SilentCapture => config.silent_hotkey = value,
        }
    }
}

#[derive(Clone)]
struct Recording {
    target: Target,
    rect: Rect,
}

#[derive(Clone)]
struct Suppression {
    until: Instant,
    key: Option<Key>,
    virtual_key: Option<i32>,
}

#[derive(Clone, Default)]
struct State {
    recording: Option<Recording>,
    suppression: Option<Suppression>,
}

fn state_id() -> Id {
    Id::new("shortcut_recorder_state")
}

fn load(ctx: &Context) -> State {
    ctx.data_mut(|data| data.get_temp::<State>(state_id()).unwrap_or_default())
}

fn save(ctx: &Context, state: State) {
    ctx.data_mut(|data| data.insert_temp(state_id(), state));
}

/// Begin recording into one draft field; also used by isolated UI smoke tests.
pub(super) fn begin(ctx: &Context, target: Target) {
    let rect = ctx.data_mut(|data| {
        data.get_temp::<Rect>(target.id().with("rect"))
            .unwrap_or(Rect::NOTHING)
    });
    let mut state = load(ctx);
    state.recording = Some(Recording { target, rect });
    save(ctx, state);
    ctx.memory_mut(|memory| memory.request_focus(target.id()));
    ctx.request_repaint();
}

pub(super) fn is_recording(ctx: &Context) -> bool {
    load(ctx).recording.is_some()
}

/// Leaving the page cancels recording, but must not remove a just-committed key guard.
/// Calling this on every ordinary app frame never extends an existing deadline.
pub(super) fn clear(ctx: &Context) {
    let mut state = load(ctx);
    if let Some(recording) = state.recording.take() {
        ctx.memory_mut(|memory| memory.surrender_focus(recording.target.id()));
        if state.suppression.is_none() {
            state.suppression = Some(Suppression {
                until: Instant::now() + RELEASE_DELAY,
                key: None,
                virtual_key: None,
            });
        }
        save(ctx, state);
        ctx.request_repaint_after(RELEASE_DELAY);
    }
}

pub(super) fn suppress_shortcuts(ctx: &Context) -> bool {
    let mut state = load(ctx);
    refresh_suppression(ctx, &mut state);
    let suppressed = state.recording.is_some() || state.suppression.is_some();
    save(ctx, state);
    suppressed
}

/// Called before application shortcuts and platform messages are dispatched.
pub(super) fn handle_input(ctx: &Context, config: &mut Config) {
    let mut state = load(ctx);
    refresh_suppression(ctx, &mut state);
    if state.suppression.is_some() {
        consume_keyboard(ctx);
        save(ctx, state);
        return;
    }
    let Some(recording) = state.recording.clone() else {
        save(ctx, state);
        return;
    };
    let events = ctx.input(|input| input.events.clone());
    let outside_click = events.iter().any(|event| {
        matches!(event, Event::PointerButton { pos, pressed: true, .. } if !recording.rect.contains(*pos))
    });
    // eframe can retain an obsolete `focused = false` after tray hide/restore.
    // The current native foreground owner is authoritative, including when a
    // delayed WindowFocused(false) event arrives after our window regained focus.
    let lost_window_focus = !window_focused(ctx);
    let widget_focused = ctx.memory(|memory| {
        memory.has_focus(recording.target.id())
            || memory.had_focus_last_frame(recording.target.id())
    });
    if outside_click || lost_window_focus || !widget_focused {
        save(ctx, state);
        clear(ctx);
        return;
    }
    consume_keyboard(ctx);
    for event in events {
        let Event::Key {
            key,
            pressed: true,
            repeat: false,
            modifiers,
            ..
        } = event
        else {
            continue;
        };
        if key == Key::Escape {
            finish(ctx, &mut state, Some(key), Some(0x1B));
            break;
        }
        // egui's Windows `command` modifier means Ctrl, not the Windows logo key.
        // Only query the two logo keys while processing an actual foreground key event.
        let windows = native_key_down(0x5B) || native_key_down(0x5C);
        if let Some((chord, virtual_key)) = chord_for_key(key, modifiers, windows) {
            recording.target.set(config, chord);
            finish(ctx, &mut state, Some(key), Some(virtual_key));
            break;
        }
    }
    save(ctx, state);
}

/// Windows may deliver a registered shortcut without an egui Key event. Consume it
/// while recording, or while waiting for the same physical press to finish.
pub(super) fn record_registered(ctx: &Context, config: &mut Config, hotkey: &str) -> bool {
    if !suppress_shortcuts(ctx) {
        return false;
    }
    let mut state = load(ctx);
    if state.suppression.is_some() {
        return true;
    }
    let Some(recording) = state.recording.clone() else {
        return false;
    };
    if !window_focused(ctx) {
        clear(ctx);
        return true;
    }
    if let Some((chord, virtual_key)) = registered_chord(hotkey) {
        if virtual_key != 0x1B {
            recording.target.set(config, chord);
        }
        finish(ctx, &mut state, None, Some(virtual_key));
        save(ctx, state);
    }
    true
}

fn finish(ctx: &Context, state: &mut State, key: Option<Key>, virtual_key: Option<i32>) {
    if let Some(recording) = state.recording.take() {
        ctx.memory_mut(|memory| memory.surrender_focus(recording.target.id()));
    }
    state.suppression = Some(Suppression {
        until: Instant::now() + RELEASE_DELAY,
        key,
        virtual_key,
    });
    ctx.request_repaint_after(Duration::from_millis(40));
}

fn refresh_suppression(ctx: &Context, state: &mut State) {
    let Some(guard) = &state.suppression else {
        return;
    };
    let key_down = window_focused(ctx)
        && (ctx.input(|input| guard.key.is_some_and(|key| input.keys_down.contains(&key)))
            || guard.virtual_key.is_some_and(native_key_down));
    if Instant::now() >= guard.until && !key_down {
        state.suppression = None;
    } else {
        ctx.request_repaint_after(Duration::from_millis(40));
    }
}

fn consume_keyboard(ctx: &Context) {
    ctx.input_mut(|input| {
        input.events.retain(|event| {
            !matches!(
                event,
                Event::Key { .. }
                    | Event::Text(_)
                    | Event::Copy
                    | Event::Cut
                    | Event::Paste(_)
                    | Event::Ime(_)
            )
        });
    });
}

#[cfg(not(test))]
fn window_focused(_: &Context) -> bool {
    native_foreground()
}

#[cfg(test)]
fn window_focused(ctx: &Context) -> bool {
    ctx.input(|input| input.focused)
}

#[cfg(not(test))]
fn native_foreground() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId,
    };
    let mut process = 0;
    // SAFETY: These queries read only the foreground HWND and write to a local process ID.
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), &mut process) };
    process == std::process::id()
}

#[cfg(not(test))]
fn native_key_down(virtual_key: i32) -> bool {
    // SAFETY: Callers gate this scalar key-state query to recording/suppression in our
    // foreground window. It installs no hook and never inspects other applications' input.
    unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyState(virtual_key) < 0 }
}

#[cfg(test)]
fn native_key_down(_: i32) -> bool {
    false
}

fn chord_for_key(key: Key, modifiers: Modifiers, windows: bool) -> Option<(String, i32)> {
    let name = key.name();
    let virtual_key = key_code(name)?;
    Some((
        format_chord(
            name,
            modifiers.ctrl,
            modifiers.alt,
            modifiers.shift,
            windows,
        ),
        virtual_key,
    ))
}

fn format_chord(name: &str, ctrl: bool, alt: bool, shift: bool, windows: bool) -> String {
    let mut components = Vec::with_capacity(5);
    for (enabled, name) in [
        (ctrl, "Ctrl"),
        (alt, "Alt"),
        (shift, "Shift"),
        (windows, "Win"),
    ] {
        if enabled {
            components.push(name);
        }
    }
    components.push(name);
    components.join("+")
}

fn registered_chord(text: &str) -> Option<(String, i32)> {
    let (mut ctrl, mut alt, mut shift, mut windows) = (false, false, false, false);
    let mut key = None;
    for component in text.split('+') {
        let component = component.trim().to_ascii_uppercase();
        match component.as_str() {
            "CTRL" | "CONTROL" => ctrl = true,
            "ALT" => alt = true,
            "SHIFT" => shift = true,
            "WIN" | "WINDOWS" | "SUPER" | "META" => windows = true,
            _ => {
                if key.is_some() {
                    return None;
                }
                let code = key_code(&component)?;
                key = Some((component, code));
            }
        }
    }
    let (name, code) = key?;
    Some((format_chord(&name, ctrl, alt, shift, windows), code))
}

/// Names shared with the Win32 shortcut parser. Modifiers alone are deliberately absent.
fn key_code(name: &str) -> Option<i32> {
    let upper = name.to_ascii_uppercase();
    Some(match upper.as_str() {
        "SPACE" | "SPACEBAR" => 0x20,
        "ENTER" | "RETURN" => 0x0D,
        "TAB" => 0x09,
        "CLEAR" => 0x0C,
        "PAUSE" => 0x13,
        "CAPSLOCK" => 0x14,
        "ESC" | "ESCAPE" => 0x1B,
        "BACKSPACE" => 0x08,
        "DELETE" | "DEL" => 0x2E,
        "INSERT" | "INS" => 0x2D,
        "HOME" => 0x24,
        "END" => 0x23,
        "PAGEUP" | "PGUP" => 0x21,
        "PAGEDOWN" | "PGDN" => 0x22,
        "LEFT" => 0x25,
        "RIGHT" => 0x27,
        "UP" => 0x26,
        "DOWN" => 0x28,
        "SELECT" => 0x29,
        "PRINT" => 0x2A,
        "EXECUTE" => 0x2B,
        "PRINTSCREEN" | "PRTSC" => 0x2C,
        "HELP" => 0x2F,
        "MULTIPLY" => 0x6A,
        "ADD" => 0x6B,
        "SEPARATOR" => 0x6C,
        "SUBTRACT" => 0x6D,
        "DECIMAL" => 0x6E,
        "DIVIDE" => 0x6F,
        "NUMLOCK" => 0x90,
        "SCROLL" | "SCROLLLOCK" => 0x91,
        value
            if value.len() == 7
                && value.starts_with("NUMPAD")
                && value.as_bytes()[6].is_ascii_digit() =>
        {
            0x60 + i32::from(value.as_bytes()[6] - b'0')
        }
        value if value.len() == 1 && value.as_bytes()[0].is_ascii_alphanumeric() => {
            i32::from(value.as_bytes()[0])
        }
        value if value.starts_with('F') => {
            let number = value[1..].parse::<i32>().ok()?;
            if !(1..=24).contains(&number) {
                return None;
            }
            0x70 + number - 1
        }
        _ => return None,
    })
}

/// A focusable recording button, never an editable text field.
pub(super) fn field(ui: &mut egui::Ui, target: Target, value: &str) {
    let p = Palette::get(ui.ctx());
    let active = is_recording(ui.ctx())
        && load(ui.ctx())
            .recording
            .is_some_and(|recording| recording.target == target);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 36.0), Sense::hover());
    let response = ui.interact(rect, target.id(), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            ui.is_enabled(),
            if active {
                "正在录入快捷键，按 Esc 取消"
            } else {
                value
            },
        )
    });
    ui.ctx()
        .data_mut(|data| data.insert_temp(target.id().with("rect"), rect));
    if active {
        let mut state = load(ui.ctx());
        if let Some(recording) = &mut state.recording {
            recording.rect = rect;
        }
        save(ui.ctx(), state);
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                target.id(),
                egui::EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            )
        });
    }
    let hovered = response.hovered() || response.has_focus();
    ui.painter().rect(
        rect,
        4,
        if active { p.accent_soft } else { p.panel },
        Stroke::new(
            if active { 1.5_f32 } else { 1.0_f32 },
            if active || hovered {
                p.accent
            } else {
                p.border
            },
        ),
        egui::StrokeKind::Inside,
    );
    let icon_rect = Rect::from_center_size(
        egui::pos2(rect.left() + 19.0, rect.center().y),
        egui::vec2(17.0, 17.0),
    );
    let icon_color = if active { p.accent } else { p.muted };
    ui.painter().rect_stroke(
        Rect::from_center_size(icon_rect.center(), egui::vec2(17.0, 12.0)),
        2,
        Stroke::new(1.0_f32, icon_color),
        egui::StrokeKind::Inside,
    );
    for y in [-2.0_f32, 1.0_f32] {
        for x in [-5.0_f32, -1.5_f32, 2.0_f32, 5.5_f32] {
            ui.painter()
                .circle_filled(icon_rect.center() + egui::vec2(x, y), 0.7, icon_color);
        }
    }
    ui.painter().text(
        egui::pos2(rect.left() + 35.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        if active {
            "请按快捷键 · Esc 取消"
        } else {
            value
        },
        FontId::proportional(13.0),
        if active { p.accent } else { p.text },
    );
    if response.clicked() {
        if active {
            clear(ui.ctx());
        } else {
            begin(ui.ctx(), target);
        }
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(key: Key, modifiers: Modifiers, repeat: bool) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat,
            modifiers,
        }
    }

    fn frame(
        ctx: &Context,
        config: &mut Config,
        events: Vec<Event>,
        modifiers: Modifiers,
        focused: bool,
    ) {
        let input = egui::RawInput {
            events,
            modifiers,
            focused,
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(600.0, 200.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            handle_input(ctx, config);
            egui::CentralPanel::default().show(ctx, |ui| {
                field(ui, Target::Capture, &config.hotkey);
                field(ui, Target::Translate, &config.translate_hotkey);
                field(ui, Target::SilentCapture, &config.silent_hotkey);
            });
        });
    }

    #[test]
    fn captures_combo_into_draft_and_suppresses_duplicate_registered_event() {
        let ctx = Context::default();
        let mut config = Config::default();
        frame(&ctx, &mut config, vec![], Modifiers::NONE, true);
        begin(&ctx, Target::Capture);
        let modifiers = Modifiers {
            ctrl: true,
            shift: true,
            ..Modifiers::NONE
        };
        frame(
            &ctx,
            &mut config,
            vec![press(Key::A, modifiers, false), Event::Text("A".into())],
            modifiers,
            true,
        );
        assert_eq!(config.hotkey, "Ctrl+Shift+A");
        assert_eq!(config.translate_hotkey, "F2");
        assert_eq!(config.silent_hotkey, "F4");
        assert!(!is_recording(&ctx));
        assert!(record_registered(&ctx, &mut config, "F2"));
        assert_eq!(config.hotkey, "Ctrl+Shift+A");
    }

    #[test]
    fn registered_key_records_once_and_restores_normal_shortcuts_after_release() {
        let ctx = Context::default();
        let mut config = Config::default();
        frame(&ctx, &mut config, vec![], Modifiers::NONE, true);
        begin(&ctx, Target::Translate);
        assert!(record_registered(&ctx, &mut config, "Ctrl+Alt+Shift+F23"));
        assert_eq!(config.translate_hotkey, "Ctrl+Alt+Shift+F23");
        assert_eq!(config.hotkey, "F5");
        assert_eq!(config.silent_hotkey, "F4");
        assert!(record_registered(&ctx, &mut config, "F2"));
        assert_eq!(config.translate_hotkey, "Ctrl+Alt+Shift+F23");
        let mut state = load(&ctx);
        state.suppression.as_mut().unwrap().until = Instant::now() - Duration::from_millis(1);
        save(&ctx, state);
        assert!(!record_registered(&ctx, &mut config, "F4"));
    }

    #[test]
    fn silent_recording_only_changes_the_silent_target_for_keyboard_and_registered_events() {
        for registered in [false, true] {
            let ctx = Context::default();
            let mut config = Config {
                silent_hotkey: "F9".into(),
                ..Config::default()
            };
            frame(&ctx, &mut config, vec![], Modifiers::NONE, true);
            begin(&ctx, Target::SilentCapture);
            let expected = if registered {
                // F4 is the registered silent default, independent of Capture's F5.
                assert!(record_registered(&ctx, &mut config, "F4"));
                "F4"
            } else {
                frame(
                    &ctx,
                    &mut config,
                    vec![press(Key::F10, Modifiers::NONE, false)],
                    Modifiers::NONE,
                    true,
                );
                "F10"
            };
            assert_eq!(config.silent_hotkey, expected);
            assert_eq!(config.hotkey, "F5");
            assert_eq!(config.translate_hotkey, "F2");
            assert!(!is_recording(&ctx));
            assert!(record_registered(&ctx, &mut config, "F2"));
            assert_eq!(config.silent_hotkey, expected);
            assert_eq!(config.translate_hotkey, "F2");
        }
    }

    #[test]
    fn modifiers_repeats_and_unsupported_keys_do_not_commit() {
        let ctx = Context::default();
        let mut config = Config::default();
        // egui recomputes `repeat` from its held-key set. Hold A before recording
        // instead of fabricating a repeat flag on the first physical key-down.
        frame(
            &ctx,
            &mut config,
            vec![press(Key::A, Modifiers::NONE, false)],
            Modifiers::NONE,
            true,
        );
        begin(&ctx, Target::Capture);
        frame(&ctx, &mut config, vec![], Modifiers::CTRL, true);
        assert!(is_recording(&ctx));
        assert_eq!(config.hotkey, "F5");
        frame(
            &ctx,
            &mut config,
            vec![
                press(Key::A, Modifiers::CTRL, true),
                press(Key::F25, Modifiers::NONE, false),
            ],
            Modifiers::NONE,
            true,
        );
        assert!(is_recording(&ctx));
        assert_eq!(config.hotkey, "F5");
        assert!(registered_chord("Ctrl+Alt+Shift+Win").is_none());
    }

    #[test]
    fn escape_cancels_and_clear_never_extends_the_guard() {
        let ctx = Context::default();
        let mut config = Config::default();
        frame(&ctx, &mut config, vec![], Modifiers::NONE, true);
        begin(&ctx, Target::Capture);
        frame(
            &ctx,
            &mut config,
            vec![press(Key::Escape, Modifiers::NONE, false)],
            Modifiers::NONE,
            true,
        );
        assert_eq!(config.hotkey, "F5");
        assert!(!is_recording(&ctx));
        let deadline = load(&ctx).suppression.unwrap().until;
        for _ in 0..5 {
            clear(&ctx);
        }
        assert_eq!(load(&ctx).suppression.unwrap().until, deadline);

        let registered_ctx = Context::default();
        frame(&registered_ctx, &mut config, vec![], Modifiers::NONE, true);
        begin(&registered_ctx, Target::Capture);
        assert!(record_registered(&registered_ctx, &mut config, "Esc"));
        assert_eq!(config.hotkey, "F5");
        assert!(!is_recording(&registered_ctx));
    }

    #[test]
    fn focus_loss_or_outside_click_cancels_without_changing_the_value() {
        for lose_focus in [false, true] {
            let ctx = Context::default();
            let mut config = Config::default();
            frame(&ctx, &mut config, vec![], Modifiers::NONE, true);
            begin(&ctx, Target::Capture);
            let events = if lose_focus {
                vec![Event::WindowFocused(false)]
            } else {
                vec![Event::PointerButton {
                    pos: egui::pos2(500.0, 190.0),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                }]
            };
            frame(&ctx, &mut config, events, Modifiers::NONE, !lose_focus);
            assert!(!is_recording(&ctx));
            assert_eq!(config.hotkey, "F5");
        }
    }

    #[test]
    fn held_key_keeps_guard_after_delay_until_key_up() {
        let ctx = Context::default();
        let mut config = Config::default();
        frame(&ctx, &mut config, vec![], Modifiers::NONE, true);
        begin(&ctx, Target::Capture);
        frame(
            &ctx,
            &mut config,
            vec![press(Key::F8, Modifiers::NONE, false)],
            Modifiers::NONE,
            true,
        );
        let mut state = load(&ctx);
        state.suppression.as_mut().unwrap().until = Instant::now() - Duration::from_millis(1);
        save(&ctx, state);
        assert!(suppress_shortcuts(&ctx));
        frame(
            &ctx,
            &mut config,
            vec![Event::Key {
                key: Key::F8,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
            Modifiers::NONE,
            true,
        );
        assert!(!suppress_shortcuts(&ctx));
    }

    #[test]
    fn mappings_follow_windows_virtual_keys_and_never_treat_command_as_win() {
        for (key, expected) in [
            (Key::F1, "F1"),
            (Key::F24, "F24"),
            (Key::Num8, "8"),
            (Key::Z, "Z"),
            (Key::PageUp, "PageUp"),
            (Key::ArrowLeft, "Left"),
            (Key::Insert, "Insert"),
            (Key::Delete, "Delete"),
            (Key::Tab, "Tab"),
        ] {
            assert_eq!(
                chord_for_key(key, Modifiers::NONE, false).unwrap().0,
                expected
            );
        }
        assert!(chord_for_key(Key::F25, Modifiers::NONE, false).is_none());
        assert!(chord_for_key(Key::Comma, Modifiers::NONE, false).is_none());
        assert_eq!(
            chord_for_key(
                Key::A,
                Modifiers {
                    ctrl: true,
                    command: true,
                    ..Modifiers::NONE
                },
                false
            )
            .unwrap()
            .0,
            "Ctrl+A"
        );
        assert_eq!(
            chord_for_key(
                Key::A,
                Modifiers {
                    alt: true,
                    shift: true,
                    ..Modifiers::NONE
                },
                true
            )
            .unwrap()
            .0,
            "Alt+Shift+Win+A"
        );
    }
}
