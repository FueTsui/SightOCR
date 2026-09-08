//! Debug-only, real Microsoft Pinyin input in an isolated production App.
//! Desktop key injection stops immediately if our window or editor loses focus.

use super::App;
use anyhow::{ensure, Context, Result};
use eframe::egui::{self, ViewportCommand};
use serde_json::{json, Value};
use std::{
    cell::RefCell,
    ffi::c_void,
    mem::{size_of, zeroed},
    path::PathBuf,
    ptr::null_mut,
    rc::Rc,
    time::{Duration, Instant},
};
use windows_sys::{
    core::GUID,
    Win32::{
        Foundation::HWND,
        System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER},
        UI::{
            Input::{
                Ime::{
                    ImmGetContext, ImmGetConversionStatus, ImmGetOpenStatus, ImmReleaseContext,
                    ImmSetOpenStatus, HIMC, IME_CMODE_NATIVE,
                },
                KeyboardAndMouse::{
                    ActivateKeyboardLayout, GetAsyncKeyState, GetFocus, GetKeyboardLayout,
                    GetKeyboardLayoutList, SendInput, HKL, INPUT, INPUT_0, INPUT_KEYBOARD,
                    KEYBDINPUT, KEYEVENTF_KEYUP,
                },
            },
            WindowsAndMessaging::{
                GetDlgItem, GetForegroundWindow, IsWindow, SendMessageW, WM_CLEAR,
            },
        },
    },
};

const PINYIN_CLSID: GUID = GUID::from_u128(0x81d4e9c9_1d3b_41bc_9e6c_4b40bf79e35e);
const PINYIN_PROFILE: GUID = GUID::from_u128(0xfa550b04_5ad7_411f_a5ac_ca038ec515d7);
const KEYBOARD_CATEGORY: GUID = GUID::from_u128(0x34745c63_b2f0_4784_8b67_5e12c8701a31);
const PROFILE_MANAGER_CLSID: GUID = GUID::from_u128(0x33c53a50_f456_4884_b049_85fd643ecfed);
const PROFILE_MANAGER_IID: GUID = GUID::from_u128(0x71c6e74c_0f28_11d8_a82a_00065b84435c);
const KEYS: [u16; 6] = [
    b'N' as u16,
    b'I' as u16,
    b'H' as u16,
    b'A' as u16,
    b'O' as u16,
    0x20,
];

pub(super) fn run(directory: PathBuf) -> Result<()> {
    use sightocr::config::Config;

    std::fs::create_dir_all(&directory)?;
    let scratch = tempfile::tempdir()?;
    let path = scratch.path().join("config.json");
    let report = Rc::new(RefCell::new(json!({
        "scope": "native-ime-live", "passed": false, "completed": false,
        "input": "real SendInput virtual keys: N I H A O SPACE, 350 ms apart",
        "ime": "installed Microsoft Pinyin 0804", "events": [], "editors": []
    })));
    let app_report = report.clone();
    sightocr::platform::set_dpi_awareness();
    let launched = eframe::run_native(
        "SightOCR IME Live Smoke",
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
            Ok(Box::new(LiveApp {
                run: None,
                app: App::new(cc, config, path)?,
                report: app_report,
                started: Instant::now(),
            }))
        }),
    );
    let mut report = report.borrow_mut();
    if let Err(error) = launched {
        report["error"] = json!(format!("Cannot run live IME smoke: {error}"));
    } else if report["completed"] != true {
        report["error"] = json!("IME live smoke closed before completion");
    }
    std::fs::write(
        directory.join("report.json"),
        serde_json::to_vec_pretty(&*report)?,
    )?;
    ensure!(report["passed"] == true, "{}", report["error"]);
    Ok(())
}

struct LiveApp {
    // Drop the input-method guard before App destroys its native child windows.
    run: Option<Run>,
    app: App,
    report: Rc<RefCell<Value>>,
    started: Instant,
}

impl eframe::App for LiveApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.app.update(ctx, frame);
        if self.report.borrow()["completed"] == true {
            return;
        }
        if self.run.is_none() {
            self.run = Some(Run::start(&mut self.app, ctx, self.report.clone()));
        }
        let result = if self.started.elapsed() > Duration::from_secs(35) {
            Some(Err(anyhow::anyhow!("IME live smoke timed out")))
        } else {
            self.run.as_mut().unwrap().tick(&mut self.app)
        };
        if let Some(mut result) = result {
            if let Some(guard) = self.run.as_mut().and_then(|run| run.method.as_mut()) {
                if let Err(error) = guard.restore() {
                    result = Err(error.context("Restoring smoke thread input method"));
                }
            }
            let mut report = self.report.borrow_mut();
            report["completed"] = json!(true);
            report["passed"] = json!(result.is_ok());
            if let Err(error) = result {
                report["error"] = json!(format!("{error:#}"));
            }
            self.app.exiting = true;
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
        ctx.request_repaint_after(Duration::from_millis(20));
    }
}

enum Stage {
    Configure(usize),
    Ready(usize),
    Key(usize, usize),
    Verify(usize),
}

struct Run {
    stage: Stage,
    since: Instant,
    method: Option<InputMethod>,
    baseline: (bool, u64, u64, u64),
    report: Rc<RefCell<Value>>,
}

impl Run {
    fn start(app: &mut App, ctx: &egui::Context, report: Rc<RefCell<Value>>) -> Self {
        ctx.memory_mut(|memory| memory.close_popup());
        app.native_editors.blur();
        app.busy = false;
        app.capturing = false;
        app.settings_open = false;
        app.editing_text = true;
        app.source.clear();
        app.translation.clear();
        app.focus_source = true;
        app.focus_translation = false;
        app.show(ctx);
        ctx.send_viewport_cmd(ViewportCommand::Focus);
        Self {
            stage: Stage::Configure(0),
            since: Instant::now(),
            method: None,
            baseline: (false, 0, 0, 0),
            report,
        }
    }

    fn tick(&mut self, app: &mut App) -> Option<Result<()>> {
        let delay = match self.stage {
            Stage::Key(_, 0) => 1000,
            Stage::Key(_, _) => 350,
            _ => 1000,
        };
        if self.since.elapsed() < Duration::from_millis(delay) {
            return None;
        }
        let result = self.step(app);
        self.since = Instant::now();
        match result {
            Ok(false) => None,
            Ok(true) => Some(Ok(())),
            Err(error) => Some(Err(error)),
        }
    }

    fn step(&mut self, app: &mut App) -> Result<bool> {
        match self.stage {
            Stage::Configure(index) => {
                let child = focused_child(app, index)?;
                if self.method.is_none() {
                    self.method = Some(InputMethod::activate(self.report.clone())?);
                }
                self.method.as_mut().unwrap().open(child)?;
                self.record(app, index, "configured");
                // Profile activation can begin an empty TSF composition. Cancel
                // it with a real key before measuring the new Pinyin sequence.
                send_key(app.hwnd as HWND, child, 0x1B)?;
                self.stage = Stage::Ready(index);
            }
            Stage::Ready(index) => {
                let child = focused_child(app, index)?;
                ensure!(
                    !app.native_editors.ime_state(index).0,
                    "Editor {index}: initial IME composition did not cancel"
                );
                // SAFETY: Clear only this isolated test control before its input
                // baseline. All Chinese text below still comes from real keys.
                unsafe {
                    SendMessageW(child, 0x00B1, 0, -1);
                    SendMessageW(child, WM_CLEAR, 0, 0);
                }
                self.stage = Stage::Key(index, 0);
            }
            Stage::Key(index, key) => {
                let child = focused_child(app, index)?;
                if key == 0 {
                    self.baseline = app.native_editors.ime_state(index);
                    ensure!(
                        !self.baseline.0,
                        "Editor {index}: composition already active before test input"
                    );
                }
                self.record(app, index, &format!("before_key_{key}"));
                if key > 0 {
                    let state = app.native_editors.ime_state(index);
                    ensure!(
                        state.0 && state.1 > self.baseline.1,
                        "Editor {index}: real Pinyin composition stopped before key {key}"
                    );
                    ensure!(
                        state.3 == self.baseline.3,
                        "Editor {index}: native font formatting ran during composition"
                    );
                }
                send_key(app.hwnd as HWND, child, KEYS[key])?;
                self.stage = if key + 1 < KEYS.len() {
                    Stage::Key(index, key + 1)
                } else {
                    Stage::Verify(index)
                };
            }
            Stage::Verify(index) => {
                focused_child(app, index)?;
                self.record(app, index, "after_commit");
                let state = app.native_editors.ime_state(index);
                let text = if index == 0 {
                    &app.source
                } else {
                    &app.translation
                };
                let evidence = json!({
                    "editor": if index == 0 {"source"} else {"translation"},
                    "text": text,
                    "codepoints": text.chars().map(|ch| format!("U+{:04X}", ch as u32)).collect::<Vec<_>>(),
                    "start_count": state.1, "end_count": state.2,
                    "composition_starts": state.1.saturating_sub(self.baseline.1),
                    "composition_ends": state.2.saturating_sub(self.baseline.2),
                    "composing_after_commit": state.0, "format_passes": state.3,
                });
                self.report.borrow_mut()["editors"]
                    .as_array_mut()
                    .unwrap()
                    .push(evidence);
                ensure!(
                    !state.0 && state.1 > self.baseline.1 && state.2 > self.baseline.2,
                    "Editor {index}: no completed native IME composition was observed"
                );
                ensure!(
                    text.chars()
                        .any(|ch| ('\u{3400}'..='\u{9fff}').contains(&ch))
                        && !text.chars().any(|ch| ch.is_ascii_alphabetic()),
                    "Editor {index}: Pinyin did not commit Chinese text: {text:?}"
                );
                if index == 1 {
                    return Ok(true);
                }
                app.native_editors.blur();
                app.focus_translation = true;
                self.stage = Stage::Configure(1);
            }
        }
        Ok(false)
    }

    fn record(&self, app: &App, index: usize, stage: &str) {
        let state = app.native_editors.ime_state(index);
        // SAFETY: Read only this isolated App's fixed native child.
        let child = unsafe { GetDlgItem(app.hwnd as HWND, 0x5340 + index as i32) };
        let ime = native_ime_diagnostic(child);
        let profile = self
            .method
            .as_ref()
            .map(InputMethod::active_profile_diagnostic);
        let text = if index == 0 {
            &app.source
        } else {
            &app.translation
        };
        self.report.borrow_mut()["events"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "editor": index, "stage": stage, "text": text, "composing": state.0,
                "start_count": state.1, "end_count": state.2, "format_passes": state.3,
                "ime": ime, "active_profile": profile,
            }));
    }
}

fn focused_child(app: &App, index: usize) -> Result<HWND> {
    // SAFETY: Query only the fixed control ID belonging to this isolated App.
    let child = unsafe { GetDlgItem(app.hwnd as HWND, 0x5340 + index as i32) };
    ensure!(!child.is_null(), "Missing native editor {index}");
    require_input_target(app.hwnd as HWND, child)?;
    Ok(child)
}

fn require_input_target(parent: HWND, child: HWND) -> Result<()> {
    // SAFETY: Read focus and foreground handles without inspecting other apps.
    unsafe {
        ensure!(
            GetForegroundWindow() == parent,
            "Smoke App lost foreground; stopped without injecting another key"
        );
        ensure!(
            GetFocus() == child && IsWindow(child) != 0,
            "Smoke native editor lost focus; stopped without injecting another key"
        );
        for key in [0x10, 0x11, 0x12, 0x5B, 0x5C] {
            ensure!(
                GetAsyncKeyState(key) >= 0,
                "A desktop modifier is held; stopped without injecting another key"
            );
        }
    }
    Ok(())
}

fn send_key(parent: HWND, child: HWND, key: u16) -> Result<()> {
    let input = [0, KEYEVENTF_KEYUP].map(|flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    });
    require_input_target(parent, child)?;
    // SAFETY: Immediately preceding checks establish our App and native child
    // as the input target. One atomic batch contains a key down/up pair only.
    let sent = unsafe {
        SendInput(
            input.len() as u32,
            input.as_ptr(),
            size_of::<INPUT>() as i32,
        )
    };
    ensure!(
        sent == input.len() as u32,
        "SendInput inserted only {sent}/2 keyboard events"
    );
    Ok(())
}

// ABI copied from the installed Windows SDK's msctf.h. Only QueryInterface,
// Release, ActivateProfile, and GetActiveProfile signatures are used here.
#[repr(C)]
struct ProfileManager {
    vtable: *const ProfileManagerVtable,
}
#[repr(C)]
struct ProfileManagerVtable {
    query_interface:
        unsafe extern "system" fn(*mut ProfileManager, *const GUID, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut ProfileManager) -> u32,
    release: unsafe extern "system" fn(*mut ProfileManager) -> u32,
    activate: unsafe extern "system" fn(
        *mut ProfileManager,
        u32,
        u16,
        *const GUID,
        *const GUID,
        HKL,
        u32,
    ) -> i32,
    unused: [usize; 6],
    get_active: unsafe extern "system" fn(*mut ProfileManager, *const GUID, *mut Profile) -> i32,
}
#[derive(Clone, Copy)]
#[repr(C)]
struct Profile {
    kind: u32,
    language: u16,
    clsid: GUID,
    profile: GUID,
    category: GUID,
    substitute: HKL,
    capabilities: u32,
    layout: HKL,
    flags: u32,
}

struct InputMethod {
    manager: *mut ProfileManager,
    previous_profile: Option<Profile>,
    previous_layout: HKL,
    opened: Vec<(HWND, HIMC, bool)>,
    restored: bool,
    report: Rc<RefCell<Value>>,
}

impl InputMethod {
    fn activate(report: Rc<RefCell<Value>>) -> Result<Self> {
        let mut manager = null_mut();
        // SAFETY: App's native editors already initialized COM on this thread.
        // IID and vtable match the SDK, and the output receives an owned ref.
        let hr = unsafe {
            CoCreateInstance(
                &PROFILE_MANAGER_CLSID,
                null_mut(),
                CLSCTX_INPROC_SERVER,
                &PROFILE_MANAGER_IID,
                &mut manager,
            )
        };
        ensure!(
            hr >= 0 && !manager.is_null(),
            "Cannot create TSF profile manager: 0x{hr:08X}"
        );
        let mut guard = Self {
            manager: manager.cast(),
            previous_profile: None,
            // SAFETY: Zero queries only this UI thread's active keyboard layout.
            previous_layout: unsafe { GetKeyboardLayout(0) },
            opened: Vec::new(),
            restored: false,
            report,
        };
        // SAFETY: Profile is an SDK POD structure; TSF fills its entire value.
        let mut previous: Profile = unsafe { zeroed() };
        // SAFETY: The live COM pointer owns this exact profile-manager vtable.
        let active_hr = unsafe {
            ((*(*guard.manager).vtable).get_active)(
                guard.manager,
                &KEYBOARD_CATEGORY,
                &mut previous,
            )
        };
        ensure!(
            active_hr == 0,
            "Cannot preserve the thread's current TSF profile: 0x{active_hr:08X}"
        );
        guard.previous_profile = Some(previous);
        // SAFETY: Query installed layouts without loading or configuring any.
        let count = unsafe { GetKeyboardLayoutList(0, null_mut()) };
        ensure!(count > 0, "No installed keyboard layouts");
        let mut layouts = vec![null_mut(); count as usize];
        // SAFETY: The vector contains count writable layout handles.
        let found = unsafe { GetKeyboardLayoutList(count, layouts.as_mut_ptr()) };
        layouts.truncate(found.max(0) as usize);
        let chinese = layouts
            .into_iter()
            .find(|layout| (*layout as usize & 0xffff) == 0x0804)
            .context("The installed Chinese 0804 keyboard layout is unavailable")?;
        // SAFETY: Flags zero selects this thread only; no layout is installed
        // and no process/session/registry flags are supplied.
        let previous = unsafe { ActivateKeyboardLayout(chinese, 0) };
        ensure!(
            !previous.is_null(),
            "Cannot activate Chinese layout in the smoke UI thread"
        );
        // SAFETY: Activate only the supplied, already installed Pinyin profile.
        // Flags zero never enable profiles or modify user registry settings.
        let hr = unsafe {
            ((*(*guard.manager).vtable).activate)(
                guard.manager,
                1,
                0x0804,
                &PINYIN_CLSID,
                &PINYIN_PROFILE,
                null_mut(),
                0,
            )
        };
        ensure!(
            hr == 0,
            "Cannot activate installed Microsoft Pinyin: 0x{hr:08X}"
        );
        guard.report.borrow_mut()["thread_profile_activated"] = json!(true);
        Ok(guard)
    }

    fn open(&mut self, child: HWND) -> Result<()> {
        let before = native_ime_diagnostic(child);
        // SAFETY: This is the verified focused child owned by the smoke App.
        let context = unsafe { ImmGetContext(child) };
        ensure!(
            !context.is_null(),
            "Focused native editor has no IME context"
        );
        // SAFETY: Read and open this owned context, then release the acquired
        // reference. Preserve its previous status once even if children share it.
        let opened = unsafe {
            if !self.opened.iter().any(|entry| entry.1 == context) {
                self.opened
                    .push((child, context, ImmGetOpenStatus(context) != 0));
            }
            let opened = ImmSetOpenStatus(context, 1) != 0;
            ImmReleaseContext(child, context);
            opened
        };
        let after = native_ime_diagnostic(child);
        let mut report = self.report.borrow_mut();
        if report.get("ime_configuration").is_none() {
            report["ime_configuration"] = json!([]);
        }
        report["ime_configuration"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "before": before, "after": after, "open_request_succeeded": opened,
            }));
        ensure!(opened, "Cannot open the native editor's Pinyin IME context");
        Ok(())
    }

    fn active_profile_diagnostic(&self) -> Value {
        // SAFETY: This SDK POD is an output buffer for our thread's manager.
        let mut profile: Profile = unsafe { zeroed() };
        // SAFETY: The COM instance and output buffer remain live for the call.
        let hr = unsafe {
            ((*(*self.manager).vtable).get_active)(self.manager, &KEYBOARD_CATEGORY, &mut profile)
        };
        if hr != 0 {
            return json!({"hresult": format!("0x{hr:08X}")});
        }
        json!({
            "kind": profile.kind, "language": format!("{:04X}", profile.language),
            "clsid": guid_string(profile.clsid), "profile": guid_string(profile.profile),
            "layout": format!("0x{:X}", profile.layout as usize), "flags": profile.flags,
        })
    }

    fn restore(&mut self) -> Result<()> {
        if self.restored {
            return Ok(());
        }
        self.restored = true;
        let mut failures = Vec::new();
        for (child, saved_context, status) in self.opened.iter().rev() {
            // SAFETY: Restoration occurs before App drops the owned children.
            // Only restore the same still-associated context that we changed.
            unsafe {
                let context = ImmGetContext(*child);
                if context == *saved_context
                    && !context.is_null()
                    && ImmSetOpenStatus(context, i32::from(*status)) == 0
                {
                    failures.push("owned IME open status");
                }
                if !context.is_null() {
                    ImmReleaseContext(*child, context);
                }
            }
        }
        // SAFETY: Restore only this thread's pre-test layout and active profile.
        unsafe {
            if !self.previous_layout.is_null()
                && ActivateKeyboardLayout(self.previous_layout, 0).is_null()
            {
                failures.push("thread keyboard layout");
            }
            if let Some(profile) = self.previous_profile {
                let hr = ((*(*self.manager).vtable).activate)(
                    self.manager,
                    profile.kind,
                    profile.language,
                    &profile.clsid,
                    &profile.profile,
                    if profile.kind == 1 {
                        null_mut()
                    } else {
                        profile.layout
                    },
                    0,
                );
                if hr != 0 {
                    failures.push("thread TSF profile");
                }
            }
        }
        self.report.borrow_mut()["input_method_restored"] = json!(failures.is_empty());
        ensure!(
            failures.is_empty(),
            "Could not restore: {}",
            failures.join(", ")
        );
        Ok(())
    }
}

impl Drop for InputMethod {
    fn drop(&mut self) {
        let _ = self.restore();
        // SAFETY: Release the single owned CoCreateInstance reference once.
        unsafe {
            ((*(*self.manager).vtable).release)(self.manager);
        }
    }
}

fn guid_string(guid: GUID) -> String {
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        guid.data1,
        guid.data2,
        guid.data3,
        guid.data4[0],
        guid.data4[1],
        guid.data4[2],
        guid.data4[3],
        guid.data4[4],
        guid.data4[5],
        guid.data4[6],
        guid.data4[7]
    )
}

fn native_ime_diagnostic(child: HWND) -> Value {
    // SAFETY: Query only the owned child and calling test thread. Release each
    // acquired context immediately; no user document or other window is read.
    unsafe {
        let context = ImmGetContext(child);
        let mut conversion = 0;
        let mut sentence = 0;
        let has_conversion = !context.is_null()
            && ImmGetConversionStatus(context, &mut conversion, &mut sentence) != 0;
        let open = !context.is_null() && ImmGetOpenStatus(context) != 0;
        let diagnostic = json!({
            "context_present": !context.is_null(), "context": format!("0x{:X}", context as usize),
            "open": open, "conversion_available": has_conversion,
            "conversion": conversion, "sentence": sentence,
            "native_mode": has_conversion && conversion & IME_CMODE_NATIVE != 0,
            "thread_layout": format!("0x{:X}", GetKeyboardLayout(0) as usize),
        });
        if !context.is_null() {
            ImmReleaseContext(child, context);
        }
        diagnostic
    }
}
