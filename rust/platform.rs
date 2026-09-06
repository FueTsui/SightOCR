//! Windows desktop integration. All HWNDs and GDI objects stay on their owning thread.
use anyhow::{anyhow, bail, Context, Result};
use image::RgbaImage;
use std::{
    ffi::c_void,
    mem::{size_of, zeroed},
    os::windows::ffi::OsStrExt,
    ptr::{null, null_mut},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::{self, JoinHandle},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{LibraryLoader::GetModuleHandleW, Registry::*, Threading::CreateMutexW},
    UI::{HiDpi::*, Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
};

mod capture_render;
mod native_cursor;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn win_error(operation: &str) -> anyhow::Error {
    anyhow!("{operation}: {}", std::io::Error::last_os_error())
}

/// Call before constructing the GUI. Capture also sets a thread-local DPI context.
pub fn set_dpi_awareness() {
    // SAFETY: These APIs accept predefined awareness constants and no borrowed pointers.
    unsafe {
        if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) == 0 {
            SetProcessDPIAware();
        }
    }
}

struct ThreadDpi(DPI_AWARENESS_CONTEXT);
impl ThreadDpi {
    fn physical_pixels() -> Self {
        // SAFETY: The constant is a valid DPI context; the previous context is retained for restoration.
        Self(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })
    }
}
impl Drop for ThreadDpi {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: This is the context returned on this same thread when the guard was created.
            unsafe { SetThreadDpiAwarenessContext(self.0) };
        }
    }
}

struct RegistryKey(HKEY);
impl Drop for RegistryKey {
    fn drop(&mut self) {
        // SAFETY: This wrapper owns a successfully opened key, closed exactly once.
        unsafe { RegCloseKey(self.0) };
    }
}

/// Exact registry value before a settings transaction, including the legacy executable path.
/// Dropping this snapshot does not modify the registry; rollback is always explicit.
pub struct AutostartSnapshot {
    value: Option<(u32, Vec<u8>)>,
}

const AUTOSTART_VALUE_LIMIT: usize = 64 * 1024;

pub fn snapshot_autostart() -> Result<AutostartSnapshot> {
    let path = wide(r"Software\Microsoft\Windows\CurrentVersion\Run");
    let mut raw = null_mut();
    // SAFETY: The key path is null-terminated; raw is writable storage for the opened key.
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            path.as_ptr(),
            0,
            KEY_QUERY_VALUE,
            &mut raw,
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(AutostartSnapshot { value: None });
    }
    if status != ERROR_SUCCESS {
        bail!(
            "无法备份开机启动设置: {}",
            std::io::Error::from_raw_os_error(status as i32)
        );
    }
    let key = RegistryKey(raw);
    // A single bounded read avoids a size-query/read race if another process edits the entry.
    let mut data = Vec::new();
    data.try_reserve_exact(AUTOSTART_VALUE_LIMIT)
        .context("无法分配开机启动备份缓冲区")?;
    data.resize(AUTOSTART_VALUE_LIMIT, 0u8);
    let mut length = data.len() as u32;
    let mut kind = 0;
    // SAFETY: data is writable for length bytes; type and byte count are valid out-parameters.
    let status = unsafe {
        RegQueryValueExW(
            key.0,
            wide("SightOCR").as_ptr(),
            null(),
            &mut kind,
            data.as_mut_ptr(),
            &mut length,
        )
    };
    match status {
        ERROR_FILE_NOT_FOUND => Ok(AutostartSnapshot { value: None }),
        ERROR_MORE_DATA => bail!("开机启动设置超过 64 KiB，无法安全备份"),
        ERROR_SUCCESS => {
            data.truncate(length as usize);
            Ok(AutostartSnapshot {
                value: Some((kind, data)),
            })
        }
        _ => bail!(
            "无法备份开机启动设置: {}",
            std::io::Error::from_raw_os_error(status as i32)
        ),
    }
}

impl AutostartSnapshot {
    /// Restore the exact original value/type, rather than rebuilding a command from a boolean.
    pub fn restore(&self) -> Result<()> {
        let path = wide(r"Software\Microsoft\Windows\CurrentVersion\Run");
        let mut raw = null_mut();
        let status = if self.value.is_some() {
            // SAFETY: The key path and out-parameter are live; default security is used for this HKCU key.
            unsafe {
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    path.as_ptr(),
                    0,
                    null(),
                    REG_OPTION_NON_VOLATILE,
                    KEY_SET_VALUE,
                    null(),
                    &mut raw,
                    null_mut(),
                )
            }
        } else {
            // SAFETY: Opening only an existing key avoids creating one for an absent original value.
            unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_SET_VALUE, &mut raw) }
        };
        if status == ERROR_FILE_NOT_FOUND && self.value.is_none() {
            return Ok(());
        }
        if status != ERROR_SUCCESS {
            bail!(
                "无法恢复开机启动设置: {}",
                std::io::Error::from_raw_os_error(status as i32)
            );
        }
        let key = RegistryKey(raw);
        let name = wide("SightOCR");
        let status = if let Some((kind, data)) = &self.value {
            // SAFETY: The snapshot owns the exact original bytes; their bounded length fits u32.
            unsafe {
                RegSetValueExW(
                    key.0,
                    name.as_ptr(),
                    0,
                    *kind,
                    if data.is_empty() {
                        null()
                    } else {
                        data.as_ptr()
                    },
                    data.len() as u32,
                )
            }
        } else {
            // SAFETY: The opened key is valid and the null-terminated name lives across the call.
            unsafe { RegDeleteValueW(key.0, name.as_ptr()) }
        };
        if status == ERROR_SUCCESS || (status == ERROR_FILE_NOT_FOUND && self.value.is_none()) {
            Ok(())
        } else {
            bail!(
                "无法恢复开机启动设置: {}",
                std::io::Error::from_raw_os_error(status as i32)
            )
        }
    }
}

/// Read the actual per-user startup registration, including entries from the Python app.
pub fn is_autostart_enabled() -> Result<bool> {
    let path = wide(r"Software\Microsoft\Windows\CurrentVersion\Run");
    let mut raw = null_mut();
    // SAFETY: The key path is null-terminated and raw is a writable out-parameter.
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            path.as_ptr(),
            0,
            KEY_QUERY_VALUE,
            &mut raw,
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    if status != ERROR_SUCCESS {
        bail!(
            "无法读取开机启动设置: {}",
            std::io::Error::from_raw_os_error(status as i32)
        );
    }
    let key = RegistryKey(raw);
    let mut value_type = 0;
    let mut bytes = 0;
    // SAFETY: The key is live; querying the size with a null data pointer is supported.
    let status = unsafe {
        RegQueryValueExW(
            key.0,
            wide("SightOCR").as_ptr(),
            null(),
            &mut value_type,
            null_mut(),
            &mut bytes,
        )
    };
    match status {
        ERROR_FILE_NOT_FOUND => Ok(false),
        ERROR_SUCCESS => Ok(matches!(value_type, REG_SZ | REG_EXPAND_SZ) && bytes > 2),
        _ => bail!(
            "无法读取开机启动设置: {}",
            std::io::Error::from_raw_os_error(status as i32)
        ),
    }
}

/// A per-user startup entry, so enabling startup never requires elevation.
pub fn set_autostart(enabled: bool) -> Result<()> {
    let path = wide(r"Software\Microsoft\Windows\CurrentVersion\Run");
    let name = wide("SightOCR");
    let mut raw = null_mut();
    // SAFETY: Both path and output handle storage remain valid throughout the synchronous call.
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            path.as_ptr(),
            0,
            null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            null(),
            &mut raw,
            null_mut(),
        )
    };
    if status != ERROR_SUCCESS {
        bail!(
            "无法打开开机启动设置: {}",
            std::io::Error::from_raw_os_error(status as i32)
        );
    }
    let key = RegistryKey(raw);
    let status = if enabled {
        let executable = std::env::current_exe().context("无法确定当前程序路径")?;
        // Encode the actual Windows path without a lossy UTF-8 conversion.
        let command: Vec<u16> = std::iter::once(b'"' as u16)
            .chain(executable.as_os_str().encode_wide())
            .chain("\" --silent\0".encode_utf16())
            .collect();
        // SAFETY: command is a null-terminated UTF-16 buffer; its exact byte length is passed.
        unsafe {
            RegSetValueExW(
                key.0,
                name.as_ptr(),
                0,
                REG_SZ,
                command.as_ptr().cast(),
                (command.len() * size_of::<u16>()) as u32,
            )
        }
    } else {
        // SAFETY: key is live and name is null-terminated for the duration of this call.
        unsafe { RegDeleteValueW(key.0, name.as_ptr()) }
    };
    if status != ERROR_SUCCESS && !(status == ERROR_FILE_NOT_FOUND && !enabled) {
        bail!(
            "保存开机启动设置失败: {}",
            std::io::Error::from_raw_os_error(status as i32)
        );
    }
    Ok(())
}

/// Shared with the UI-owned window subclass; activation never shows a window directly.
pub const MAIN_ACTIVATE_MESSAGE: &str = "SightOCR.MainWindow.Activate";
pub const MAIN_WINDOW_PROPERTY: &str = "SightOCR.MainWindow";
pub const MAIN_EXIT_MESSAGE: &str = "SightOCR.MainWindow.Exit";
pub const MAIN_EXIT_PROPERTY: &str = "SightOCR.MainWindow.ExitSupported";

fn request_main_window_activation(title: &str) {
    let title = wide(title);
    let marker = wide(MAIN_WINDOW_PROPERTY);
    let message_name = wide(MAIN_ACTIVATE_MESSAGE);
    let mut window = null_mut();
    loop {
        // SAFETY: Enumerates top-level HWNDs using live NUL-terminated text.
        // The marker only identifies an endpoint; its opaque value is never dereferenced.
        window = unsafe { FindWindowExW(null_mut(), window, null(), title.as_ptr()) };
        if window.is_null() {
            return;
        }
        // SAFETY: GetPropW accepts an opaque HWND and a live property name.
        if unsafe { GetPropW(window, marker.as_ptr()) }.is_null() {
            continue;
        }
        // SAFETY: RegisterWindowMessageW copies the live name; PostMessageW sends
        // scalar data only. The owning UI thread decides whether to show its window.
        unsafe {
            let message = RegisterWindowMessageW(message_name.as_ptr());
            if message != 0 && PostMessageW(window, message, 0, 0) != 0 {
                return;
            }
        }
    }
}

pub struct SingleInstance(HANDLE);
impl SingleInstance {
    pub fn acquire() -> Result<Option<Self>> {
        // SAFETY: The mutex name is null-terminated and no security attributes are supplied.
        let handle =
            unsafe { CreateMutexW(null(), 0, wide(r"Local\SightOCR.Rust.Instance").as_ptr()) };
        if handle.is_null() {
            return Err(win_error("创建单实例锁失败"));
        }
        // SAFETY: Reads the calling thread's error immediately after CreateMutexW.
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            // SAFETY: We own handle; FindWindowW receives a live null-terminated class name.
            unsafe {
                CloseHandle(handle);
                let window = FindWindowW(wide(PLATFORM_CLASS).as_ptr(), null());
                if !window.is_null() {
                    PostMessageW(window, WM_SHOW_APP, 0, 0);
                } else {
                    request_main_window_activation("SightOCR");
                }
            }
            return Ok(None);
        }
        Ok(Some(Self(handle)))
    }
}
impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: The mutex handle is owned by this wrapper and closed exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PlatformEvent {
    Ocr,
    Translate,
    SilentOcr,
    Show,
    Settings,
    Restart,
    Exit,
    Error(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Hotkey {
    modifiers: u32,
    key: u32,
}

fn parse_hotkey(text: &str) -> Result<Hotkey> {
    let mut modifiers = MOD_NOREPEAT;
    let mut key = None;
    for component in text.split('+') {
        let component = component.trim().to_ascii_uppercase();
        let modifier = match component.as_str() {
            "CTRL" | "CONTROL" => Some(MOD_CONTROL),
            "ALT" => Some(MOD_ALT),
            "SHIFT" => Some(MOD_SHIFT),
            "WIN" | "WINDOWS" | "SUPER" | "META" => Some(MOD_WIN),
            _ => None,
        };
        if let Some(modifier) = modifier {
            if modifiers & modifier != 0 {
                bail!("快捷键修饰键重复: {text}");
            }
            modifiers |= modifier;
            continue;
        }
        if key.is_some() {
            bail!("快捷键只能包含一个普通按键: {text}");
        }
        let value = match component.as_str() {
            "SPACE" | "SPACEBAR" => VK_SPACE as u32,
            "ENTER" | "RETURN" => VK_RETURN as u32,
            "TAB" => VK_TAB as u32,
            "CLEAR" => VK_CLEAR as u32,
            "PAUSE" => VK_PAUSE as u32,
            "CAPSLOCK" => VK_CAPITAL as u32,
            "ESC" | "ESCAPE" => VK_ESCAPE as u32,
            "BACKSPACE" => VK_BACK as u32,
            "DELETE" | "DEL" => VK_DELETE as u32,
            "INSERT" | "INS" => VK_INSERT as u32,
            "HOME" => VK_HOME as u32,
            "END" => VK_END as u32,
            "PAGEUP" | "PGUP" => VK_PRIOR as u32,
            "PAGEDOWN" | "PGDN" => VK_NEXT as u32,
            "LEFT" => VK_LEFT as u32,
            "RIGHT" => VK_RIGHT as u32,
            "UP" => VK_UP as u32,
            "DOWN" => VK_DOWN as u32,
            "SELECT" => VK_SELECT as u32,
            "PRINT" => VK_PRINT as u32,
            "EXECUTE" => VK_EXECUTE as u32,
            "PRINTSCREEN" | "PRTSC" => VK_SNAPSHOT as u32,
            "HELP" => VK_HELP as u32,
            "MULTIPLY" => VK_MULTIPLY as u32,
            "ADD" => VK_ADD as u32,
            "SEPARATOR" => VK_SEPARATOR as u32,
            "SUBTRACT" => VK_SUBTRACT as u32,
            "DECIMAL" => VK_DECIMAL as u32,
            "DIVIDE" => VK_DIVIDE as u32,
            "NUMLOCK" => VK_NUMLOCK as u32,
            "SCROLL" | "SCROLLLOCK" => VK_SCROLL as u32,
            "LSHIFT" => VK_LSHIFT as u32,
            "RSHIFT" => VK_RSHIFT as u32,
            "LCONTROL" => VK_LCONTROL as u32,
            "RCONTROL" => VK_RCONTROL as u32,
            "LALT" => VK_LMENU as u32,
            "RALT" => VK_RMENU as u32,
            value
                if value.len() == 7
                    && value.starts_with("NUMPAD")
                    && value.as_bytes()[6].is_ascii_digit() =>
            {
                VK_NUMPAD0 as u32 + (value.as_bytes()[6] - b'0') as u32
            }
            value if value.len() == 1 && value.as_bytes()[0].is_ascii_alphanumeric() => {
                value.as_bytes()[0] as u32
            }
            value if value.starts_with('F') => {
                let number = value[1..].parse::<u32>().unwrap_or(0);
                if !(1..=24).contains(&number) {
                    bail!("功能键范围为 F1 至 F24: {text}");
                }
                VK_F1 as u32 + number - 1
            }
            _ => bail!("不支持的快捷键: {text}（例如 F4、Ctrl+Shift+O）"),
        };
        key = Some(value);
    }
    Ok(Hotkey {
        modifiers,
        key: key.context("快捷键缺少普通按键")?,
    })
}

pub(crate) fn hotkeys_match(first: &str, second: &str) -> Result<bool> {
    Ok(parse_hotkey(first)? == parse_hotkey(second)?)
}

pub(crate) fn validate_hotkeys(ocr: &str, translate: &str, silent: &str) -> Result<()> {
    parse_hotkeys(ocr, translate, silent).map(|_| ())
}

fn parse_hotkeys(ocr: &str, translate: &str, silent: &str) -> Result<[Hotkey; 3]> {
    let keys = [
        parse_hotkey(ocr)?,
        parse_hotkey(translate)?,
        parse_hotkey(silent)?,
    ];
    if keys[0] == keys[1] || keys[0] == keys[2] || keys[1] == keys[2] {
        bail!("识别、翻译和静默识别不能使用相同的快捷键");
    }
    Ok(keys)
}

struct HotkeySettings {
    keys: [Hotkey; 3],
    labels: [String; 3],
}

impl HotkeySettings {
    fn parse(ocr: &str, translate: &str, silent: &str) -> Result<Self> {
        Ok(Self {
            keys: parse_hotkeys(ocr, translate, silent)?,
            labels: [ocr, translate, silent]
                .map(|text| text.split('+').map(str::trim).collect::<Vec<_>>().join("+")),
        })
    }
}

const PLATFORM_CLASS: &str = "SightOCR.PlatformWindow";
const WM_COMMANDS: u32 = WM_APP + 1;
const WM_TRAY: u32 = WM_APP + 2;
const WM_SHOW_APP: u32 = WM_APP + 3;

struct Update {
    hotkeys: HotkeySettings,
    hide_tray: bool,
    delivered: mpsc::Receiver<()>,
    reply: mpsc::Sender<std::result::Result<(), String>>,
}

pub struct Platform {
    hwnd: usize,
    commands: mpsc::Sender<Update>,
    worker: Option<JoinHandle<()>>,
    tray_visible: Arc<AtomicBool>,
}

impl Platform {
    /// Hotkey conflicts are emitted as Error events; tray and GUI remain usable.
    pub fn start(
        ocr_hotkey: &str,
        translate_hotkey: &str,
        silent_hotkey: &str,
        hide_tray: bool,
        sender: mpsc::Sender<PlatformEvent>,
    ) -> Result<Self> {
        let parsed = HotkeySettings::parse(ocr_hotkey, translate_hotkey, silent_hotkey)
            .map_err(|error| error.to_string());
        let (commands, receiver) = mpsc::channel();
        let (ready, startup) = mpsc::sync_channel(1);
        let tray_visible = Arc::new(AtomicBool::new(false));
        let worker_visibility = tray_visible.clone();
        let worker = thread::Builder::new()
            .name("windows-events".into())
            .spawn(move || {
                let result = run_platform(
                    parsed,
                    hide_tray,
                    sender.clone(),
                    receiver,
                    worker_visibility,
                    &ready,
                );
                if let Err(error) = result {
                    let message = format!("Windows 集成启动失败: {error:#}");
                    let _ = ready.send(Err(message.clone()));
                    let _ = sender.send(PlatformEvent::Error(message));
                }
            })
            .context("无法启动 Windows 消息线程")?;
        match startup.recv() {
            Ok(Ok(hwnd)) => Ok(Self {
                hwnd,
                commands,
                worker: Some(worker),
                tray_visible,
            }),
            failure => {
                let _ = worker.join();
                match failure {
                    Ok(Err(message)) => Err(anyhow!(message)),
                    _ => bail!("Windows 消息线程意外退出"),
                }
            }
        }
    }

    /// Actual shell icon visibility; independent of background lifetime and close handling.
    pub fn tray_visible(&self) -> bool {
        self.tray_visible.load(Ordering::Acquire)
    }

    /// The worker registers new combinations before releasing any old ones.
    pub fn update(&self, ocr: &str, translate: &str, silent: &str, hide_tray: bool) -> Result<()> {
        let hotkeys = HotkeySettings::parse(ocr, translate, silent)?;
        let (reply, result) = mpsc::channel();
        let (commit, delivered) = mpsc::channel();
        self.commands
            .send(Update {
                hotkeys,
                hide_tray,
                delivered,
                reply,
            })
            .map_err(|_| anyhow!("Windows 消息线程已退出"))?;
        // SAFETY: Posting scalar message data does not transfer pointers or access the window state.
        if unsafe { PostMessageW(self.hwnd as HWND, WM_COMMANDS, 0, 0) } == 0 {
            return Err(win_error("无法发送快捷键更新"));
        }
        // The worker may see this command through an earlier wakeup. It must wait
        // until this wakeup succeeds, so a failed PostMessage cannot mutate later.
        commit.send(()).context("Windows 消息线程已退出")?;
        result
            .recv()
            .context("Windows 消息线程未返回更新结果")?
            .map_err(|message| anyhow!(message))
    }
}

impl Drop for Platform {
    fn drop(&mut self) {
        // SAFETY: This posts only a scalar close request; the owning thread destroys its window.
        unsafe { PostMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0) };
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Registration {
    id: i32,
    key: Hotkey,
}

#[derive(Default)]
struct RegisteredHotkeys {
    entries: Vec<Registration>,
    labels: [String; 3],
}

impl RegisteredHotkeys {
    fn replace(
        &mut self,
        next: &HotkeySettings,
        register: impl FnMut(i32, Hotkey) -> Result<()>,
        unregister: impl FnMut(i32),
    ) -> Result<()> {
        let entries = replace_hotkeys(&self.entries, next.keys, register, unregister)?;
        // Commit the menu labels together with the successful registration transaction.
        // A failed registration leaves both the old bindings and their labels untouched.
        self.entries = entries;
        self.labels.clone_from(&next.labels);
        Ok(())
    }
}

/// A transaction can reuse existing combinations (including swapping their actions).
/// Failure releases only newly acquired registrations, leaving old ones untouched.
fn replace_hotkeys(
    old: &[Registration],
    keys: [Hotkey; 3],
    mut register: impl FnMut(i32, Hotkey) -> Result<()>,
    mut unregister: impl FnMut(i32),
) -> Result<Vec<Registration>> {
    let mut next = Vec::with_capacity(3);
    let mut acquired = Vec::new();
    for key in keys {
        if let Some(existing) = old.iter().find(|entry| entry.key == key) {
            next.push(*existing);
            continue;
        }
        // At most three old and three new IDs coexist during the transaction.
        let id = (1..=6)
            .find(|id| !old.iter().chain(next.iter()).any(|entry| entry.id == *id))
            .context("快捷键注册状态无效")?;
        if let Err(error) = register(id, key) {
            for id in acquired {
                unregister(id);
            }
            return Err(error);
        }
        acquired.push(id);
        next.push(Registration { id, key });
    }
    for previous in old {
        if !next.iter().any(|entry| entry.id == previous.id) {
            unregister(previous.id);
        }
    }
    Ok(next)
}

struct TrayIcon {
    handle: HICON,
    owned: bool,
}
impl TrayIcon {
    fn load() -> Self {
        let bytes = include_bytes!("../assets/icon.ico");
        // The ICO directory points to the individual image consumed by user32.
        if bytes.len() >= 6 {
            let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
            let candidate = (0..count)
                .filter_map(|index| {
                    let entry = bytes.get(6 + index * 16..6 + (index + 1) * 16)?;
                    let width = if entry[0] == 0 { 256 } else { entry[0] as i32 };
                    let length = u32::from_le_bytes(entry[8..12].try_into().ok()?) as usize;
                    let offset = u32::from_le_bytes(entry[12..16].try_into().ok()?) as usize;
                    let data = bytes.get(offset..offset.checked_add(length)?)?;
                    Some(((width - 32).abs(), data))
                })
                .min_by_key(|candidate| candidate.0);
            if let Some((_, data)) = candidate {
                // SAFETY: data is a bounds-checked icon image from our embedded ICO resource.
                let handle = unsafe {
                    CreateIconFromResourceEx(
                        data.as_ptr(),
                        data.len() as u32,
                        1,
                        0x0003_0000,
                        32,
                        32,
                        LR_DEFAULTCOLOR,
                    )
                };
                if !handle.is_null() {
                    return Self {
                        handle,
                        owned: true,
                    };
                }
            }
        }
        Self {
            // SAFETY: IDI_APPLICATION names a predefined shared icon that we do not destroy.
            handle: unsafe { LoadIconW(null_mut(), IDI_APPLICATION) },
            owned: false,
        }
    }
}
impl Drop for TrayIcon {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: Only non-shared icons returned by CreateIconFromResourceEx reach this branch.
            unsafe { DestroyIcon(self.handle) };
        }
    }
}

struct PlatformState {
    sender: mpsc::Sender<PlatformEvent>,
    commands: mpsc::Receiver<Update>,
    hotkeys: RegisteredHotkeys,
    icon: TrayIcon,
    tray_visible: bool,
    visibility: Arc<AtomicBool>,
    taskbar_created: u32,
}
impl PlatformState {
    fn notify_data(&self, hwnd: HWND) -> NOTIFYICONDATAW {
        // SAFETY: This Win32 POD structure permits all-zero initialization of unused fields.
        let mut data: NOTIFYICONDATAW = unsafe { zeroed() };
        data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = hwnd;
        data.uID = 1;
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        data.uCallbackMessage = WM_TRAY;
        data.hIcon = self.icon.handle;
        let tip = wide("SightOCR");
        data.szTip[..tip.len()].copy_from_slice(&tip);
        data
    }

    fn set_tray(&mut self, hwnd: HWND, visible: bool) -> Result<()> {
        if self.tray_visible == visible {
            return Ok(());
        }
        let operation = if visible { NIM_ADD } else { NIM_DELETE };
        // SAFETY: The complete notification structure and icon remain alive for the synchronous call.
        if unsafe { Shell_NotifyIconW(operation, &self.notify_data(hwnd)) } == 0 {
            bail!("无法{}系统托盘图标", if visible { "显示" } else { "移除" });
        }
        self.tray_visible = visible;
        self.visibility.store(visible, Ordering::Release);
        Ok(())
    }

    fn keys(&mut self, hwnd: HWND, keys: &HotkeySettings) -> Result<()> {
        self.hotkeys.replace(
            keys,
            |id, key| {
                // SAFETY: hwnd belongs to this thread and key/id were validated before registration.
                if unsafe { RegisterHotKey(hwnd, id, key.modifiers, key.key) } == 0 {
                    Err(win_error("全局快捷键被占用或不可注册；原快捷键已保留"))
                } else {
                    Ok(())
                }
            },
            |id| {
                // SAFETY: This ID was registered for the same live window on this thread.
                unsafe { UnregisterHotKey(hwnd, id) };
            },
        )?;
        Ok(())
    }

    fn update(&mut self, hwnd: HWND, update: &Update) -> Result<()> {
        let old_visible = self.tray_visible;
        self.set_tray(hwnd, !update.hide_tray)?;
        if let Err(error) = self.keys(hwnd, &update.hotkeys) {
            if let Err(rollback) = self.set_tray(hwnd, old_visible) {
                let _ = self.sender.send(PlatformEvent::Error(rollback.to_string()));
            }
            return Err(error);
        }
        Ok(())
    }

    fn cleanup(&mut self, hwnd: HWND) {
        if self.tray_visible {
            // SAFETY: The notification identifies the existing icon; referenced fields stay alive.
            unsafe { Shell_NotifyIconW(NIM_DELETE, &self.notify_data(hwnd)) };
            self.tray_visible = false;
            self.visibility.store(false, Ordering::Release);
        }
        for registration in self.hotkeys.entries.drain(..) {
            // SAFETY: Each registration is owned by this window and removed only once.
            unsafe { UnregisterHotKey(hwnd, registration.id) };
        }
    }
}

fn register_class(name: &str, procedure: WNDPROC, crosshair: bool) -> Result<()> {
    let name = wide(name);
    let class = WNDCLASSW {
        lpfnWndProc: procedure,
        // SAFETY: A null module name requests the process module without changing its ownership.
        hInstance: unsafe { GetModuleHandleW(null()) },
        // Capture selects its owned PNG cursor in WM_SETCURSOR. The class must not retain
        // that short-lived handle after the overlay closes; other windows use the system arrow.
        hCursor: if crosshair {
            null_mut()
        } else {
            // SAFETY: IDC_ARROW names a shared system cursor with process lifetime.
            unsafe { LoadCursorW(null_mut(), IDC_ARROW) }
        },
        lpszClassName: name.as_ptr(),
        // SAFETY: WNDCLASSW is POD and all unused pointer fields may be null.
        ..unsafe { zeroed() }
    };
    // SAFETY: Class name and callback remain valid; user32 copies the class metadata.
    if unsafe { RegisterClassW(&class) } == 0
        // SAFETY: Reads the error from the immediately preceding registration call.
        && unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS
    {
        return Err(win_error("注册窗口类失败"));
    }
    Ok(())
}

fn run_platform(
    parsed: std::result::Result<HotkeySettings, String>,
    hide_tray: bool,
    sender: mpsc::Sender<PlatformEvent>,
    commands: mpsc::Receiver<Update>,
    visibility: Arc<AtomicBool>,
    ready: &mpsc::SyncSender<std::result::Result<usize, String>>,
) -> Result<()> {
    register_class(PLATFORM_CLASS, Some(platform_proc), false)?;
    let mut state = Box::new(PlatformState {
        sender,
        commands,
        hotkeys: RegisteredHotkeys::default(),
        icon: TrayIcon::load(),
        tray_visible: false,
        visibility,
        // SAFETY: This synchronous call copies the null-terminated registered message name.
        taskbar_created: unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) },
    });
    // SAFETY: The boxed state has a stable address and outlives this window and every callback.
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            wide(PLATFORM_CLASS).as_ptr(),
            wide("SightOCR events").as_ptr(),
            0,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            GetModuleHandleW(null()),
            (&mut *state as *mut PlatformState).cast(),
        )
    };
    if hwnd.is_null() {
        return Err(win_error("创建 Windows 消息窗口失败"));
    }
    if let Err(error) = state.set_tray(hwnd, !hide_tray) {
        let _ = state.sender.send(PlatformEvent::Error(error.to_string()));
    }
    let key_result = parsed
        .map_err(|message| anyhow!(message))
        .and_then(|keys| state.keys(hwnd, &keys));
    if let Err(error) = key_result {
        let _ = state.sender.send(PlatformEvent::Error(error.to_string()));
    }
    if ready.send(Ok(hwnd as usize)).is_err() {
        // SAFETY: hwnd is a successfully created window owned by this thread.
        unsafe { DestroyWindow(hwnd) };
        return Ok(());
    }
    // SAFETY: MSG is a Win32 POD structure whose fields may initially be zero.
    let mut message: MSG = unsafe { zeroed() };
    loop {
        // SAFETY: message is writable and the null HWND requests this thread's messages.
        let result = unsafe { GetMessageW(&mut message, null_mut(), 0, 0) };
        if result <= 0 {
            if result < 0 {
                let _ = state.sender.send(PlatformEvent::Error(
                    win_error("Windows 消息循环失败").to_string(),
                ));
            }
            break;
        }
        // SAFETY: GetMessageW returned a valid initialized message for this thread.
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    // WM_DESTROY normally cleans up. This also covers an unexpected WM_QUIT.
    // SAFETY: IsWindow accepts stale HWND values and does not dereference Rust state.
    if unsafe { IsWindow(hwnd) } != 0 {
        // SAFETY: The live window belongs to this thread and its state is still boxed above.
        unsafe { DestroyWindow(hwnd) };
    }
    Ok(())
}

unsafe extern "system" fn platform_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // A Rust panic must never unwind through a Windows callback frame.
    match std::panic::catch_unwind(|| platform_dispatch(hwnd, message, wparam, lparam)) {
        Ok(result) => result,
        Err(_) => {
            let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut PlatformState;
            if !pointer.is_null() {
                let _ = (*pointer).sender.send(PlatformEvent::Error(
                    "Windows 消息处理异常，系统托盘服务已停止".into(),
                ));
            }
            PostQuitMessage(1);
            0
        }
    }
}

unsafe fn platform_dispatch(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if message == WM_NCCREATE {
        let create = &*(lparam as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
    }
    let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut PlatformState;
    if pointer.is_null() {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }
    // Do not hold a reference across APIs that dispatch nested window messages.
    match message {
        WM_COMMANDS => {
            while let Ok(update) = (*pointer).commands.try_recv() {
                if update.delivered.recv().is_err() {
                    continue;
                }
                let result = (*pointer)
                    .update(hwnd, &update)
                    .map_err(|error| error.to_string());
                let _ = update.reply.send(result);
            }
            0
        }
        WM_HOTKEY => {
            let index = (*pointer)
                .hotkeys
                .entries
                .iter()
                .position(|entry| entry.id == wparam as i32);
            let event = match index {
                Some(0) => Some(PlatformEvent::Ocr),
                Some(1) => Some(PlatformEvent::Translate),
                Some(2) => Some(PlatformEvent::SilentOcr),
                _ => None,
            };
            if let Some(event) = event {
                let _ = (*pointer).sender.send(event);
            }
            0
        }
        WM_SHOW_APP => {
            let _ = (*pointer).sender.send(PlatformEvent::Show);
            0
        }
        WM_TRAY => {
            match lparam as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                    let _ = (*pointer).sender.send(PlatformEvent::Show);
                }
                WM_RBUTTONUP | WM_CONTEXTMENU => {
                    // The native menu runs a nested message loop. Copy the committed labels
                    // before entering it, so no borrow of state crosses reentrant updates.
                    let labels = (*pointer).hotkeys.labels.clone();
                    let event = match tray_menu(hwnd, &labels) {
                        Ok(selected) => tray_command(selected),
                        Err(error) => Some(PlatformEvent::Error(error.to_string())),
                    };
                    if let Some(event) = event {
                        let _ = (*pointer).sender.send(event);
                    }
                }
                _ => {}
            }
            0
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            (*pointer).cleanup(hwnd);
            PostQuitMessage(0);
            0
        }
        WM_NCDESTROY => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        _ if message != 0 && message == (*pointer).taskbar_created => {
            if (*pointer).tray_visible {
                (*pointer).tray_visible = false;
                (*pointer).visibility.store(false, Ordering::Release);
                if let Err(error) = (*pointer).set_tray(hwnd, true) {
                    let _ = (*pointer)
                        .sender
                        .send(PlatformEvent::Error(error.to_string()));
                }
            }
            0
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

fn tray_items(labels: &[String; 3]) -> [(usize, String); 7] {
    let shortcut = |label: &str, key: &str| {
        if key.is_empty() {
            label.to_owned()
        } else {
            // A native menu tab right-aligns the active shortcut in its own column.
            format!("{label}\t{key}")
        }
    };
    [
        (1, "主界面".into()),
        (7, shortcut("静默识别", &labels[2])),
        (2, shortcut("截图识别", &labels[0])),
        (3, shortcut("截图翻译", &labels[1])),
        (4, "设置".into()),
        (5, "重启".into()),
        (6, "退出".into()),
    ]
}

fn tray_command(command: i32) -> Option<PlatformEvent> {
    match command {
        1 => Some(PlatformEvent::Show),
        2 => Some(PlatformEvent::Ocr),
        3 => Some(PlatformEvent::Translate),
        4 => Some(PlatformEvent::Settings),
        5 => Some(PlatformEvent::Restart),
        6 => Some(PlatformEvent::Exit),
        7 => Some(PlatformEvent::SilentOcr),
        _ => None,
    }
}

struct TrayMenu(HMENU);

impl TrayMenu {
    fn new(labels: &[String; 3]) -> Result<Self> {
        // SAFETY: CreatePopupMenu takes no pointers and returns a new owned menu.
        let menu = Self(unsafe { CreatePopupMenu() });
        if menu.0.is_null() {
            return Err(win_error("创建托盘菜单失败"));
        }
        for (id, label) in tray_items(labels) {
            // SAFETY: The menu is live; user32 copies the temporary NUL-terminated label.
            if unsafe { AppendMenuW(menu.0, MF_STRING, id, wide(&label).as_ptr()) } == 0 {
                return Err(win_error("添加托盘菜单项失败"));
            }
        }
        Ok(menu)
    }
}

impl Drop for TrayMenu {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: This guard exclusively owns the menu and drops after tracking finishes.
            unsafe { DestroyMenu(self.0) };
        }
    }
}

fn tray_menu(hwnd: HWND, labels: &[String; 3]) -> Result<i32> {
    let menu = TrayMenu::new(labels)?;
    // SAFETY: POINT is POD and zero initialization is valid.
    let mut point: POINT = unsafe { zeroed() };
    // SAFETY: point is valid writable storage for the cursor coordinates.
    if unsafe { GetCursorPos(&mut point) } == 0 {
        return Err(win_error("读取托盘菜单位置失败"));
    }
    // SAFETY: hwnd is the live platform window on this thread. Tracking borrows the owned
    // menu synchronously; scalar WM_NULL restores normal dismissal behavior afterward.
    let selection = unsafe {
        SetForegroundWindow(hwnd);
        let selected = TrackPopupMenu(
            menu.0,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            0,
            hwnd,
            null(),
        );
        PostMessageW(hwnd, WM_NULL, 0, 0);
        selected
    };
    Ok(selection)
}

struct ScreenDc(HDC);
impl Drop for ScreenDc {
    fn drop(&mut self) {
        // SAFETY: This wrapper owns the screen DC obtained with GetDC(null).
        unsafe { ReleaseDC(null_mut(), self.0) };
    }
}

struct BitmapSurface {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    pixels: *mut u8,
    width: i32,
    height: i32,
    length: usize,
}
impl BitmapSurface {
    fn new(screen: HDC, width: i32, height: i32) -> Result<Self> {
        if width <= 0 || height <= 0 {
            bail!("屏幕尺寸无效");
        }
        let length = (width as usize)
            .checked_mul(height as usize)
            .and_then(|size| size.checked_mul(4))
            .filter(|length| *length <= 256 * 1024 * 1024)
            .context("桌面尺寸过大，无法分配截图缓冲区")?;
        // SAFETY: screen is a live screen device context owned by the calling capture scope.
        let dc = unsafe { CreateCompatibleDC(screen) };
        if dc.is_null() {
            return Err(win_error("创建截图设备上下文失败"));
        }
        // SAFETY: BITMAPINFO is POD; unused palette entries are valid when zero.
        let mut info: BITMAPINFO = unsafe { zeroed() };
        info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = width;
        info.bmiHeader.biHeight = -height; // top-down: same coordinates as the virtual desktop
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;
        let mut pixels: *mut c_void = null_mut();
        // SAFETY: The top-down 32-bpp header is initialized and pixels is a writable out-parameter.
        let bitmap =
            unsafe { CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut pixels, null_mut(), 0) };
        if bitmap.is_null() {
            let error = win_error("创建截图位图失败");
            // SAFETY: dc was created above and no bitmap was selected into it.
            unsafe { DeleteDC(dc) };
            return Err(error);
        }
        // SAFETY: Both handles are live and this bitmap is not selected into any other DC.
        let previous = unsafe { SelectObject(dc, bitmap) };
        if previous.is_null() || previous as isize == -1 {
            let error = win_error("选择截图位图失败");
            // SAFETY: Selection failed; the unselected bitmap and owned DC can be released.
            unsafe {
                DeleteObject(bitmap);
                DeleteDC(dc);
            }
            return Err(error);
        }
        Ok(Self {
            dc,
            bitmap,
            previous,
            pixels: pixels.cast(),
            width,
            height,
            length,
        })
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: The DIB allocation covers length bytes and outlives this borrow; callers flush GDI first.
        unsafe { std::slice::from_raw_parts(self.pixels, self.length) }
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: The DIB is exclusively borrowed for its checked allocation length;
        // callers flush this thread's GDI access before borrowing the pixel memory.
        unsafe { std::slice::from_raw_parts_mut(self.pixels, self.length) }
    }
}
impl Drop for BitmapSurface {
    fn drop(&mut self) {
        // SAFETY: Restore the original object before releasing our selected bitmap, then its owned DC.
        unsafe {
            SelectObject(self.dc, self.previous);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PixelPoint {
    x: i32,
    y: i32,
}

fn selection_bounds(start: PixelPoint, end: PixelPoint, width: i32, height: i32) -> RECT {
    RECT {
        left: start.x.min(end.x).clamp(0, width),
        top: start.y.min(end.y).clamp(0, height),
        right: start.x.max(end.x).clamp(0, width),
        bottom: start.y.max(end.y).clamp(0, height),
    }
}

struct CaptureState {
    original: BitmapSurface,
    frame: BitmapSurface,
    cursor: native_cursor::CaptureCursor,
    left: i32,
    top: i32,
    start: PixelPoint,
    end: PixelPoint,
    dragging: bool,
    done: bool,
    cancelled: bool,
}
impl CaptureState {
    fn pointer(&self) -> PixelPoint {
        // SAFETY: POINT is POD and its zero value is valid.
        let mut point: POINT = unsafe { zeroed() };
        // SAFETY: point is a writable POINT; failure safely retains its initialized value.
        unsafe { GetCursorPos(&mut point) };
        PixelPoint {
            x: point
                .x
                .saturating_sub(self.left)
                .clamp(0, self.original.width),
            y: point
                .y
                .saturating_sub(self.top)
                .clamp(0, self.original.height),
        }
    }
    fn bounds(&self) -> RECT {
        selection_bounds(
            self.start,
            self.end,
            self.original.width,
            self.original.height,
        )
    }

    fn repaint(&mut self, dirty: RECT) -> Option<RECT> {
        let selection = self.dragging.then(|| self.bounds());
        // SAFETY: The surfaces are confined to this thread. Complete any previous
        // BitBlt reading frame before the CPU updates its DIB allocation in place.
        unsafe { GdiFlush() };
        capture_render::repaint(
            self.original.bytes(),
            self.frame.bytes_mut(),
            self.original.width,
            self.original.height,
            selection,
            dirty,
        )
    }
}

static CAPTURE_LOCK: Mutex<()> = Mutex::new(());
const CAPTURE_CLASS: &str = "SightOCR.CaptureOverlay";

/// Freeze the complete virtual desktop before showing an overlay, then return only
/// the chosen pixels. Esc, right click, and an empty selection return None.
pub fn capture_region() -> Result<Option<RgbaImage>> {
    let _capture = CAPTURE_LOCK
        .try_lock()
        .map_err(|_| anyhow!("截图选择已在进行中"))?;
    let _dpi = ThreadDpi::physical_pixels();
    // SAFETY: A null HWND requests the desktop DC, which ScreenDc releases on every exit path.
    let screen = ScreenDc(unsafe { GetDC(null_mut()) });
    if screen.0.is_null() {
        return Err(win_error("无法访问桌面图像"));
    }
    // SAFETY: The metric identifiers are valid constants; these APIs take no borrowed pointers.
    let (left, top, width, height) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    let original = BitmapSurface::new(screen.0, width, height)?;
    // SAFETY: Source/destination DCs are live and the destination allocation covers the checked dimensions.
    if unsafe {
        BitBlt(
            original.dc,
            0,
            0,
            width,
            height,
            screen.0,
            left,
            top,
            SRCCOPY | CAPTUREBLT,
        )
    } == 0
    {
        return Err(win_error("截取桌面图像失败"));
    }
    // SAFETY: Flush this thread's GDI writes before reading the DIB's pixel allocation.
    unsafe { GdiFlush() };
    // Reuse the second DIB for complete preview frames. Mouse movement allocates
    // neither another desktop snapshot nor another full-screen drawing surface.
    let frame = BitmapSurface::new(screen.0, width, height)?;
    drop(screen);
    register_class(CAPTURE_CLASS, Some(capture_proc), true)?;
    let mut state = Box::new(CaptureState {
        original,
        frame,
        cursor: native_cursor::CaptureCursor::load()?,
        left,
        top,
        start: PixelPoint::default(),
        end: PixelPoint::default(),
        dragging: false,
        done: false,
        cancelled: true,
    });
    // SAFETY: The boxed CaptureState has a stable address and remains alive until after DestroyWindow.
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            wide(CAPTURE_CLASS).as_ptr(),
            wide("SightOCR — 拖动截图 / Esc 取消").as_ptr(),
            WS_POPUP,
            left,
            top,
            width,
            height,
            null_mut(),
            null_mut(),
            GetModuleHandleW(null()),
            (&mut *state as *mut CaptureState).cast(),
        )
    };
    if hwnd.is_null() {
        return Err(win_error("创建截图选区窗口失败"));
    }
    // SAFETY: This thread owns the initialized overlay HWND and keeps its boxed state alive.
    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        SetWindowPos(hwnd, HWND_TOPMOST, left, top, width, height, SWP_SHOWWINDOW);
        SetForegroundWindow(hwnd);
        SetFocus(hwnd);
        UpdateWindow(hwnd);
    }
    state.cursor.activate();
    // SAFETY: MSG is POD; GetMessageW initializes its fields before dispatch.
    let mut message: MSG = unsafe { zeroed() };
    let mut loop_error = None;
    while !state.done {
        // SAFETY: message is writable and null HWND selects this capture thread's message queue.
        let result = unsafe { GetMessageW(&mut message, null_mut(), 0, 0) };
        if result <= 0 {
            if result < 0 {
                loop_error = Some(win_error("截图消息循环失败"));
            }
            state.cancelled = true;
            break;
        }
        // SAFETY: This initialized message came from a successful GetMessageW on the same thread.
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    // SAFETY: The live boxed state outlives window destruction; capture and GDI objects belong to this thread.
    unsafe {
        if GetCapture() == hwnd {
            ReleaseCapture();
        }
        if IsWindow(hwnd) != 0 {
            DestroyWindow(hwnd);
        }
        GdiFlush();
    }
    if let Some(error) = loop_error {
        return Err(error);
    }
    if state.cancelled {
        return Ok(None);
    }
    let rectangle = state.bounds();
    let crop_width = rectangle.right - rectangle.left;
    let crop_height = rectangle.bottom - rectangle.top;
    if crop_width <= 0 || crop_height <= 0 {
        return Ok(None);
    }
    let source = state.original.bytes();
    let output_length = crop_width as usize * crop_height as usize * 4;
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(output_length)
        .context("内存不足，无法创建截图结果")?;
    pixels.resize(output_length, 0);
    let mut output = RgbaImage::from_raw(crop_width as u32, crop_height as u32, pixels)
        .context("截图缓冲区尺寸不匹配")?;
    for (row, destination) in output
        .as_mut()
        .chunks_exact_mut(crop_width as usize * 4)
        .enumerate()
    {
        let offset =
            ((rectangle.top as usize + row) * width as usize + rectangle.left as usize) * 4;
        for (bgra, rgba) in source[offset..offset + destination.len()]
            .as_chunks::<4>()
            .0
            .iter()
            .zip(destination.as_chunks_mut::<4>().0.iter_mut())
        {
            rgba.copy_from_slice(&[bgra[2], bgra[1], bgra[0], 255]);
        }
    }
    Ok(Some(output))
}

unsafe extern "system" fn capture_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // Keep unwinding inside Rust and end this selection if callback processing fails.
    match std::panic::catch_unwind(|| capture_dispatch(hwnd, message, wparam, lparam)) {
        Ok(result) => result,
        Err(_) => {
            let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut CaptureState;
            if !pointer.is_null() {
                (*pointer).done = true;
                (*pointer).cancelled = true;
            }
            0
        }
    }
}

unsafe fn capture_dispatch(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if message == WM_NCCREATE {
        let create = &*(lparam as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
    }
    let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut CaptureState;
    if pointer.is_null() {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }
    match message {
        WM_SETCURSOR => {
            (*pointer).cursor.activate();
            1
        }
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut paint: PAINTSTRUCT = zeroed();
            let dc = BeginPaint(hwnd, &mut paint);
            let state = &mut *pointer;
            if let Some(dirty) = state.repaint(paint.rcPaint) {
                // Present the fully composed dirty rectangle once. Painting the
                // dim layer and selection separately to this DC causes flicker.
                BitBlt(
                    dc,
                    dirty.left,
                    dirty.top,
                    dirty.right - dirty.left,
                    dirty.bottom - dirty.top,
                    state.frame.dc,
                    dirty.left,
                    dirty.top,
                    SRCCOPY,
                );
            }
            EndPaint(hwnd, &paint);
            0
        }
        WM_LBUTTONDOWN => {
            (*pointer).start = (*pointer).pointer();
            (*pointer).end = (*pointer).start;
            (*pointer).dragging = true;
            SetCapture(hwnd);
            0
        }
        WM_MOUSEMOVE => {
            if (*pointer).dragging {
                let previous = (*pointer).bounds();
                (*pointer).end = (*pointer).pointer();
                let current = (*pointer).bounds();
                if previous.left != current.left
                    || previous.top != current.top
                    || previous.right != current.right
                    || previous.bottom != current.bottom
                {
                    // Windows coalesces these regions until WM_PAINT; the final
                    // selection is composited across all changed pixels at once.
                    let dirty = capture_render::damage(previous, current);
                    InvalidateRect(hwnd, &dirty, 0);
                }
            }
            0
        }
        WM_LBUTTONUP => {
            if (*pointer).dragging {
                (*pointer).end = (*pointer).pointer();
                (*pointer).dragging = false;
                (*pointer).cancelled = false;
                (*pointer).done = true;
                ReleaseCapture();
                ShowWindow(hwnd, SW_HIDE);
            }
            0
        }
        WM_KEYDOWN if wparam == VK_ESCAPE as usize => {
            (*pointer).done = true;
            (*pointer).cancelled = true;
            0
        }
        WM_RBUTTONDOWN | WM_CLOSE | WM_CANCELMODE | WM_DISPLAYCHANGE => {
            (*pointer).done = true;
            (*pointer).cancelled = true;
            0
        }
        WM_CAPTURECHANGED if (*pointer).dragging => {
            (*pointer).done = true;
            (*pointer).cancelled = true;
            0
        }
        WM_ACTIVATE if !(*pointer).done && wparam & 0xffff == WA_INACTIVE as usize => {
            (*pointer).done = true;
            (*pointer).cancelled = true;
            0
        }
        WM_DESTROY => {
            (*pointer).done = true;
            0
        }
        WM_NCDESTROY => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

#[cfg(test)]
mod native_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn fallback_activation_posts_only_to_marked_test_windows_without_showing_them() -> Result<()> {
        struct TestWindow(HWND);
        impl Drop for TestWindow {
            fn drop(&mut self) {
                // SAFETY: The hidden window was created on and is owned by this test thread.
                unsafe { DestroyWindow(self.0) };
            }
        }
        let title = format!("SightOCR activation test {}", std::process::id());
        let create = || -> Result<TestWindow> {
            // SAFETY: STATIC is a built-in class. All names are live for the call;
            // no visible style, application state pointer, or external HWND is supplied.
            let window = unsafe {
                CreateWindowExW(
                    0,
                    wide("STATIC").as_ptr(),
                    wide(&title).as_ptr(),
                    0,
                    0,
                    0,
                    1,
                    1,
                    null_mut(),
                    null_mut(),
                    GetModuleHandleW(null()),
                    null(),
                )
            };
            if window.is_null() {
                return Err(win_error("创建隐藏的激活测试窗口失败"));
            }
            Ok(TestWindow(window))
        };
        let marked = create()?;
        let decoy = create()?;
        assert_ne!(
            // SAFETY: The property value is a non-null marker, not an owned pointer.
            unsafe {
                SetPropW(
                    marked.0,
                    wide(MAIN_WINDOW_PROPERTY).as_ptr(),
                    1usize as HANDLE,
                )
            },
            0
        );
        // SAFETY: The API copies this live NUL-terminated message name.
        let message = unsafe { RegisterWindowMessageW(wide(MAIN_ACTIVATE_MESSAGE).as_ptr()) };
        assert_ne!(message, 0);
        request_main_window_activation(&title);
        // SAFETY: MSG is POD and PeekMessageW writes only to this local storage,
        // filtering exclusively for the two test-owned windows and registered message.
        unsafe {
            let mut notification: MSG = zeroed();
            assert_ne!(
                PeekMessageW(&mut notification, marked.0, message, message, PM_REMOVE),
                0
            );
            assert_eq!(
                PeekMessageW(&mut notification, decoy.0, message, message, PM_REMOVE),
                0
            );
            assert_eq!(IsWindowVisible(marked.0), 0);
            assert_eq!(IsWindowVisible(decoy.0), 0);
            RemovePropW(marked.0, wide(MAIN_WINDOW_PROPERTY).as_ptr());
            request_main_window_activation(&title);
            assert_eq!(
                PeekMessageW(&mut notification, marked.0, message, message, PM_REMOVE),
                0
            );
        }
        Ok(())
    }

    #[test]
    fn hotkeys_accept_normalized_modifiers_and_validate_keys() {
        assert_eq!(
            parse_hotkey(" ctrl + Shift + o ").unwrap(),
            Hotkey {
                modifiers: MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT,
                key: b'O' as u32
            }
        );
        assert_eq!(parse_hotkey("F24").unwrap().key, VK_F24 as u32);
        for invalid in ["", "Ctrl", "Ctrl+Ctrl+O", "F0", "F25", "A+B", "Ctrl+", "🐈"] {
            assert!(parse_hotkey(invalid).is_err(), "accepted {invalid:?}");
        }
        for keys in [
            ["Ctrl+O", "control+o", "F4"],
            ["Ctrl+O", "F2", "control+o"],
            ["F5", "Win+Shift+O", "shift+meta+o"],
        ] {
            assert!(parse_hotkeys(keys[0], keys[1], keys[2]).is_err());
        }
        assert!(parse_hotkeys("F5", "F2", "F4").is_ok());
    }

    #[test]
    fn tray_menu_commands_match_the_seven_visible_actions() {
        let labels = ["Ctrl+Shift+O".into(), "Alt+F8".into(), "F4".into()];
        assert_eq!(
            tray_items(&labels),
            [
                (1, "主界面".into()),
                (7, "静默识别\tF4".into()),
                (2, "截图识别\tCtrl+Shift+O".into()),
                (3, "截图翻译\tAlt+F8".into()),
                (4, "设置".into()),
                (5, "重启".into()),
                (6, "退出".into()),
            ]
        );
        for (command, expected) in [
            (1, PlatformEvent::Show),
            (2, PlatformEvent::Ocr),
            (3, PlatformEvent::Translate),
            (4, PlatformEvent::Settings),
            (5, PlatformEvent::Restart),
            (6, PlatformEvent::Exit),
            (7, PlatformEvent::SilentOcr),
        ] {
            assert_eq!(tray_command(command), Some(expected));
        }
        assert_eq!(tray_command(0), None);
        assert_eq!(tray_command(8), None);
    }

    #[test]
    fn tray_labels_follow_only_committed_hotkey_transactions() {
        let active = RefCell::new(Vec::<Registration>::new());
        let mut bindings = RegisteredHotkeys::default();
        let original = HotkeySettings::parse(" F5 ", " F2 ", " F4 ").unwrap();
        bindings
            .replace(
                &original,
                |id, key| {
                    active.borrow_mut().push(Registration { id, key });
                    Ok(())
                },
                |_| panic!("initial registration must not unregister"),
            )
            .unwrap();
        let previous = bindings.entries.clone();
        let requested = HotkeySettings::parse("Ctrl+Shift+O", "Alt+F8", "Ctrl+F9").unwrap();
        assert!(bindings
            .replace(
                &requested,
                |id, key| {
                    if key == requested.keys[2] {
                        bail!("occupied");
                    }
                    active.borrow_mut().push(Registration { id, key });
                    Ok(())
                },
                |id| active.borrow_mut().retain(|entry| entry.id != id),
            )
            .is_err());
        assert_eq!(bindings.entries, previous);
        assert_eq!(*active.borrow(), previous);
        assert_eq!(bindings.labels, original.labels);
        assert_eq!(tray_items(&bindings.labels)[1].1, "静默识别\tF4");
        assert_eq!(tray_items(&bindings.labels)[2].1, "截图识别\tF5");
        assert_eq!(tray_items(&bindings.labels)[3].1, "截图翻译\tF2");

        // Reuse the previous translate combination for OCR and add a new translate key.
        let replacement = HotkeySettings::parse(" F2 ", " Ctrl + Shift + O ", " F5 ").unwrap();
        bindings
            .replace(
                &replacement,
                |id, key| {
                    active.borrow_mut().push(Registration { id, key });
                    Ok(())
                },
                |id| active.borrow_mut().retain(|entry| entry.id != id),
            )
            .unwrap();
        assert_eq!(bindings.labels, ["F2", "Ctrl+Shift+O", "F5"]);
        assert_eq!(bindings.entries[0], previous[1]);
        assert_eq!(bindings.entries[1].key, replacement.keys[1]);
        assert_eq!(bindings.entries[2], previous[0]);
        assert_eq!(tray_items(&bindings.labels)[1].1, "静默识别\tF5");
        assert_eq!(tray_items(&bindings.labels)[2].1, "截图识别\tF2");
        assert_eq!(tray_items(&bindings.labels)[3].1, "截图翻译\tCtrl+Shift+O");
    }

    #[test]
    fn failed_initial_registration_does_not_advertise_inactive_shortcuts() {
        let mut bindings = RegisteredHotkeys::default();
        let requested = HotkeySettings::parse("F5", "F2", "F4").unwrap();
        assert!(bindings
            .replace(&requested, |_, _| bail!("occupied"), |_| {})
            .is_err());
        assert!(bindings.entries.is_empty());
        assert_eq!(tray_items(&bindings.labels)[1].1, "静默识别");
        assert_eq!(tray_items(&bindings.labels)[2].1, "截图识别");
        assert_eq!(tray_items(&bindings.labels)[3].1, "截图翻译");
    }

    #[test]
    fn failed_registration_keeps_old_hotkeys_and_releases_only_new_ones() {
        let old = [
            Registration {
                id: 1,
                key: parse_hotkey("F4").unwrap(),
            },
            Registration {
                id: 2,
                key: parse_hotkey("F2").unwrap(),
            },
            Registration {
                id: 3,
                key: parse_hotkey("F5").unwrap(),
            },
        ];
        let active = RefCell::new(old.to_vec());
        let new = parse_hotkeys("F8", "F9", "F10").unwrap();
        let result = replace_hotkeys(
            &old,
            new,
            |id, key| {
                if key == new[2] {
                    bail!("occupied");
                }
                active.borrow_mut().push(Registration { id, key });
                Ok(())
            },
            |id| active.borrow_mut().retain(|entry| entry.id != id),
        );
        assert!(result.is_err());
        assert_eq!(*active.borrow(), old);
    }

    #[test]
    fn swapping_actions_does_not_unregister_existing_hotkeys() {
        let old = [
            Registration {
                id: 1,
                key: parse_hotkey("F4").unwrap(),
            },
            Registration {
                id: 2,
                key: parse_hotkey("F2").unwrap(),
            },
            Registration {
                id: 3,
                key: parse_hotkey("F5").unwrap(),
            },
        ];
        let next = replace_hotkeys(
            &old,
            [old[2].key, old[0].key, old[1].key],
            |_, _| panic!("must reuse"),
            |_| panic!("must retain"),
        )
        .unwrap();
        assert_eq!(next, [old[2], old[0], old[1]]);
    }

    #[test]
    fn successful_registration_releases_only_obsolete_keys() {
        let old = [
            Registration {
                id: 1,
                key: parse_hotkey("F4").unwrap(),
            },
            Registration {
                id: 2,
                key: parse_hotkey("F2").unwrap(),
            },
            Registration {
                id: 3,
                key: parse_hotkey("F5").unwrap(),
            },
        ];
        let active = RefCell::new(old.to_vec());
        let keys = parse_hotkeys("F4", "F8", "F2").unwrap();
        let next = replace_hotkeys(
            &old,
            keys,
            |id, key| {
                active.borrow_mut().push(Registration { id, key });
                Ok(())
            },
            |id| active.borrow_mut().retain(|entry| entry.id != id),
        )
        .unwrap();
        assert_eq!(active.borrow().len(), next.len());
        assert!(active.borrow().iter().all(|entry| next.contains(entry)));
        assert_eq!(next[0], old[0]);
        assert_eq!(next[1].key, keys[1]);
        assert_eq!(next[2], old[1]);
    }

    #[test]
    fn reversed_selection_is_clamped_to_virtual_desktop() {
        let rect = selection_bounds(
            PixelPoint { x: 200, y: 150 },
            PixelPoint { x: -20, y: -5 },
            100,
            100,
        );
        assert_eq!(
            (rect.left, rect.top, rect.right, rect.bottom),
            (0, 0, 100, 100)
        );
    }
}
