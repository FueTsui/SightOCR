//! Match the native Windows caption to the app while retaining system window behavior.
//!
//! DWM controls corner rounding (including the square corners of a maximized window).
//! Unsupported attributes on older Windows releases are an optional visual enhancement.
//! See https://learn.microsoft.com/windows/win32/api/dwmapi/ne-dwmapi-dwmwindowattribute.

use anyhow::{bail, ensure, Result};
use std::{
    cell::Cell,
    mem::{size_of, size_of_val, zeroed},
    panic::{catch_unwind, AssertUnwindSafe},
    ptr::null_mut,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::{
        Dwm::{
            DwmFlush, DwmSetWindowAttribute, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR,
            DWMWA_TRANSITIONS_FORCEDISABLED, DWMWA_USE_IMMERSIVE_DARK_MODE,
            DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
        },
        Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST},
    },
    System::Threading::GetCurrentThreadId,
    UI::{
        HiDpi::{
            SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT,
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        },
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{
            GetAncestor, GetPropW, GetWindowRect, GetWindowThreadProcessId, IsIconic,
            IsWindowVisible, IsZoomed, PostMessageW, RegisterWindowMessageW, RemovePropW,
            SetForegroundWindow, SetPropW, SetWindowPos, ShowWindow, ShowWindowAsync, GA_ROOT,
            HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER,
            SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, SW_MINIMIZE, SW_RESTORE, SW_SHOW, WM_NCDESTROY,
            WM_PAINT,
        },
    },
};

fn is_own_top_level_window(hwnd: HWND) -> bool {
    if hwnd.is_null() {
        return false;
    }
    let mut owner = 0;
    // SAFETY: These APIs validate opaque HWND values; owner is writable local storage.
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut owner) != 0
            && owner == std::process::id()
            && GetAncestor(hwnd, GA_ROOT) == hwnd
    }
}

/// Read native visibility only for a live top-level window owned by this process.
/// This is a state snapshot, not a fence for pending eframe or Win32 show commands.
pub fn is_visible(hwnd: usize) -> bool {
    let hwnd = hwnd as HWND;
    is_own_top_level_window(hwnd)
        // SAFETY: The short-circuit check verifies this process's live top-level HWND.
        && unsafe { IsWindowVisible(hwnd) != 0 }
}

const ACTIVATION_SUBCLASS_ID: usize = 0x534F_4143;

struct ActivationState {
    requested: Arc<AtomicBool>,
    exit_requested: Arc<AtomicBool>,
    context: eframe::egui::Context,
    message: u32,
    exit_message: u32,
    property: Vec<u16>,
    exit_property: Vec<u16>,
    exit_marked: Cell<bool>,
    attached: Cell<bool>,
}

/// Keep activation and installer shutdown available when hotkeys or tray setup failed.
/// The Rc makes this guard UI-thread-only. App consumes the atomic flag and decides
/// whether to show; the native callback never changes visibility or foreground focus.
pub struct MainWindowActivation {
    hwnd: HWND,
    state: Rc<ActivationState>,
}

impl MainWindowActivation {
    pub fn install(
        hwnd: usize,
        requested: Arc<AtomicBool>,
        exit_requested: Arc<AtomicBool>,
        context: eframe::egui::Context,
    ) -> Result<Self> {
        let hwnd = hwnd as HWND;
        ensure!(is_own_top_level_window(hwnd), "无法验证主窗口激活入口归属");
        ensure!(
            // SAFETY: The HWND is owned and both queries only read thread identifiers.
            unsafe { GetWindowThreadProcessId(hwnd, null_mut()) == GetCurrentThreadId() },
            "主窗口激活入口必须在窗口所属线程安装"
        );
        let property: Vec<u16> = sightocr::platform::MAIN_WINDOW_PROPERTY
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let name: Vec<u16> = sightocr::platform::MAIN_ACTIVATE_MESSAGE
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let exit_property: Vec<u16> = sightocr::platform::MAIN_EXIT_PROPERTY
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let exit_name: Vec<u16> = sightocr::platform::MAIN_EXIT_MESSAGE
            .encode_utf16()
            .chain(Some(0))
            .collect();
        ensure!(
            // SAFETY: Both names are NUL-terminated and hwnd is this process's live window.
            unsafe {
                GetPropW(hwnd, property.as_ptr()).is_null()
                    && GetPropW(hwnd, exit_property.as_ptr()).is_null()
            },
            "主窗口激活入口已安装"
        );
        // SAFETY: RegisterWindowMessageW copies a valid NUL-terminated message name.
        let message = unsafe { RegisterWindowMessageW(name.as_ptr()) };
        ensure!(message != 0, "无法注册主窗口激活消息");
        // SAFETY: RegisterWindowMessageW copies a valid NUL-terminated message name.
        let exit_message = unsafe { RegisterWindowMessageW(exit_name.as_ptr()) };
        ensure!(exit_message != 0, "无法注册主窗口退出消息");
        let state = Rc::new(ActivationState {
            requested,
            exit_requested,
            context,
            message,
            exit_message,
            property,
            exit_property,
            exit_marked: Cell::new(false),
            attached: Cell::new(false),
        });
        // The subclass owns one explicit Rc reference, released exactly once on
        // removal or WM_NCDESTROY. The guard owns a separate reference.
        let reference = Rc::into_raw(state.clone()) as usize;
        // SAFETY: Installation runs on the owning UI thread; the retained Rc keeps
        // callback data valid through every callback and until successful removal.
        let installed = unsafe {
            SetWindowSubclass(
                hwnd,
                Some(activation_subclass),
                ACTIVATION_SUBCLASS_ID,
                reference,
            )
        };
        if installed == 0 {
            // SAFETY: Installation failed, so no callback owns the retained reference.
            unsafe { drop(Rc::from_raw(reference as *const ActivationState)) };
            bail!("无法安装主窗口激活入口");
        }
        state.attached.set(true);
        let guard = Self { hwnd, state };
        ensure!(
            // SAFETY: Only mark the owned HWND after its receiver is installed. The
            // property is an opaque identity; the other process never dereferences it.
            unsafe { SetPropW(hwnd, guard.state.property.as_ptr(), reference as _) != 0 },
            "无法标记主窗口激活入口"
        );
        ensure!(
            // SAFETY: Publish support only after the handler and main marker exist.
            // The value is an opaque scalar capability, never a dereferenced pointer.
            unsafe { SetPropW(hwnd, guard.state.exit_property.as_ptr(), 1usize as _) != 0 },
            "无法标记主窗口退出入口"
        );
        guard.state.exit_marked.set(true);
        Ok(guard)
    }
}

impl Drop for MainWindowActivation {
    fn drop(&mut self) {
        detach_activation(self.hwnd, &self.state, false);
    }
}

fn detach_activation(hwnd: HWND, state: &Rc<ActivationState>, destroying: bool) {
    if !state.attached.get() {
        return;
    }
    // SAFETY: The !Send guard and callback both run on the HWND-owning thread.
    // WM_NCDESTROY also guarantees there can be no later window callbacks.
    let removed = unsafe {
        RemoveWindowSubclass(hwnd, Some(activation_subclass), ACTIVATION_SUBCLASS_ID) != 0
    };
    if removed || destroying {
        state.attached.set(false);
        let reference = Rc::as_ptr(state);
        // SAFETY: Remove only our own marker and release the subclass's one retained
        // Rc. The caller's Rc keeps state alive throughout cleanup and any reentrancy.
        unsafe {
            if GetPropW(hwnd, state.property.as_ptr()) == reference.cast_mut().cast() {
                RemovePropW(hwnd, state.property.as_ptr());
            }
            if state.exit_marked.replace(false)
                && GetPropW(hwnd, state.exit_property.as_ptr()) == 1usize as _
            {
                RemovePropW(hwnd, state.exit_property.as_ptr());
            }
            drop(Rc::from_raw(reference));
        }
    }
    // If removal unexpectedly fails while the HWND is live, its retained Rc stays
    // alive until WM_NCDESTROY, so dropping the guard cannot leave dangling callback data.
}

unsafe extern "system" fn activation_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference: usize,
) -> LRESULT {
    let pointer = reference as *const ActivationState;
    // SAFETY: SetWindowSubclass received a retained Rc on this same thread. Take
    // a temporary callback reference so removal/reentrancy cannot invalidate state.
    let state = unsafe {
        Rc::increment_strong_count(pointer);
        Rc::from_raw(pointer)
    };
    if message == state.message || message == state.exit_message {
        let requested = if message == state.exit_message {
            &state.exit_requested
        } else {
            &state.requested
        };
        requested.store(true, Ordering::Release);
        // A repaint callback is external code; never unwind through Win32's ABI.
        let _ = catch_unwind(AssertUnwindSafe(|| state.context.request_repaint()));
        // SAFETY: This scalar wake targets only the HWND receiving our installed callback.
        unsafe { PostMessageW(hwnd, WM_PAINT, 0, 0) };
        return 0;
    }
    if message == WM_NCDESTROY {
        detach_activation(hwnd, &state, true);
    }
    // SAFETY: Forward untouched messages to the next subclass/original winit procedure.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

struct PhysicalPixels(DPI_AWARENESS_CONTEXT);
impl PhysicalPixels {
    fn enter() -> Self {
        // SAFETY: The predefined awareness constant is valid; retain the prior thread context.
        Self(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })
    }
}
impl Drop for PhysicalPixels {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: Restore the context returned when this guard entered on this same thread.
            unsafe { SetThreadDpiAwarenessContext(self.0) };
        }
    }
}

fn centered_origin(window: RECT, work: RECT) -> Option<(i32, i32)> {
    let width = i64::from(window.right) - i64::from(window.left);
    let height = i64::from(window.bottom) - i64::from(window.top);
    let work_width = i64::from(work.right) - i64::from(work.left);
    let work_height = i64::from(work.bottom) - i64::from(work.top);
    if width <= 0 || height <= 0 || work_width <= 0 || work_height <= 0 {
        return None;
    }
    // Keep the title bar reachable when the outer window exceeds a small work area.
    let x = i64::from(work.left) + (work_width - width).max(0) / 2;
    let y = i64::from(work.top) + (work_height - height).max(0) / 2;
    Some((i32::try_from(x).ok()?, i32::try_from(y).ok()?))
}

/// Position the newly created window before eframe displays its first frame.
/// Read both the full native frame and current monitor work area in physical pixels.
pub fn center_on_work_area(hwnd: usize) {
    let hwnd = hwnd as HWND;
    if !is_own_top_level_window(hwnd) {
        return;
    }
    let _dpi = PhysicalPixels::enter();
    // SAFETY: Both structures are POD; each API receives initialized writable storage.
    let (mut window, mut monitor): (RECT, MONITORINFO) = unsafe { (zeroed(), zeroed()) };
    monitor.cbSize = size_of::<MONITORINFO>() as u32;
    // SAFETY: The HWND was verified as this process's top-level window. The APIs
    // validate their handles and write only to the appropriately sized local structures.
    unsafe {
        if IsIconic(hwnd) != 0 || IsZoomed(hwnd) != 0 || GetWindowRect(hwnd, &mut window) == 0 {
            return;
        }
        let display = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if display.is_null() || GetMonitorInfoW(display, &mut monitor) == 0 {
            return;
        }
        if let Some((x, y)) = centered_origin(window, monitor.rcWork) {
            SetWindowPos(
                hwnd,
                null_mut(),
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOACTIVATE,
            );
        }
    }
}

/// Show a requested result or explicit activation, preserving the user's pinned state.
///
/// Call on the window-owning UI thread after eframe has applied its visibility and
/// minimized commands. Do not call for background/silent work or while capturing.
/// Normal windows are promoted only for this call, then returned to the front of
/// the non-topmost group. Windows can still reject keyboard foreground activation;
/// this does not bypass its foreground-lock policy or other apps' topmost windows.
/// See https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-setforegroundwindow.
pub fn bring_to_front(hwnd: usize, always_on_top: bool) {
    let hwnd = hwnd as HWND;
    if !is_own_top_level_window(hwnd) {
        return;
    }
    // SAFETY: The HWND was verified as a live top-level window in this process.
    // All operations below are synchronous on its owning UI thread: no delayed
    // restore/promotion can outlive this call and surface during a later capture.
    unsafe {
        if GetWindowThreadProcessId(hwnd, null_mut()) != GetCurrentThreadId() {
            return;
        }
        // SW_RESTORE on an already maximized window would unmaximize it. Restore
        // only minimized windows; SW_SHOW keeps a normal/maximized frame intact.
        ShowWindow(
            hwnd,
            if IsIconic(hwnd) != 0 {
                SW_RESTORE
            } else {
                SW_SHOW
            },
        );
        SetForegroundWindow(hwnd);

        // Separate the activation request from z-order. NOACTIVATE avoids a second
        // focus attempt, and NOMOVE/NOSIZE preserve physical/DPI-adjusted geometry.
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOOWNERZORDER | SWP_NOACTIVATE;
        SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, flags);
        if !always_on_top {
            // NOTOPMOST also places the temporarily promoted window ahead of normal
            // windows, without retaining WS_EX_TOPMOST after a result is displayed.
            SetWindowPos(hwnd, HWND_NOTOPMOST, 0, 0, 0, 0, flags);
        }
    }
}

/// Pause this window's animations before queuing any minimize/hide commands.
/// Move the single guard through the pending request into the capture thread; DWM
/// accepts these per-window calls across threads. The HWND is an opaque identity,
/// stored as usize so the guard can move without claiming ownership of the window.
pub struct CaptureTransitions(usize);
impl CaptureTransitions {
    pub fn disable(hwnd: usize) -> Result<Self> {
        let window = hwnd as HWND;
        ensure!(
            is_own_top_level_window(window),
            "截图准备失败：无法验证主窗口归属"
        );
        let disabled = 1_i32;
        // SAFETY: The HWND was verified as this process's top-level window. The attribute expects
        // a four-byte BOOL and copies it synchronously; no other window is affected.
        let result = unsafe {
            DwmSetWindowAttribute(
                window,
                DWMWA_TRANSITIONS_FORCEDISABLED as u32,
                std::ptr::from_ref(&disabled).cast(),
                size_of_val(&disabled) as u32,
            )
        };
        ensure!(
            result >= 0,
            "无法暂停主窗口动画，已停止截图（DWM 0x{result:08X}）"
        );
        Ok(Self(hwnd))
    }

    /// Consume on the capture thread after the UI frame committed its hide commands.
    /// Keep animations disabled through the native state checks and compositor flush;
    /// Drop restores them on success, preparation failure, or cancellation before launch.
    pub fn prepare(self) -> Result<()> {
        prepare_capture_window(self.0)
    }
}
impl Drop for CaptureTransitions {
    fn drop(&mut self) {
        let hwnd = self.0 as HWND;
        if is_own_top_level_window(hwnd) {
            // This module is the sole owner of the app's per-window transition override.
            // Return to the app's normal system animations on success and every error path.
            set_attribute(hwnd, DWMWA_TRANSITIONS_FORCEDISABLED as u32, &0_i32);
        }
    }
}

fn wait_for_window_state(
    hwnd: HWND,
    deadline: Instant,
    condition: impl Fn(HWND) -> bool,
) -> Result<()> {
    loop {
        ensure!(
            is_own_top_level_window(hwnd),
            "截图准备失败：主窗口已关闭或句柄已失效"
        );
        if condition(hwnd) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("主窗口未能及时最小化并隐藏，已停止截图以避免遮挡；请重试");
        }
        // Poll actual HWND state; this interval is not an assumed animation duration.
        thread::sleep(Duration::from_millis(5));
    }
}

/// Run on the capture thread before creating any selection overlay or freezing pixels.
/// The caller keeps transitions disabled while awaiting native state and synchronizing DWM.
/// An already hidden tray window keeps its normal/minimized state unchanged.
/// Success guarantees that the window is hidden, not that it is necessarily minimized.
/// No queued hide/minimize action remains necessary after this function succeeds.
/// See https://devblogs.microsoft.com/oldnewthing/20121003-00/?p=6423.
fn prepare_capture_window(hwnd: usize) -> Result<()> {
    let hwnd = hwnd as HWND;
    ensure!(
        is_own_top_level_window(hwnd),
        "截图准备失败：无法验证主窗口归属"
    );
    let deadline = Instant::now() + Duration::from_secs(3);

    // Hiding to tray clears WS_VISIBLE without setting WS_MINIMIZE. Minimizing
    // that already hidden window is unnecessary and can show it or race winit's
    // cached show state. Preserve it exactly and only synchronize the compositor.
    // SAFETY: The HWND was verified as this process's live top-level window.
    let initially_visible = unsafe { IsWindowVisible(hwnd) != 0 };
    if initially_visible {
        // SAFETY: The HWND belongs to this process and only scalar show commands cross threads.
        // ShowWindowAsync reports that the operation started; the state checks await completion.
        unsafe {
            if IsIconic(hwnd) == 0 && ShowWindowAsync(hwnd, SW_MINIMIZE) == 0 {
                bail!(
                    "无法最小化主窗口，已停止截图：{}",
                    std::io::Error::last_os_error()
                );
            }
        }
        wait_for_window_state(hwnd, deadline, |window| {
            // SAFETY: wait_for_window_state verifies ownership immediately before the query.
            unsafe { IsIconic(window) != 0 }
        })?;
        // Hide after observing minimization, so an asynchronous minimize cannot subsequently
        // show the taskbar thumbnail or steal activation from the new selection overlay.
        // SAFETY: The HWND is still owned, and both calls validate opaque handles.
        unsafe {
            if IsWindowVisible(hwnd) != 0 && ShowWindowAsync(hwnd, SW_HIDE) == 0 {
                bail!(
                    "无法隐藏主窗口，已停止截图：{}",
                    std::io::Error::last_os_error()
                );
            }
        }
        wait_for_window_state(hwnd, deadline, |window| {
            // SAFETY: wait_for_window_state verifies ownership immediately before the query.
            unsafe { IsWindowVisible(window) == 0 }
        })?;
    }

    // SAFETY: DwmFlush takes no arguments and waits for this app's pending composition.
    // Transitions remain disabled until after this flush, so animation ghosts cannot linger.
    let result = unsafe { DwmFlush() };
    ensure!(
        result >= 0,
        "桌面合成尚未就绪，已停止截图（DWM 0x{result:08X}）"
    );
    ensure!(is_own_top_level_window(hwnd), "截图准备失败：主窗口已关闭");
    ensure!(
        // SAFETY: The top-level HWND ownership was rechecked after the compositor wait.
        unsafe { IsWindowVisible(hwnd) == 0 },
        "截图前主窗口重新显示，已停止截图；请重试"
    );
    Ok(())
}

/// Compatibility entry for isolated tests that do not submit eframe viewport commands.
#[cfg(any(debug_assertions, test))]
pub fn prepare_capture(hwnd: usize) -> Result<()> {
    CaptureTransitions::disable(hwnd)?.prepare()
}

/// Apply the current app theme to this process's native window frame.
///
/// Call once after window creation and again when the effective theme changes.
/// The native caption, resize borders, system menu, and Snap behavior stay in charge.
pub fn apply(hwnd: usize, dark: bool) {
    let hwnd = hwnd as HWND;
    if !is_own_top_level_window(hwnd) {
        return;
    }

    let dark_mode = i32::from(dark); // Win32 BOOL is a four-byte integer.
    let corners = DWMWCP_ROUND;
    // COLORREF uses 0x00BBGGRR: the light caption matches the #F1F3F9 toolbar.
    let caption: u32 = if dark { 0x0020_2020 } else { 0x00F9_F3F1 };
    let text: u32 = if dark { 0x00FF_FFFF } else { 0x001A_1A1A };
    set_attribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE as u32, &dark_mode);
    set_attribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE as u32, &corners);
    set_attribute(hwnd, DWMWA_CAPTION_COLOR as u32, &caption);
    set_attribute(hwnd, DWMWA_TEXT_COLOR as u32, &text);
}

fn set_attribute<T>(hwnd: HWND, attribute: u32, value: &T) {
    // SAFETY: The caller verified window ownership. Each call above pairs the documented
    // attribute with its required BOOL, DWM_WINDOW_CORNER_PREFERENCE, or COLORREF type.
    // DWM reads exactly size_of_val(value) bytes synchronously; the reference stays live.
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            attribute,
            std::ptr::from_ref(value).cast(),
            size_of_val(value) as u32,
        );
    }
    // Older Windows versions can reject these attributes. Keep the normal native frame.
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn center_uses_outer_frame_and_work_area_offsets() {
        assert_eq!(
            centered_origin(rect(99, 88, 1199, 828), rect(0, 0, 1920, 1040)),
            Some((410, 150))
        );
        // A taskbar on the left/top and a negative-coordinate secondary display.
        assert_eq!(
            centered_origin(rect(0, 0, 1000, 700), rect(-1872, -1040, 0, 0)),
            Some((-1436, -870))
        );
        // Physical dimensions at 150% DPI are used as-is, without a second scale factor.
        assert_eq!(
            centered_origin(rect(0, 0, 1590, 1062), rect(1920, 40, 4480, 1400)),
            Some((2405, 189))
        );
    }

    #[test]
    fn center_keeps_oversized_window_title_bar_reachable() {
        assert_eq!(
            centered_origin(rect(0, 0, 1400, 900), rect(-1024, 30, 0, 768)),
            Some((-1024, 30))
        );
        assert!(centered_origin(rect(0, 0, 0, 100), rect(0, 0, 800, 600)).is_none());
        assert!(centered_origin(rect(0, 0, 100, 100), rect(0, 0, 800, 0)).is_none());
    }

    #[test]
    fn capture_preparation_rejects_invalid_window_without_side_effects() {
        assert!(prepare_capture(0).is_err());
        assert!(!is_visible(0));
        center_on_work_area(0);
    }

    #[test]
    fn bring_to_front_ignores_invalid_window() {
        bring_to_front(0, false);
        bring_to_front(0, true);
    }

    #[test]
    #[ignore = "requires an interactive Windows message queue; uses only an owned hidden window"]
    fn native_main_activation_keeps_hidden_and_releases_callback_state() -> Result<()> {
        use windows_sys::Win32::{
            System::LibraryLoader::GetModuleHandleW,
            UI::WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, DispatchMessageW, PeekMessageW, MSG, PM_REMOVE,
                WS_EX_TOOLWINDOW, WS_POPUP,
            },
        };
        struct TestWindow(HWND);
        impl Drop for TestWindow {
            fn drop(&mut self) {
                if !self.0.is_null() {
                    // SAFETY: This hidden window is created/destroyed on this test thread.
                    unsafe { DestroyWindow(self.0) };
                }
            }
        }
        fn deliver(hwnd: HWND, message: u32) {
            // SAFETY: Post and dispatch scalar messages only for the test's live HWND.
            unsafe {
                assert_ne!(PostMessageW(hwnd, message, 0, 0), 0);
                let mut pending: MSG = zeroed();
                while PeekMessageW(&mut pending, hwnd, 0, 0, PM_REMOVE) != 0 {
                    DispatchMessageW(&pending);
                }
            }
        }
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        // SAFETY: STATIC is a predefined class; valid pointers live through the call.
        // No WS_VISIBLE flag is set, so this test never shows or captures a window.
        let mut window = TestWindow(unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class.as_ptr(),
                std::ptr::null(),
                WS_POPUP,
                0,
                0,
                32,
                32,
                null_mut(),
                null_mut(),
                GetModuleHandleW(std::ptr::null()),
                null_mut(),
            )
        });
        ensure!(!window.0.is_null(), "无法创建隐藏的激活测试窗口");
        let requested = Arc::new(AtomicBool::new(false));
        let exit_requested = Arc::new(AtomicBool::new(false));
        let context = eframe::egui::Context::default();
        let (handle, other_flag, other_exit_flag, other_context) = (
            window.0 as usize,
            requested.clone(),
            exit_requested.clone(),
            context.clone(),
        );
        assert!(thread::spawn(move || {
            MainWindowActivation::install(handle, other_flag, other_exit_flag, other_context)
                .is_err()
        })
        .join()
        .expect("cross-thread installation panicked"));

        let guard = MainWindowActivation::install(
            handle,
            requested.clone(),
            exit_requested.clone(),
            context.clone(),
        )?;
        let message = guard.state.message;
        let exit_message = guard.state.exit_message;
        let property = guard.state.property.clone();
        let exit_property = guard.state.exit_property.clone();
        assert!(MainWindowActivation::install(
            handle,
            requested.clone(),
            exit_requested.clone(),
            context.clone(),
        )
        .is_err());
        assert_ne!(message, exit_message);
        deliver(window.0, message);
        assert!(requested.swap(false, Ordering::AcqRel));
        assert!(!exit_requested.load(Ordering::Acquire));
        deliver(window.0, exit_message);
        assert!(exit_requested.swap(false, Ordering::AcqRel));
        assert!(!requested.load(Ordering::Acquire));
        assert!(
            // SAFETY: These queries target only the guarded hidden test window.
            unsafe {
                IsWindowVisible(window.0) == 0
                    && !GetPropW(window.0, property.as_ptr()).is_null()
                    && GetPropW(window.0, exit_property.as_ptr()) == 1usize as _
            }
        );
        drop(guard);
        assert_eq!(
            Arc::strong_count(&requested),
            1,
            "guard removal leaked state"
        );
        assert_eq!(Arc::strong_count(&exit_requested), 1, "exit state leaked");
        assert!(
            // SAFETY: The owned HWND and both NUL-terminated properties are still valid.
            unsafe {
                GetPropW(window.0, property.as_ptr()).is_null()
                    && GetPropW(window.0, exit_property.as_ptr()).is_null()
            }
        );
        deliver(window.0, message);
        deliver(window.0, exit_message);
        assert!(
            !requested.load(Ordering::Acquire) && !exit_requested.load(Ordering::Acquire),
            "removed callback still ran"
        );

        let guard = MainWindowActivation::install(
            handle,
            requested.clone(),
            exit_requested.clone(),
            context,
        )?;
        let destroyed = std::mem::replace(&mut window.0, null_mut());
        // SAFETY: Destroy exactly once on the owning thread while the guard is still live.
        assert_ne!(unsafe { DestroyWindow(destroyed) }, 0);
        assert!(!guard.state.attached.get(), "WM_NCDESTROY did not detach");
        assert!(
            !guard.state.exit_marked.get(),
            "WM_NCDESTROY did not remove exit capability"
        );
        drop(guard);
        assert_eq!(
            Arc::strong_count(&requested),
            1,
            "window destruction leaked state"
        );
        assert_eq!(Arc::strong_count(&exit_requested), 1, "exit state leaked");
        Ok(())
    }

    #[test]
    #[ignore = "briefly shows owned test windows; run serially in an interactive Windows session"]
    fn native_bring_to_front_restores_geometry_and_window_level() -> Result<()> {
        use windows_sys::Win32::{
            System::LibraryLoader::GetModuleHandleW,
            UI::WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, GetWindow, GetWindowLongW, GWL_EXSTYLE,
                GW_HWNDNEXT, SW_MAXIMIZE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_OVERLAPPEDWINDOW,
            },
        };
        struct TestWindow(HWND);
        impl TestWindow {
            fn new(x: i32) -> Result<Self> {
                let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
                // SAFETY: STATIC is a system class and the pointers live through the call.
                // Create a hidden, unowned top-level window in this test process/thread.
                let hwnd = unsafe {
                    CreateWindowExW(
                        WS_EX_TOOLWINDOW,
                        class.as_ptr(),
                        std::ptr::null(),
                        WS_OVERLAPPEDWINDOW,
                        x,
                        90,
                        320,
                        240,
                        null_mut(),
                        null_mut(),
                        GetModuleHandleW(std::ptr::null()),
                        null_mut(),
                    )
                };
                ensure!(!hwnd.is_null(), "无法创建窗口置前测试窗口");
                Ok(Self(hwnd))
            }
            fn bounds(&self) -> (i32, i32, i32, i32) {
                // SAFETY: RECT is POD and GetWindowRect writes this local structure.
                let mut bounds: RECT = unsafe { zeroed() };
                // SAFETY: This guard owns a live HWND and the output has the correct size.
                assert_ne!(unsafe { GetWindowRect(self.0, &mut bounds) }, 0);
                (bounds.left, bounds.top, bounds.right, bounds.bottom)
            }
            fn pinned(&self) -> bool {
                // SAFETY: The guard keeps the test HWND live; the style query is read-only.
                unsafe { GetWindowLongW(self.0, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST != 0 }
            }
            fn above(&self, other: &Self) -> bool {
                let mut next = self.0;
                // Bound traversal because unrelated windows may change concurrently.
                // Only inspect the order; never modify or activate another app's HWND.
                for _ in 0..4096 {
                    // SAFETY: GetWindow validates each opaque handle before reading order.
                    next = unsafe { GetWindow(next, GW_HWNDNEXT) };
                    if next == other.0 {
                        return true;
                    }
                    if next.is_null() {
                        return false;
                    }
                }
                false
            }
        }
        impl Drop for TestWindow {
            fn drop(&mut self) {
                // SAFETY: The HWND was created on this thread and has no other owner.
                unsafe { DestroyWindow(self.0) };
            }
        }

        let target = TestWindow::new(120)?;
        let peer = TestWindow::new(160)?;
        let _target_transitions = CaptureTransitions::disable(target.0 as usize)?;
        let _peer_transitions = CaptureTransitions::disable(peer.0 as usize)?;
        let normal_bounds = target.bounds();
        let target_handle = target.0 as usize;
        thread::spawn(move || bring_to_front(target_handle, true))
            .join()
            .expect("cross-thread validation panicked");
        assert!(
            // SAFETY: The guard owns this live test window; the worker has completed.
            unsafe { IsWindowVisible(target.0) == 0 }
        );
        assert!(
            !target.pinned(),
            "wrong-thread call queued a later promotion"
        );
        bring_to_front(peer.0 as usize, false);
        bring_to_front(target.0 as usize, false);
        assert!(is_visible(target.0 as usize));
        assert!(
            // SAFETY: The guard owns this live test window.
            unsafe { IsWindowVisible(target.0) != 0 && IsIconic(target.0) == 0 }
        );
        assert!(!target.pinned());
        assert!(
            target.above(&peer),
            "requested result stayed behind a normal window"
        );
        assert_eq!(target.bounds(), normal_bounds);

        bring_to_front(peer.0 as usize, true);
        bring_to_front(target.0 as usize, true);
        assert!(target.pinned() && peer.pinned());
        assert!(target.above(&peer));
        bring_to_front(target.0 as usize, false);
        assert!(
            !target.pinned(),
            "temporary promotion left the result always on top"
        );
        assert!(peer.pinned() && peer.above(&target));
        assert_eq!(target.bounds(), normal_bounds);
        bring_to_front(peer.0 as usize, false);

        // This is the actual visible-capture preparation shape: minimized, then hidden.
        // SAFETY: Show commands affect only this test's own live window on its UI thread.
        unsafe {
            ShowWindow(target.0, SW_MINIMIZE);
            ShowWindow(target.0, SW_HIDE);
        }
        assert!(!is_visible(target.0 as usize));
        bring_to_front(target.0 as usize, false);
        assert!(
            // SAFETY: The guard owns this live test window.
            unsafe { IsWindowVisible(target.0) != 0 && IsIconic(target.0) == 0 }
        );
        assert!(!target.pinned() && target.above(&peer));
        assert_eq!(target.bounds(), normal_bounds);

        // A result arriving while maximized must not restore a smaller normal frame.
        // SAFETY: These commands target only the guarded test window.
        unsafe {
            ShowWindow(target.0, SW_MAXIMIZE);
            ShowWindow(target.0, SW_HIDE);
        }
        let maximized_bounds = target.bounds();
        bring_to_front(target.0 as usize, false);
        assert!(
            // SAFETY: The guard owns this live test window.
            unsafe { IsZoomed(target.0) != 0 }
        );
        assert_eq!(target.bounds(), maximized_bounds);
        assert!(!target.pinned());
        // Foreground keyboard ownership is deliberately not asserted: Windows can
        // deny it even when z-order, restoration and the persistent pin are correct.
        Ok(())
    }

    #[test]
    fn native_capture_preparation_keeps_hidden_window_hidden() -> Result<()> {
        assert_hidden_preparation_preserves_state(true)
    }

    #[test]
    fn native_capture_preparation_preserves_tray_hidden_normal_window() -> Result<()> {
        // Closing the real egui window to tray hides it without minimizing it.
        assert_hidden_preparation_preserves_state(false)
    }

    fn assert_hidden_preparation_preserves_state(minimized: bool) -> Result<()> {
        use windows_sys::Win32::{
            System::LibraryLoader::GetModuleHandleW,
            UI::WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, DispatchMessageW, PeekMessageW, MSG, PM_REMOVE,
                WS_EX_TOOLWINDOW, WS_MINIMIZE, WS_POPUP,
            },
        };
        struct TestWindow(HWND);
        impl Drop for TestWindow {
            fn drop(&mut self) {
                // SAFETY: This unshown test window was created and is destroyed on this thread.
                unsafe { DestroyWindow(self.0) };
            }
        }
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        // Create hidden, either normal or minimized: never show a window, capture desktop
        // pixels, inject input, or touch the user's application during this test.
        // SAFETY: STATIC is a predefined class; pointers remain valid for this call.
        // The resulting top-level HWND belongs to this test process and thread.
        let window = TestWindow(unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class.as_ptr(),
                std::ptr::null(),
                WS_POPUP | if minimized { WS_MINIMIZE } else { 0 },
                120,
                90,
                96,
                64,
                null_mut(),
                null_mut(),
                GetModuleHandleW(std::ptr::null()),
                null_mut(),
            )
        });
        ensure!(!window.0.is_null(), "无法创建隐藏的截图准备测试窗口");
        assert!(!is_visible(window.0 as usize));
        // SAFETY: The guarded HWND is live and belongs only to this test.
        assert!(unsafe {
            (IsIconic(window.0) != 0) == minimized && IsWindowVisible(window.0) == 0
        });
        // SAFETY: RECT is POD and GetWindowRect initializes the writable output storage.
        let mut before: RECT = unsafe { zeroed() };
        // SAFETY: The test's guarded HWND is live and before is a correctly sized RECT.
        assert_ne!(unsafe { GetWindowRect(window.0, &mut before) }, 0);
        for _ in 0..2 {
            prepare_capture(window.0 as usize)?;
            assert!(!is_visible(window.0 as usize));
            // Deliver any pending messages for this test window only, checking that
            // preparation leaves no queued show/minimize transition behind, even on reuse.
            // SAFETY: MSG is POD; PeekMessage initializes it before DispatchMessage uses it.
            let mut message: MSG = unsafe { zeroed() };
            // SAFETY: The HWND filter restricts dispatch to the test's own window.
            unsafe {
                while PeekMessageW(&mut message, window.0, 0, 0, PM_REMOVE) != 0 {
                    DispatchMessageW(&message);
                }
            }
            assert!(
                // SAFETY: The guarded HWND remains live and belongs only to this test.
                unsafe { (IsIconic(window.0) != 0) == minimized && IsWindowVisible(window.0) == 0 },
                "preparation changed the hidden window's normal/minimized state"
            );
            // SAFETY: RECT is POD, and GetWindowRect initializes this local output.
            let mut after: RECT = unsafe { zeroed() };
            // SAFETY: The same owned HWND is live and after is a correctly sized RECT.
            assert_ne!(unsafe { GetWindowRect(window.0, &mut after) }, 0);
            assert_eq!(
                (before.left, before.top, before.right, before.bottom),
                (after.left, after.top, after.right, after.bottom),
                "preparation moved or resized an already hidden window"
            );
        }
        Ok(())
    }
}
