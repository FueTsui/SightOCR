//! Keep egui's host IME policy compatible with focused native text children.

use eframe::egui::{self, Pos2, Rect, Vec2};
use std::{mem::size_of, ptr::null_mut};
use windows_sys::Win32::{
    Foundation::{HWND, POINT, RECT},
    Graphics::Gdi::{ClientToScreen, ScreenToClient},
    System::Threading::GetCurrentThreadId,
    UI::{
        Input::{
            Ime::{ImmAssociateContextEx, ImmGetContext, ImmReleaseContext, IACE_DEFAULT},
            KeyboardAndMouse::GetFocus,
        },
        WindowsAndMessaging::{GetCaretPos, GetGUIThreadInfo, IsChild, IsWindow, GUITHREADINFO},
    },
};

/// Restore a native child's default context only if the host disconnected it.
///
/// Call on the owning UI thread, before forwarding `WM_SETFOCUS` to RichEdit.
/// Existing composition, open status, and keyboard layout are left untouched.
/// Returns whether a context was already present or restoration succeeded.
pub(super) fn restore_context_if_missing(hwnd: HWND) -> bool {
    // SAFETY: The caller supplies its own live native child on the UI thread.
    // Handles are checked before querying; every acquired context is released.
    unsafe {
        if hwnd.is_null() || IsWindow(hwnd) == 0 {
            return false;
        }
        let context = ImmGetContext(hwnd);
        if !context.is_null() {
            ImmReleaseContext(hwnd, context);
            return true;
        }
        ImmAssociateContextEx(hwnd, null_mut(), IACE_DEFAULT) != 0
    }
}

/// Publish the focused native caret in the parent viewport's logical units.
///
/// Call after native placement and popup/modal focus handling each egui frame.
/// egui-winit 0.31 otherwise treats a native editor as an absent text edit and
/// winit's Windows disable path disconnects the host's child IME contexts.
/// Returns false without modifying output when this child does not own focus.
pub(super) fn publish_ime_output(ctx: &egui::Context, parent: HWND, child: HWND) -> bool {
    // SAFETY: The caller owns both windows on this UI thread. Only window and
    // focus state is read; no activation or focus changes are requested here.
    if unsafe {
        parent.is_null()
            || child.is_null()
            || GetFocus() != child
            || IsWindow(child) == 0
            || IsChild(parent, child) == 0
    } {
        return false;
    }

    let caret = native_caret_rect(child);
    let mut top_left = POINT {
        x: caret.left,
        y: caret.top,
    };
    // SAFETY: Both handles refer to our own existing windows. The point is a
    // writable local initialized in the child's client coordinate system.
    let mapped = unsafe {
        ClientToScreen(child, &mut top_left) != 0 && ScreenToClient(parent, &mut top_left) != 0
    };
    if !mapped {
        // A transient caret/window transition must not emit ime=None and tear
        // down composition. A later frame will publish the actual position.
        top_left = POINT { x: 0, y: 0 };
    }
    let pixels_per_point = ctx.pixels_per_point();
    let rect = Rect::from_min_size(
        Pos2::new(
            top_left.x as f32 / pixels_per_point,
            top_left.y as f32 / pixels_per_point,
        ),
        Vec2::new(
            (caret.right - caret.left).max(1) as f32 / pixels_per_point,
            (caret.bottom - caret.top).max(1) as f32 / pixels_per_point,
        ),
    );
    ctx.output_mut(|output| {
        // This backend uses rect, rather than cursor_rect, for candidate
        // positioning. Use the same native caret rectangle for both fields.
        output.ime = Some(egui::output::IMEOutput {
            rect,
            cursor_rect: rect,
        });
    });
    true
}

fn native_caret_rect(child: HWND) -> RECT {
    let mut info = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        flags: 0,
        hwndActive: null_mut(),
        hwndFocus: null_mut(),
        hwndCapture: null_mut(),
        hwndMenuOwner: null_mut(),
        hwndMoveSize: null_mut(),
        hwndCaret: null_mut(),
        rcCaret: RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        },
    };
    // SAFETY: Explicitly query the calling UI thread, not the foreground thread
    // (which zero would select). info is sized and writable; check caret owner.
    if unsafe { GetGUIThreadInfo(GetCurrentThreadId(), &mut info) != 0 && info.hwndCaret == child }
    {
        return info.rcCaret;
    }
    let mut point = POINT { x: 0, y: 0 };
    // SAFETY: publish_ime_output verified this child owns the thread's focus.
    // GetCaretPos writes only to the initialized local point.
    if unsafe { GetCaretPos(&mut point) } == 0 {
        point = POINT { x: 0, y: 0 };
    }
    RECT {
        left: point.x,
        top: point.y,
        right: point.x.saturating_add(1),
        bottom: point.y.saturating_add(1),
    }
}
