//! Unicode result editors backed by the Windows text engine.
//!
//! egui 0.31 lays out one glyph per Unicode scalar, without script shaping or
//! bidi cursor mapping. RichEdit supplies Uniscribe shaping, font binding, UBA,
//! selection, undo, and IME for the original logical-order Unicode text.
//! https://learn.microsoft.com/windows/win32/controls/about-rich-edit-controls

use super::theme::Palette;
#[path = "native_font_runs.rs"]
mod native_font_runs;
#[path = "native_ime.rs"]
mod native_ime;
use anyhow::{ensure, Result};
#[cfg(any(debug_assertions, test))]
use eframe::egui::ColorImage;
use eframe::egui::{self, Color32, Rect, Response, Sense, Vec2};
#[cfg(any(debug_assertions, test))]
use std::cell::RefCell;
use std::{
    cell::Cell,
    ffi::c_void,
    mem::{size_of, zeroed},
    panic::{catch_unwind, AssertUnwindSafe},
    ptr::{null, null_mut},
    rc::Rc,
};
use windows_sys::core::GUID;
#[cfg(any(debug_assertions, test))]
use windows_sys::Win32::Graphics::Gdi::RDW_ERASE;
#[cfg(test)]
use windows_sys::Win32::UI::WindowsAndMessaging::WM_SETTEXT;
use windows_sys::Win32::{
    Foundation::{FreeLibrary, HMODULE, HWND, LPARAM, LRESULT, POINT, WPARAM},
    Globalization::{GetStringTypeW, C2_LEFTTORIGHT, C2_RIGHTTOLEFT, CT_CTYPE2},
    Graphics::Gdi::{
        CombineRgn, CreateRectRgn, DeleteObject, RedrawWindow, SetWindowRgn, NULLREGION,
        RDW_INVALIDATE, RDW_NOERASE, RDW_UPDATENOW, RGN_DIFF,
    },
    System::{
        Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED},
        LibraryLoader::{LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32},
        Threading::GetCurrentThreadId,
    },
    UI::{
        HiDpi::GetDpiForWindow,
        Input::KeyboardAndMouse::{GetFocus, SetFocus},
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, GetWindowLongW, GetWindowThreadProcessId, IsChild,
            IsWindow, IsWindowVisible, PostMessageW, SendMessageW, SetWindowLongW, SetWindowPos,
            ShowWindow, WindowFromPoint, ES_AUTOVSCROLL, ES_MULTILINE, ES_NOHIDESEL, ES_WANTRETURN,
            GWL_STYLE, HWND_TOP, SWP_NOACTIVATE, SWP_NOZORDER, SWP_SHOWWINDOW, SW_HIDE, WM_CHAR,
            WM_CLEAR, WM_CUT, WM_HSCROLL, WM_IME_COMPOSITION, WM_IME_ENDCOMPOSITION,
            WM_IME_STARTCOMPOSITION, WM_KEYDOWN, WM_KEYUP, WM_KILLFOCUS, WM_LBUTTONUP,
            WM_MOUSEHWHEEL, WM_MOUSEWHEEL, WM_NCDESTROY, WM_NOTIFY, WM_PAINT, WM_PASTE,
            WM_SETFOCUS, WM_UNDO, WM_USER, WM_VSCROLL, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS,
            WS_TABSTOP, WS_VSCROLL,
        },
    },
};
#[cfg(any(debug_assertions, test))]
use windows_sys::Win32::{
    Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, GdiFlush, RestoreDC, SaveDC, SelectObject,
        BITMAPINFO, BI_RGB, DIB_RGB_COLORS,
    },
    Storage::Xps::{PrintWindow, PW_CLIENTONLY},
    UI::WindowsAndMessaging::{GetClientRect, PRF_CLIENT, PRF_ERASEBKGND, WM_PRINTCLIENT},
};

// windows-sys 0.59 does not expose Richedit.h. These messages and repr(C)
// structures follow the Windows SDK; all text APIs explicitly use UTF-16.
const EM_GETMODIFY: u32 = 0x00B8;
const EM_SETMODIFY: u32 = 0x00B9;
const EM_SETREADONLY: u32 = 0x00CF;
const EM_EXLIMITTEXT: u32 = WM_USER + 53;
const EM_SETBKGNDCOLOR: u32 = WM_USER + 67;
const EM_SETCHARFORMAT: u32 = WM_USER + 68;
const EM_SETPARAFORMAT: u32 = WM_USER + 71;
const EM_SETTEXTMODE: u32 = WM_USER + 89;
const EM_GETTEXTEX: u32 = WM_USER + 94;
const EM_GETTEXTLENGTHEX: u32 = WM_USER + 95;
const EM_SETLANGOPTIONS: u32 = WM_USER + 120;
const EM_GETLANGOPTIONS: u32 = WM_USER + 121;
const EM_SETBIDIOPTIONS: u32 = WM_USER + 200;
const EM_SETTYPOGRAPHYOPTIONS: u32 = WM_USER + 202;
const EM_SETEDITSTYLE: u32 = WM_USER + 204;
const SCF_ALL: usize = 4;
const CFM_SIZE: u32 = 0x8000_0000;
const CFM_COLOR: u32 = 0x4000_0000;
const CFM_FACE: u32 = 0x2000_0000;
const SUBCLASS_ID: usize = 0x534F_5458;
const WHEEL_SUBCLASS_ID: usize = 0x534F_5748;
const REDRAW_MESSAGE: u32 = 0x8000 + 0x533;
const IME_NOTIFICATION_MESSAGE: u32 = 0x8000 + 0x535;
const EN_STARTCOMPOSITION: u32 = 0x0713;
const EN_ENDCOMPOSITION: u32 = 0x0714;

#[repr(C)]
#[derive(Clone, Copy)]
struct NotifyHeader {
    hwnd_from: HWND,
    id_from: usize,
    code: u32,
}

#[repr(C, packed(4))]
struct EndCompositionNotify {
    header: NotifyHeader,
    code: u32,
}
#[cfg(any(debug_assertions, test))]
const SNAPSHOT_MESSAGE: u32 = 0x8000 + 0x534;

#[repr(C)]
struct GetTextLengthEx {
    flags: u32,
    codepage: u32,
}

#[repr(C, packed(4))]
struct GetTextEx {
    cb: u32,
    flags: u32,
    codepage: u32,
    default_char: *const u8,
    used_default_char: *mut i32,
}

#[repr(C)]
struct CharFormatW {
    size: u32,
    mask: u32,
    effects: u32,
    height: i32,
    offset: i32,
    text_color: u32,
    charset: u8,
    pitch_and_family: u8,
    face: [u16; 32],
}

#[repr(C)]
struct BidiOptions {
    size: u32,
    mask: u16,
    effects: u16,
}

#[repr(C)]
struct ParaFormat {
    size: u32,
    mask: u32,
    numbering: u16,
    effects: u16,
    start_indent: i32,
    right_indent: i32,
    offset: i32,
    alignment: u16,
    tab_count: i16,
    tabs: [i32; 32],
}

#[repr(C, packed(4))]
struct EditStream {
    cookie: usize,
    error: u32,
    callback: unsafe extern "system" fn(usize, *mut u8, i32, *mut i32) -> u32,
}

struct TextStream<'a> {
    bytes: &'a [u8],
    offset: usize,
}

#[repr(C)]
struct DocumentVtable {
    query: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    // IDispatch's four methods followed by ITextDocument GetName through Save.
    unused: [usize; 15],
    freeze: unsafe extern "system" fn(*mut c_void, *mut i32) -> i32,
    unfreeze: unsafe extern "system" fn(*mut c_void, *mut i32) -> i32,
    begin_collection: usize,
    end_collection: usize,
    undo: unsafe extern "system" fn(*mut c_void, i32, *mut i32) -> i32,
}

/// TOM's supported Freeze/Undo(tomSuspend) operations batch layout while keeping
/// application font/direction formatting outside the user's undo history.
struct FormatGuard {
    hwnd: HWND,
    document: *mut c_void,
    frozen: bool,
    suspended: bool,
    selection: [i32; 2],
    scroll: windows_sys::Win32::Foundation::POINT,
    modified: isize,
    freeze_count: i32,
}

impl FormatGuard {
    fn new(hwnd: HWND) -> Self {
        let mut guard = Self {
            hwnd,
            document: null_mut(),
            frozen: false,
            suspended: false,
            selection: [0; 2],
            scroll: windows_sys::Win32::Foundation::POINT { x: 0, y: 0 },
            modified: 0,
            freeze_count: 0,
        };
        let iid = GUID {
            data1: 0x8CC497C0,
            data2: 0xA1DF,
            data3: 0x11CE,
            data4: [0x80, 0x98, 0x00, 0xAA, 0x00, 0x47, 0xBE, 0x5D],
        };
        let mut ole: *mut c_void = null_mut();
        // SAFETY: These are read-only control messages with correctly sized local
        // storage. EM_GETOLEINTERFACE adds one owned IUnknown reference.
        unsafe {
            guard.modified = SendMessageW(hwnd, EM_GETMODIFY, 0, 0);
            SendMessageW(hwnd, WM_USER + 52, 0, guard.selection.as_mut_ptr() as isize);
            SendMessageW(hwnd, WM_USER + 221, 0, &mut guard.scroll as *mut _ as isize);
            SendMessageW(hwnd, WM_USER + 60, 0, &mut ole as *mut _ as isize);
            if !ole.is_null() {
                let table = *(ole as *const *const DocumentVtable);
                ((*table).query)(ole, &iid, &mut guard.document);
                ((*table).release)(ole);
            }
            if !guard.document.is_null() {
                let table = *(guard.document as *const *const DocumentVtable);
                let mut count = 0;
                guard.frozen = ((*table).freeze)(guard.document, &mut count) >= 0;
                guard.freeze_count = count;
                guard.suspended = ((*table).undo)(guard.document, -9_999_995, &mut count) >= 0;
            }
        }
        guard
    }
}

impl Drop for FormatGuard {
    fn drop(&mut self) {
        // SAFETY: Restore only this child's state before releasing its retained
        // TOM interface; selection/scroll structures match Richedit.h layouts.
        unsafe {
            SendMessageW(self.hwnd, WM_USER + 55, 0, self.selection.as_ptr() as isize);
            SendMessageW(
                self.hwnd,
                WM_USER + 222,
                0,
                &self.scroll as *const _ as isize,
            );
            SendMessageW(self.hwnd, EM_SETMODIFY, self.modified as usize, 0);
            if !self.document.is_null() {
                let table = *(self.document as *const *const DocumentVtable);
                let mut count = 0;
                if self.suspended {
                    ((*table).undo)(self.document, -9_999_994, &mut count);
                }
                if self.frozen {
                    ((*table).unfreeze)(self.document, &mut count);
                }
                ((*table).release)(self.document);
            }
        }
    }
}

unsafe extern "system" fn stream_text(
    cookie: usize,
    destination: *mut u8,
    capacity: i32,
    written: *mut i32,
) -> u32 {
    // SAFETY: EM_STREAMIN synchronously calls this callback with the stack-owned
    // TextStream cookie and writable system buffer of the stated capacity.
    let input = unsafe { &mut *(cookie as *mut TextStream<'_>) };
    let count = input
        .bytes
        .len()
        .saturating_sub(input.offset)
        .min(capacity.max(0) as usize);
    // SAFETY: count is bounded by both the source slice and system destination.
    unsafe {
        std::ptr::copy_nonoverlapping(input.bytes.as_ptr().add(input.offset), destination, count);
        *written = count as i32;
    }
    input.offset += count;
    0
}

struct WheelRouter {
    parent: HWND,
    children: Cell<[HWND; 2]>,
    dispatching: Cell<bool>,
    attached: Cell<bool>,
}

impl WheelRouter {
    fn install(self: &Rc<Self>) -> Result<()> {
        let reference = Rc::into_raw(self.clone()) as usize;
        // SAFETY: The owned Rc keeps this UI-thread-only parent callback alive.
        if unsafe {
            SetWindowSubclass(
                self.parent,
                Some(wheel_parent_subclass),
                WHEEL_SUBCLASS_ID,
                reference,
            )
        } == 0
        {
            // SAFETY: Installation failed, so no callback owns this retained reference.
            unsafe {
                drop(Rc::from_raw(reference as *const Self));
            }
            anyhow::bail!("无法连接结果编辑器的鼠标滚轮");
        }
        self.attached.set(true);
        Ok(())
    }

    fn uninstall(&self) {
        if self.attached.replace(false) {
            // SAFETY: Remove exactly our parent subclass and release its one Rc.
            unsafe {
                RemoveWindowSubclass(self.parent, Some(wheel_parent_subclass), WHEEL_SUBCLASS_ID);
                drop(Rc::from_raw(self as *const Self));
            }
        }
    }

    fn target_at(&self, lparam: LPARAM) -> Option<HWND> {
        // WindowFromPoint respects visibility, z-order and SetWindowRgn popup holes.
        // Only return one of our own two controls, never another application's HWND.
        // SAFETY: These APIs only query windows at the signed screen coordinates.
        unsafe {
            let hit = WindowFromPoint(wheel_screen_point(lparam));
            self.children.get().into_iter().find(|child| {
                !child.is_null()
                    && IsWindowVisible(*child) != 0
                    && (hit == *child || IsChild(*child, hit) != 0)
            })
        }
    }

    fn forward(
        &self,
        origin: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Option<LRESULT> {
        if self.dispatching.get() {
            return None;
        }
        let target = self.target_at(lparam).unwrap_or(self.parent);
        if target == origin {
            return None;
        }
        let _guard = WheelDispatchGuard::new(&self.dispatching);
        // SAFETY: Preserve delta/modifiers/screen coordinates and synchronously
        // deliver once to our hovered editor, or back to our own egui parent.
        Some(unsafe { SendMessageW(target, message, wparam, lparam) })
    }
}

struct WheelDispatchGuard<'a> {
    flag: &'a Cell<bool>,
    previous: bool,
}
impl<'a> WheelDispatchGuard<'a> {
    fn new(flag: &'a Cell<bool>) -> Self {
        Self {
            flag,
            previous: flag.replace(true),
        }
    }
}
impl Drop for WheelDispatchGuard<'_> {
    fn drop(&mut self) {
        self.flag.set(self.previous);
    }
}

fn wheel_screen_point(lparam: LPARAM) -> POINT {
    POINT {
        x: (lparam as u16 as i16) as i32,
        y: ((lparam as usize >> 16) as u16 as i16) as i32,
    }
}

unsafe extern "system" fn wheel_parent_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    reference: usize,
) -> LRESULT {
    // SAFETY: Installed parent callback owns a reference; retain one across nested dispatch/destruction.
    let router = unsafe {
        Rc::increment_strong_count(reference as *const WheelRouter);
        Rc::from_raw(reference as *const WheelRouter)
    };
    if message == WM_NCDESTROY {
        router.uninstall();
    }
    if message == WM_NOTIFY && lparam != 0 {
        // SAFETY: WM_NOTIFY supplies a synchronous NMHDR pointer. Only forward
        // composition notifications from the two children registered with us.
        // TSF-enabled RichEdit need not send the legacy WM_IME_* messages.
        unsafe {
            let header = std::ptr::read_unaligned(lparam as *const NotifyHeader);
            if router.children.get().contains(&header.hwnd_from)
                && !header.hwnd_from.is_null()
                && matches!(header.code, EN_STARTCOMPOSITION | EN_ENDCOMPOSITION)
            {
                let detail = if header.code == EN_ENDCOMPOSITION {
                    std::ptr::addr_of!((*(lparam as *const EndCompositionNotify)).code)
                        .read_unaligned() as isize
                } else {
                    0
                };
                SendMessageW(
                    header.hwnd_from,
                    IME_NOTIFICATION_MESSAGE,
                    header.code as usize,
                    detail,
                );
            }
        }
    }
    if matches!(message, WM_MOUSEWHEEL | WM_MOUSEHWHEEL) {
        if let Some(result) = router.forward(hwnd, message, wparam, lparam) {
            return result;
        }
    }
    // SAFETY: Every unhandled event continues through this same window's chain.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

struct EditorWake {
    ctx: egui::Context,
    parent: HWND,
    wheel_router: Rc<WheelRouter>,
    attached: Cell<bool>,
    text_event: Cell<bool>,
    ime_composing: Cell<bool>,
    #[cfg(any(debug_assertions, test))]
    format_passes: Cell<u64>,
    #[cfg(any(debug_assertions, test))]
    ime_starts: Cell<u64>,
    #[cfg(any(debug_assertions, test))]
    ime_ends: Cell<u64>,
    redraw_pending: Cell<bool>,
    redraw_dirty: Cell<bool>,
    last_visible: Cell<bool>,
    #[cfg(any(debug_assertions, test))]
    redraw_requests: Cell<u64>,
    #[cfg(any(debug_assertions, test))]
    wheel_deliveries: Cell<u64>,
    #[cfg(any(debug_assertions, test))]
    snapshot_generation: Cell<u64>,
    #[cfg(any(debug_assertions, test))]
    snapshot_pending: Cell<Option<SnapshotKey>>,
    #[cfg(any(debug_assertions, test))]
    snapshot: RefCell<Option<(SnapshotKey, Option<Vec<Color32>>)>>,
}

#[cfg(any(debug_assertions, test))]
#[derive(Clone, Copy, PartialEq, Eq)]
struct SnapshotKey {
    generation: u64,
    width: i32,
    height: i32,
}

impl EditorWake {
    fn begin_composition(&self) {
        #[cfg(any(debug_assertions, test))]
        if !self.ime_composing.get() {
            self.ime_starts.set(self.ime_starts.get().wrapping_add(1));
        }
        self.ime_composing.set(true);
    }

    fn end_composition(&self) {
        #[cfg(any(debug_assertions, test))]
        if self.ime_composing.get() {
            self.ime_ends.set(self.ime_ends.get().wrapping_add(1));
        }
        self.ime_composing.set(false);
        self.text_event.set(true);
    }

    fn invalidate_rendering(&self) {
        self.redraw_dirty.set(true);
        #[cfg(any(debug_assertions, test))]
        {
            self.snapshot_generation
                .set(self.snapshot_generation.get().wrapping_add(1));
            self.snapshot.borrow_mut().take();
        }
    }

    #[cfg(any(debug_assertions, test))]
    fn capture_snapshot(&self, hwnd: HWND) {
        if let Some(key) = self.snapshot_pending.get() {
            // The posted callback runs after egui's parent WM_PAINT returns.
            // Synchronous nested printing can silently omit individual glyph runs.
            let pixels = print_client(hwnd, key.width, key.height);
            self.snapshot_pending.set(None);
            if self.snapshot_generation.get() == key.generation {
                *self.snapshot.borrow_mut() = Some((key, pixels));
            }
            self.ctx.request_repaint();
        }
    }
}

#[derive(Clone, Copy)]
struct Placement {
    rect: Rect,
    enabled: bool,
    focus: bool,
}

struct Editor {
    hwnd: HWND,
    wake: Rc<EditorWake>,
    /// Exact model snapshot: untouched OCR text, including its line endings,
    /// must not be rewritten just because the control uses CR paragraphs.
    synced: String,
    requested: Option<Placement>,
    bounds: Option<[i32; 4]>,
    visible: bool,
    style: Option<(i32, Color32, Color32)>,
    cutouts: Vec<[i32; 4]>,
    region_size: [i32; 2],
    fully_occluded: bool,
    read_only: bool,
}

/// Owns two UI-thread-only children. Drop destroys them before releasing Msftedit.
pub(super) struct NativeEditors {
    editors: [Editor; 2],
    library: HMODULE,
    added_child_clipping: bool,
    initialized_com: bool,
    popup_rects: Vec<Rect>,
    wheel_router: Rc<WheelRouter>,
}

impl NativeEditors {
    pub(super) fn new(parent: usize, ctx: egui::Context) -> Result<Self> {
        let parent = parent as HWND;
        let mut process = 0;
        // SAFETY: The APIs validate HWND and write only to the local process id.
        let thread = unsafe { GetWindowThreadProcessId(parent, &mut process) };
        ensure!(
            !parent.is_null() && process == std::process::id()
                // SAFETY: Returns the calling thread id without side effects.
                && thread == unsafe { GetCurrentThreadId() },
            "多语言编辑器需要本进程的主界面窗口"
        );
        let dll = wide("Msftedit.dll");
        // SAFETY: The DLL name is NUL-terminated; retained until all children die.
        let library =
            unsafe { LoadLibraryExW(dll.as_ptr(), null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32) };
        ensure!(!library.is_null(), "无法加载 Windows 多语言文字引擎");
        // SAFETY: RichEdit's TSF and TOM services run in the owning UI apartment.
        // Balance successful (including already initialized) COM initialization.
        let initialized_com =
            unsafe { CoInitializeEx(null_mut(), COINIT_APARTMENTTHREADED as u32) >= 0 };
        if !initialized_com {
            // SAFETY: No child or COM interface exists yet; release our DLL reference.
            unsafe { FreeLibrary(library) };
            anyhow::bail!("Windows 多语言文字服务需要可用的界面 COM 线程");
        }
        let result = (|| {
            let wheel_router = Rc::new(WheelRouter {
                parent,
                children: Cell::new([null_mut(); 2]),
                dispatching: Cell::new(false),
                attached: Cell::new(false),
            });
            let first = Editor::new(parent, ctx.clone(), 0, wheel_router.clone())?;
            let second = Editor::new(parent, ctx, 1, wheel_router.clone())?;
            wheel_router.children.set([first.hwnd, second.hwnd]);
            wheel_router.install()?;
            // The GL parent must exclude children while redrawing its client area.
            // SAFETY: Preserve every style bit of our own UI-thread parent.
            let added_child_clipping = unsafe {
                let style = GetWindowLongW(parent, GWL_STYLE) as u32;
                if style & WS_CLIPCHILDREN == 0 {
                    SetWindowLongW(parent, GWL_STYLE, (style | WS_CLIPCHILDREN) as i32);
                    true
                } else {
                    false
                }
            };
            Ok(Self {
                editors: [first, second],
                library,
                added_child_clipping,
                initialized_com,
                popup_rects: Vec::new(),
                wheel_router,
            })
        })();
        if result.is_err() {
            // SAFETY: Failed construction has dropped any successfully made child.
            // SAFETY: Children have dropped. COM may dispatch cleanup callbacks,
            // so keep Msftedit loaded until its UI apartment is released.
            unsafe {
                CoUninitialize();
                FreeLibrary(library);
            }
        }
        result
    }

    /// Call before processing worker events, so newer OCR results win over edits.
    pub(super) fn begin_frame(&mut self, source: &mut String, translation: &mut String) {
        for (editor, text) in self.editors.iter_mut().zip([source, translation]) {
            editor.sync_edits(text);
            editor.requested = None;
        }
    }

    pub(super) fn is_focused(&self, index: usize) -> bool {
        // SAFETY: GetFocus reads only the calling UI thread's focus window.
        unsafe { GetFocus() == self.editors[index].hwnd }
    }

    pub(super) fn blur(&self) {
        for editor in &self.editors {
            // SAFETY: Return focus from our own child to its existing parent.
            unsafe {
                if GetFocus() == editor.hwnd {
                    SetFocus(editor.wake.parent);
                }
            }
        }
    }

    /// Reserve the egui card body; placement is applied after menus have rendered.
    pub(super) fn show(
        &mut self,
        index: usize,
        ui: &mut egui::Ui,
        text: &str,
        size: Vec2,
        enabled: bool,
        focus: bool,
    ) -> Response {
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        self.show_at(index, ui, text, rect, enabled, focus || response.clicked());
        response
    }

    /// Activate an already allocated empty-state body in the click's own frame.
    /// Waiting until the next frame leaves keyboard input routed to egui while
    /// the native editor is still hidden.
    pub(super) fn show_at(
        &mut self,
        index: usize,
        ui: &egui::Ui,
        text: &str,
        rect: Rect,
        enabled: bool,
        focus: bool,
    ) {
        let editor = &mut self.editors[index];
        if editor.synced != text {
            editor.set_text(text);
        }
        let clipped = rect.intersect(ui.clip_rect());
        if clipped.is_positive() {
            editor.requested = Some(Placement {
                rect: clipped,
                enabled,
                focus,
            });
        }
    }

    pub(super) fn end_frame(&mut self, ctx: &egui::Context, hidden: bool) {
        let popups = active_popup_rects(ctx);
        let hidden = hidden || popups.is_none() || ctx.memory(|m| m.top_modal_layer().is_some());
        self.popup_rects = popups.unwrap_or_default();
        let palette = Palette::get(ctx);
        for editor in &mut self.editors {
            if let Some(placement) = editor.requested.filter(|_| !hidden) {
                editor.place(placement, ctx.pixels_per_point(), &palette);
                if editor.apply_occlusion(&self.popup_rects, ctx.pixels_per_point()) {
                    editor.queue_redraw_after_frame();
                } else {
                    editor.hide();
                }
            } else {
                editor.hide();
            }
        }
        if !self.popup_rects.is_empty() {
            // A clipped editor stays visible, but keyboard input belongs to the
            // egui menu. Return focus after placement so a pending editor focus
            // request cannot reclaim it; RichEdit retains its text and selection.
            self.blur();
        }
        // eframe consumes PlatformOutput after this method. Tell it that the
        // focused native child owns IME, otherwise an egui TextEdit -> RichEdit
        // transition disables IME on all existing child windows on Windows.
        for editor in &self.editors {
            if editor.visible && !editor.read_only {
                // SAFETY: Query only this UI thread's keyboard focus.
                if unsafe { GetFocus() } == editor.hwnd {
                    native_ime::restore_context_if_missing(editor.hwnd);
                    native_ime::publish_ime_output(ctx, editor.wake.parent, editor.hwnd);
                    break;
                }
            }
        }
    }

    #[cfg(any(debug_assertions, test))]
    pub(super) fn popup_rects(&self) -> &[Rect] {
        &self.popup_rects
    }

    #[cfg(any(debug_assertions, test))]
    pub(super) fn ime_state(&self, index: usize) -> (bool, u64, u64, u64) {
        let wake = &self.editors[index].wake;
        (
            wake.ime_composing.get(),
            wake.ime_starts.get(),
            wake.ime_ends.get(),
            wake.format_passes.get(),
        )
    }

    #[cfg(any(debug_assertions, test))]
    pub(super) fn native_redraw_requests(&self) -> [u64; 2] {
        self.editors
            .each_ref()
            .map(|editor| editor.wake.redraw_requests.get())
    }

    #[cfg(any(debug_assertions, test))]
    pub(super) fn scroll_positions(&self) -> [(i32, i32); 2] {
        self.editors.each_ref().map(|editor| {
            let mut point = POINT { x: 0, y: 0 };
            // SAFETY: Read only this owned editor's scroll offset into local storage.
            unsafe {
                SendMessageW(editor.hwnd, WM_USER + 221, 0, &mut point as *mut _ as isize);
            }
            (point.x, point.y)
        })
    }

    #[cfg(any(debug_assertions, test))]
    pub(super) fn wheel_delivery_counts(&self) -> [u64; 2] {
        self.editors
            .each_ref()
            .map(|editor| editor.wake.wheel_deliveries.get())
    }

    /// egui screenshots contain only its GL framebuffer. Ask our own children to
    /// paint into memory and composite them; never capture the desktop or another app.
    #[cfg(any(debug_assertions, test))]
    pub(super) fn composite_screenshot(&self, image: &mut ColorImage) -> Result<bool> {
        let mut ready = true;
        for editor in &self.editors {
            if !editor.visible || editor.fully_occluded {
                continue;
            }
            if let Some([_, _, width, height]) = editor.bounds {
                let key = SnapshotKey {
                    generation: editor.wake.snapshot_generation.get(),
                    width,
                    height,
                };
                if !editor
                    .wake
                    .snapshot
                    .borrow()
                    .as_ref()
                    .is_some_and(|(cached, _)| *cached == key)
                {
                    ready = false;
                    if editor.wake.snapshot_pending.get().is_none() {
                        editor.wake.snapshot_pending.set(Some(key));
                        // SAFETY: Queue only our own child; no bitmap/DC crosses threads.
                        let posted = unsafe { PostMessageW(editor.hwnd, SNAPSHOT_MESSAGE, 0, 0) };
                        if posted == 0 {
                            editor.wake.snapshot_pending.set(None);
                        }
                        ensure!(posted != 0, "无法安排多语言结果编辑器的测试截图");
                    }
                }
            }
        }
        if !ready {
            return Ok(false);
        }
        for (index, editor) in self.editors.iter().enumerate() {
            if !editor.visible || editor.fully_occluded {
                continue;
            }
            if let Some([x, y, width, height]) = editor.bounds {
                let snapshot = editor.wake.snapshot.borrow();
                if let Some((_, Some(pixels))) = snapshot.as_ref() {
                    for row in 0..height as usize {
                        let target_y = y + row as i32;
                        if !(0..image.height() as i32).contains(&target_y) {
                            continue;
                        }
                        for col in 0..width as usize {
                            if editor.cutouts.iter().any(|&[left, top, right, bottom]| {
                                (left..right).contains(&(col as i32))
                                    && (top..bottom).contains(&(row as i32))
                            }) {
                                continue;
                            }
                            let target_x = x + col as i32;
                            if (0..image.width() as i32).contains(&target_x) {
                                image[(target_x as usize, target_y as usize)] =
                                    pixels[row * width as usize + col];
                            }
                        }
                    }
                } else {
                    let mut client = windows_sys::Win32::Foundation::RECT {
                        left: 0,
                        top: 0,
                        right: 0,
                        bottom: 0,
                    };
                    // SAFETY: Diagnostic state reads only this editor's own window.
                    let visible = unsafe {
                        GetClientRect(editor.hwnd, &mut client);
                        IsWindowVisible(editor.hwnd)
                    };
                    let freeze_count = FormatGuard::new(editor.hwnd).freeze_count - 1;
                    anyhow::bail!("无法绘制多语言结果编辑器的测试截图: child={index} hwnd={:?} bounds={x},{y},{width},{height} client={},{},{},{} visible={visible} text_chars={} frozen={freeze_count}", editor.hwnd,client.left,client.top,client.right,client.bottom,editor.synced.chars().count());
                }
            }
        }
        Ok(true)
    }
}

impl Drop for NativeEditors {
    fn drop(&mut self) {
        self.wheel_router.uninstall();
        for editor in &mut self.editors {
            editor.destroy();
        }
        if self.added_child_clipping {
            let parent = self.editors[0].wake.parent;
            // SAFETY: Remove only the style bit this owner added, preserving any
            // other eframe changes; the parent may already have been destroyed.
            unsafe {
                if IsWindow(parent) != 0 {
                    let style = GetWindowLongW(parent, GWL_STYLE) as u32;
                    SetWindowLongW(parent, GWL_STYLE, (style & !WS_CLIPCHILDREN) as i32);
                }
            }
        }
        if self.initialized_com {
            // SAFETY: Balanced with this guard's successful UI-thread initialization.
            unsafe { CoUninitialize() };
        }
        // SAFETY: Children and their COM services have been released first.
        unsafe { FreeLibrary(self.library) };
    }
}

impl Editor {
    fn new(
        parent: HWND,
        ctx: egui::Context,
        index: usize,
        wheel_router: Rc<WheelRouter>,
    ) -> Result<Self> {
        let class = wide("RICHEDIT50W");
        // SAFETY: The class was registered by Msftedit; parent is this UI thread's
        // window. The new child begins hidden and never opens a top-level window.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                wide("").as_ptr(),
                WS_CHILD
                    | WS_CLIPSIBLINGS
                    | WS_TABSTOP
                    | WS_VSCROLL
                    | (ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN | ES_NOHIDESEL) as u32,
                0,
                0,
                1,
                1,
                parent,
                (0x5340 + index) as _,
                null_mut(),
                null(),
            )
        };
        ensure!(!hwnd.is_null(), "无法创建 Windows 多语言结果编辑器");
        let wake = Rc::new(EditorWake {
            ctx,
            parent,
            wheel_router,
            attached: Cell::new(false),
            text_event: Cell::new(false),
            ime_composing: Cell::new(false),
            #[cfg(any(debug_assertions, test))]
            format_passes: Cell::new(0),
            #[cfg(any(debug_assertions, test))]
            ime_starts: Cell::new(0),
            #[cfg(any(debug_assertions, test))]
            ime_ends: Cell::new(0),
            redraw_pending: Cell::new(false),
            redraw_dirty: Cell::new(true),
            last_visible: Cell::new(false),
            #[cfg(any(debug_assertions, test))]
            redraw_requests: Cell::new(0),
            #[cfg(any(debug_assertions, test))]
            wheel_deliveries: Cell::new(0),
            #[cfg(any(debug_assertions, test))]
            snapshot_generation: Cell::new(0),
            #[cfg(any(debug_assertions, test))]
            snapshot_pending: Cell::new(None),
            #[cfg(any(debug_assertions, test))]
            snapshot: RefCell::new(None),
        });
        let reference = Rc::into_raw(wake.clone()) as usize;
        // SAFETY: The explicit Rc reference retains the callback state. Installation
        // and removal happen on the window's owning thread.
        let installed =
            unsafe { SetWindowSubclass(hwnd, Some(editor_subclass), SUBCLASS_ID, reference) };
        if installed == 0 {
            // SAFETY: No callback was installed, so reclaim its reference and child.
            unsafe {
                drop(Rc::from_raw(reference as *const EditorWake));
                DestroyWindow(hwnd);
            }
            anyhow::bail!("无法连接多语言编辑器的输入事件");
        }
        wake.attached.set(true);
        let mut editor = Self {
            hwnd,
            wake,
            synced: String::new(),
            requested: None,
            bounds: None,
            visible: false,
            style: None,
            cutouts: Vec::new(),
            region_size: [0, 0],
            fully_occluded: false,
            read_only: false,
        };
        let bidi = BidiOptions {
            size: size_of::<BidiOptions>() as u32,
            mask: 0x0080,
            effects: 0x0080,
        };
        // SAFETY: All messages target our child. Flags come from Richedit.h. Plain
        // text plus multiple code pages retains full Unicode and strips rich paste
        // formatting; UBA and contextual reading are applied without changing text.
        unsafe {
            SendMessageW(hwnd, EM_SETTEXTMODE, 2 | 8 | 32, 0);
            SendMessageW(hwnd, EM_EXLIMITTEXT, 0, 0x7FFF_FFFE);
            SendMessageW(hwnd, EM_SETTYPOGRAPHYOPTIONS, 1, 1);
            SendMessageW(hwnd, EM_SETBIDIOPTIONS, 0, &bidi as *const _ as isize);
            let options = SendMessageW(hwnd, EM_GETLANGOPTIONS, 0, 0);
            // Keep font binding, but never switch the user's keyboard language.
            SendMessageW(
                hwnd,
                EM_SETLANGOPTIONS,
                0,
                (options | 2) & !(1 | 0x10 | 0x80),
            );
            // TSF supports IME; disabling sequence filtering preserves OCR/pasted
            // Unicode, including combining marks supplied in their original order.
            SendMessageW(hwnd, EM_SETEDITSTYLE, 0x0001_0800, 0x0001_0800);
            // RichEdit's TSF path reports composition through WM_NOTIFY rather
            // than necessarily emitting legacy WM_IME_* messages to the child.
            let events = SendMessageW(hwnd, WM_USER + 59, 0, 0); // EM_GETEVENTMASK
            SendMessageW(hwnd, WM_USER + 69, 0, events | 0x3000_0000); // EM_SETEVENTMASK
        }
        editor.apply_style(
            editor.wake.ctx.pixels_per_point(),
            true,
            &Palette::get(&editor.wake.ctx),
        );
        Ok(editor)
    }

    fn set_text(&mut self, text: &str) {
        // A deliberate new OCR/result document supersedes any pending preedit.
        self.wake.ime_composing.set(false);
        self.wake.invalidate_rendering();
        let utf16: Vec<u16> = native_line_endings(text).encode_utf16().collect();
        // SAFETY: UTF-16 storage remains live for the synchronous stream callback;
        // Windows is little-endian and every pair of bytes belongs to this slice.
        let bytes =
            unsafe { std::slice::from_raw_parts(utf16.as_ptr() as *const u8, utf16.len() * 2) };
        let mut input = TextStream { bytes, offset: 0 };
        let mut stream = EditStream {
            cookie: &mut input as *mut _ as usize,
            error: 0,
            callback: stream_text,
        };
        // WM_SETTEXT and even EM_REPLACESEL recognize an RTF header in rich mode.
        // SF_TEXT | SF_UNICODE explicitly requests literal UTF-16 plain text.
        // SAFETY: The callback and all stack-owned stream data live until return.
        unsafe {
            SendMessageW(
                self.hwnd,
                WM_USER + 73,
                0x0011,
                &mut stream as *mut _ as isize,
            );
            SendMessageW(self.hwnd, 0x00B1, 0, 0);
            SendMessageW(self.hwnd, EM_SETMODIFY, 0, 0);
        }
        self.format_document(text);
        // SAFETY: New OCR content replaces the document; it starts with no undo
        // history. Editing updates below use TOM suspension and preserve history.
        unsafe { SendMessageW(self.hwnd, 0x00CD, 0, 0) };
        self.synced = text.to_owned();
        self.wake.text_event.set(false);
    }

    fn format_document(&self, text: &str) {
        if self.wake.ime_composing.get() {
            return;
        }
        #[cfg(any(debug_assertions, test))]
        self.wake
            .format_passes
            .set(self.wake.format_passes.get().wrapping_add(1));
        self.wake.invalidate_rendering();
        let _guard = FormatGuard::new(self.hwnd);
        // Rich text mode is needed for independent font/paragraph formats. Apply
        // reading direction per paragraph without altering logical Unicode order.
        // SAFETY: Valid SDK POD with masks restricted to alignment/direction.
        let mut format: ParaFormat = unsafe { zeroed() };
        format.size = size_of::<ParaFormat>() as u32;
        format.mask = 0x0001_0008;
        // Bind each script explicitly: older RichEdit font linking can choose a
        // mismatched script face after several unrelated languages share a result.
        // SAFETY: CHARFORMATW is SDK POD; all unused fields remain zero.
        let mut font: CharFormatW = unsafe { zeroed() };
        font.size = size_of::<CharFormatW>() as u32;
        font.mask = CFM_FACE | CFM_SIZE | 0x0800_0000;
        font.charset = 1;
        font.height = self.style.map_or(225, |style| style.0);
        for (start, end, face) in native_font_runs::font_runs(&native_line_endings(text)) {
            font.face.fill(0);
            for (target, source) in font.face.iter_mut().zip(face.encode_utf16()) {
                *target = source;
            }
            // SAFETY: Checked UTF-16 ranges and NUL-terminated font face are sent
            // synchronously to this child's own selection. No characters change.
            unsafe {
                SendMessageW(self.hwnd, 0x00B1, start as usize, end as isize);
                SendMessageW(
                    self.hwnd,
                    EM_SETCHARFORMAT,
                    0x0021,
                    &font as *const _ as isize,
                );
            }
        }
        for (start, end, rtl) in paragraph_runs(text) {
            format.effects = u16::from(rtl);
            format.alignment = if rtl { 2 } else { 1 };
            // SAFETY: UTF-16 offsets exactly cover this paragraph in the child.
            unsafe {
                SendMessageW(self.hwnd, 0x00B1, start, end as isize);
                SendMessageW(self.hwnd, EM_SETPARAFORMAT, 0, &format as *const _ as isize);
            }
        }
    }

    fn sync_edits(&mut self, text: &mut String) {
        // RichEdit owns the temporary composition string and selection. Moving
        // its selection to apply font runs would commit/cancel the first Latin
        // preedit character, closing the IME before a candidate can be chosen.
        // Keep dirty/event flags until END; expose only committed text to Rust.
        if self.wake.ime_composing.get() {
            return;
        }
        // SAFETY: A read-only message to this thread's owned RichEdit child.
        let modified = unsafe { SendMessageW(self.hwnd, EM_GETMODIFY, 0, 0) != 0 };
        if modified || self.wake.text_event.replace(false) {
            // Never replace a newer model value with a stale control's contents.
            if *text == self.synced {
                if let Some(value) = read_text(self.hwnd) {
                    if value != self.synced {
                        self.format_document(&value);
                    }
                    *text = value;
                    self.synced.clone_from(text);
                }
            }
            // SAFETY: Resets only the child control's modification bit, not undo.
            unsafe { SendMessageW(self.hwnd, EM_SETMODIFY, 0, 0) };
        }
    }

    fn apply_style(&mut self, pixels_per_point: f32, enabled: bool, p: &Palette) {
        // Theme/DPI changes may arrive between IME messages. Defer selection and
        // character-format mutations until composition commits or is cancelled.
        if self.wake.ime_composing.get() {
            return;
        }
        // SAFETY: This query reads the DPI of our own live child.
        let dpi = unsafe { GetDpiForWindow(self.hwnd) }.max(96) as f32;
        let height = (15.0 * pixels_per_point * 1440.0 / dpi).round() as i32;
        let text_color = if enabled { p.text } else { p.muted };
        let style = (height, text_color, p.panel);
        if self.style != Some(style) {
            self.wake.invalidate_rendering();
            let _guard = FormatGuard::new(self.hwnd);
            // SAFETY: CHARFORMATW is an SDK POD and all fields are initialized below.
            let mut format: CharFormatW = unsafe { zeroed() };
            format.size = size_of::<CharFormatW>() as u32;
            format.mask = CFM_SIZE | CFM_COLOR | CFM_FACE;
            format.height = height;
            format.text_color = color_ref(text_color);
            for (target, source) in format.face.iter_mut().zip("Segoe UI".encode_utf16()) {
                *target = source;
            }
            // SAFETY: Synchronous messages use a fully initialized SDK structure.
            // Apply face only to the default; font binding chooses actual script
            // fonts. Updating colors/size must not replace those script choices.
            unsafe {
                let modified = SendMessageW(self.hwnd, EM_GETMODIFY, 0, 0);
                SendMessageW(self.hwnd, EM_SETCHARFORMAT, 0, &format as *const _ as isize);
                format.mask = CFM_SIZE | CFM_COLOR;
                SendMessageW(
                    self.hwnd,
                    EM_SETCHARFORMAT,
                    SCF_ALL,
                    &format as *const _ as isize,
                );
                SendMessageW(self.hwnd, EM_SETBKGNDCOLOR, 0, color_ref(p.panel) as isize);
                SendMessageW(self.hwnd, EM_SETMODIFY, modified as usize, 0);
            }
            self.style = Some(style);
        }
    }

    fn place(&mut self, placement: Placement, pixels_per_point: f32, p: &Palette) {
        let rect = placement.rect;
        let bounds = [
            (rect.min.x * pixels_per_point).round() as i32,
            (rect.min.y * pixels_per_point).round() as i32,
            (rect.width() * pixels_per_point).round().max(1.0) as i32,
            (rect.height() * pixels_per_point).round().max(1.0) as i32,
        ];
        self.apply_style(pixels_per_point, placement.enabled, p);
        let read_only = !placement.enabled;
        if self.read_only != read_only {
            // Repeating EM_SETREADONLY can invalidate RichEdit even when unchanged.
            // SAFETY: Read-only still permits native selection and copying.
            unsafe { SendMessageW(self.hwnd, EM_SETREADONLY, usize::from(read_only), 0) };
            self.read_only = read_only;
            self.wake.invalidate_rendering();
        }
        if self.bounds != Some(bounds) || !self.visible {
            self.wake.invalidate_rendering();
            // SAFETY: All coordinates are bounded by the allocated egui body, the
            // target is a child of our UI window, and activation is suppressed.
            unsafe {
                SetWindowPos(
                    self.hwnd,
                    HWND_TOP,
                    bounds[0],
                    bounds[1],
                    bounds[2],
                    bounds[3],
                    SWP_NOACTIVATE | SWP_NOZORDER | SWP_SHOWWINDOW,
                );
            }
            self.bounds = Some(bounds);
            self.visible = true;
        }
        if placement.focus && placement.enabled {
            // SAFETY: Focus goes only to this visible child on the same UI thread.
            unsafe { SetFocus(self.hwnd) };
        }
    }

    fn queue_redraw_after_frame(&self) {
        // Visibility can change through the top-level window (startup/tray),
        // without changing the child's own WS_VISIBLE flag or placement.
        // SAFETY: This is a read-only query of our own child and parent chain.
        let visible = unsafe { IsWindowVisible(self.hwnd) != 0 };
        if self.wake.last_visible.replace(visible) != visible && visible {
            self.wake.redraw_dirty.set(true);
        }
        if self.fully_occluded || !self.wake.redraw_dirty.get() {
            return;
        }
        if !self.wake.redraw_pending.replace(true) {
            // eframe swaps the OpenGL parent after App::update returns. Repaint
            // the child from a queued message after that swap, so an old loading
            // or menu frame cannot remain inside its rectangle. Coalesce queued
            // frames and never request a parent redraw from the callback.
            // SAFETY: This message targets only our UI-thread-owned child.
            if unsafe { PostMessageW(self.hwnd, REDRAW_MESSAGE, 0, 0) } == 0 {
                self.wake.redraw_pending.set(false);
            } else {
                #[cfg(any(debug_assertions, test))]
                self.wake
                    .redraw_requests
                    .set(self.wake.redraw_requests.get().wrapping_add(1));
            }
        }
    }

    fn apply_occlusion(&mut self, popups: &[Rect], pixels_per_point: f32) -> bool {
        let Some(bounds @ [_, _, width, height]) = self.bounds else {
            return true;
        };
        let cutouts = popup_cutouts(popups, bounds, pixels_per_point);
        if self.cutouts == cutouts && self.region_size == [width, height] {
            return true;
        }
        let mut fully_occluded = false;
        // SAFETY: Regions affect only the child we own. SetWindowRgn takes ownership
        // on success; temporary difference regions are always deleted here.
        unsafe {
            let region = if cutouts.is_empty() {
                null_mut()
            } else {
                CreateRectRgn(0, 0, width, height)
            };
            if !cutouts.is_empty() && region.is_null() {
                return false;
            }
            for &[left, top, right, bottom] in &cutouts {
                let hole = CreateRectRgn(left, top, right, bottom);
                if hole.is_null() {
                    DeleteObject(region);
                    return false;
                }
                let kind = CombineRgn(region, region, hole, RGN_DIFF);
                DeleteObject(hole);
                if kind == 0 {
                    DeleteObject(region);
                    return false;
                }
                fully_occluded = kind == NULLREGION;
            }
            if SetWindowRgn(self.hwnd, region, 1) == 0 {
                if !region.is_null() {
                    DeleteObject(region);
                }
                return false;
            }
        }
        self.cutouts = cutouts;
        self.region_size = [width, height];
        self.fully_occluded = fully_occluded;
        self.wake.invalidate_rendering();
        // Refresh both parent GL buffers after exposing or restoring a menu hole.
        self.wake.ctx.request_repaint();
        true
    }

    fn hide(&mut self) {
        if self.visible {
            self.wake.invalidate_rendering();
            // SAFETY: Hidden children must not retain keyboard/IME focus. Returning
            // focus to their own parent does not activate another application.
            unsafe {
                if GetFocus() == self.hwnd {
                    SetFocus(self.wake.parent);
                }
                ShowWindow(self.hwnd, SW_HIDE);
            }
            self.visible = false;
            // A newly exposed GL rectangle may still belong to the other back
            // buffer. Schedule one more parent frame after this visibility change.
            self.wake.ctx.request_repaint();
        }
    }

    fn destroy(&mut self) {
        if !self.hwnd.is_null() {
            self.hide();
            // SAFETY: On the UI thread, destroy only the child we created. Its
            // WM_NCDESTROY callback releases the retained callback reference.
            unsafe {
                if self.wake.attached.get() && IsWindow(self.hwnd) != 0 {
                    DestroyWindow(self.hwnd);
                }
            }
            self.hwnd = null_mut();
        }
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        self.destroy();
    }
}

unsafe extern "system" fn editor_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    reference: usize,
) -> LRESULT {
    // SAFETY: Installation owns one Rc reference until WM_NCDESTROY. The temporary
    // clone also protects against destruction during a nested native callback.
    let state = unsafe {
        Rc::increment_strong_count(reference as *const EditorWake);
        Rc::from_raw(reference as *const EditorWake)
    };
    if message == WM_IME_STARTCOMPOSITION || (message == WM_IME_COMPOSITION && lparam & 0x0008 != 0)
    {
        // Mark before native dispatch: RichEdit/TSF may reenter the message loop.
        state.begin_composition();
    }
    if message == IME_NOTIFICATION_MESSAGE {
        if wparam == EN_STARTCOMPOSITION as usize {
            state.begin_composition();
        } else if wparam == EN_ENDCOMPOSITION as usize && lparam & 1 != 0 {
            // ECN_NEWTEXT alone is an update inside the existing composition.
            state.end_composition();
        }
        state.invalidate_rendering();
        let _ = catch_unwind(AssertUnwindSafe(|| state.ctx.request_repaint()));
        return 0;
    }
    if message == WM_SETFOCUS {
        native_ime::restore_context_if_missing(hwnd);
    }
    let wheel = matches!(message, WM_MOUSEWHEEL | WM_MOUSEHWHEEL);
    if wheel {
        if let Some(result) = state.wheel_router.forward(hwnd, message, wparam, lparam) {
            return result;
        }
        #[cfg(any(debug_assertions, test))]
        state
            .wheel_deliveries
            .set(state.wheel_deliveries.get().wrapping_add(1));
    }
    if message == REDRAW_MESSAGE {
        state.redraw_pending.set(false);
        state.redraw_dirty.set(false);
        // A popup may have hidden this child since the frame queued the redraw.
        // SAFETY: Paint only our still-visible child after the parent's GL swap;
        // neither the egui parent nor another application's windows are redrawn.
        unsafe {
            if IsWindowVisible(hwnd) != 0 {
                RedrawWindow(
                    hwnd,
                    null(),
                    null_mut(),
                    RDW_INVALIDATE | RDW_NOERASE | RDW_UPDATENOW,
                );
            }
        }
        return 0;
    }
    #[cfg(any(debug_assertions, test))]
    if message == SNAPSHOT_MESSAGE {
        if catch_unwind(AssertUnwindSafe(|| state.capture_snapshot(hwnd))).is_err() {
            if let Some(key) = state.snapshot_pending.take() {
                *state.snapshot.borrow_mut() = Some((key, None));
            }
            let _ = catch_unwind(AssertUnwindSafe(|| state.ctx.request_repaint()));
        }
        return 0;
    }
    if message == WM_NCDESTROY && state.attached.replace(false) {
        // SAFETY: Remove this exact subclass once, then reclaim its explicit Rc.
        unsafe {
            RemoveWindowSubclass(hwnd, Some(editor_subclass), SUBCLASS_ID);
            drop(Rc::from_raw(reference as *const EditorWake));
        }
    }
    // Rich text layout supplies per-run fonts; clipboard input remains plain
    // Unicode so external formatting and embedded objects never enter OCR text.
    let wheel_guard = wheel.then(|| WheelDispatchGuard::new(&state.wheel_router.dispatching));
    // SAFETY: The same owned control handles either its default message or a
    // standard CF_UNICODETEXT paste; only user-initiated WM_PASTE accesses clipboard.
    let result = unsafe {
        if message == WM_PASTE {
            SendMessageW(hwnd, WM_USER + 64, 13, 0) // EM_PASTESPECIAL, CF_UNICODETEXT
        } else {
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
    };
    drop(wheel_guard);
    if matches!(message, WM_IME_ENDCOMPOSITION | WM_KILLFOCUS) {
        // Default processing must finish its commit/cancel before model sync.
        state.end_composition();
    }
    if matches!(
        message,
        WM_CHAR | WM_CUT | WM_PASTE | WM_CLEAR | WM_UNDO | 0x00C7 | 0x0454 | WM_IME_ENDCOMPOSITION
    ) || (message == WM_KEYDOWN && matches!(wparam, 0x59 | 0x5A))
    {
        // Undo can restore EM_GETMODIFY=false along with the saved text. Remember
        // the native editing event so the Rust model still observes that change.
        state.text_event.set(true);
    }
    if matches!(
        message,
        WM_CHAR
            | WM_KEYDOWN
            | WM_KEYUP
            | WM_CUT
            | WM_PASTE
            | WM_CLEAR
            | WM_UNDO
            | WM_IME_COMPOSITION
            | WM_IME_STARTCOMPOSITION
            | WM_IME_ENDCOMPOSITION
            | WM_SETFOCUS
            | WM_KILLFOCUS
            | WM_LBUTTONUP
            | WM_MOUSEWHEEL
            | WM_MOUSEHWHEEL
            | WM_VSCROLL
            | WM_HSCROLL
    ) {
        state.invalidate_rendering();
        // Never unwind across the system callback boundary. Native input wakes
        // egui immediately so text counts, translation, and copy see current edits.
        let _ = catch_unwind(AssertUnwindSafe(|| state.ctx.request_repaint()));
        // SAFETY: Post to the parent that owns this child; no external window I/O.
        unsafe { PostMessageW(state.parent, WM_PAINT, 0, 0) };
    }
    result
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn native_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r")
}

fn active_popup_rects(ctx: &egui::Context) -> Option<Vec<Rect>> {
    let (open, areas) = ctx.memory(|memory| {
        (
            memory.any_popup_open(),
            memory
                .areas()
                .visible_layer_ids()
                .into_iter()
                .filter(|layer| memory.is_popup_open(layer.id))
                .filter_map(|layer| memory.area_rect(layer.id).map(|rect| (layer, rect)))
                .collect::<Vec<_>>(),
        )
    });
    if !open {
        return Some(Vec::new());
    }
    let shadow = ctx.style().visuals.popup_shadow.margin();
    let rects: Vec<_> = areas
        .into_iter()
        .filter_map(|(layer, rect)| {
            let rect = ctx
                .layer_transform_to_global(layer)
                .map_or(rect, |transform| transform * rect);
            (rect.is_finite() && rect.is_positive())
                .then_some(Rect::from_min_max(
                    rect.min - egui::vec2(shadow.left.max(0.0), shadow.top.max(0.0)),
                    rect.max + egui::vec2(shadow.right.max(0.0), shadow.bottom.max(0.0)),
                ))
                .filter(Rect::is_positive)
        })
        .collect();
    // An unmeasured/unknown popup must keep the conservative whole-editor fallback.
    (!rects.is_empty()).then_some(rects)
}

fn popup_cutouts(
    popups: &[Rect],
    [x, y, width, height]: [i32; 4],
    pixels_per_point: f32,
) -> Vec<[i32; 4]> {
    let mut cutouts: Vec<_> = popups
        .iter()
        .filter_map(|rect| {
            let left = ((rect.min.x * pixels_per_point).floor() as i32 - x).clamp(0, width);
            let top = ((rect.min.y * pixels_per_point).floor() as i32 - y).clamp(0, height);
            let right = ((rect.max.x * pixels_per_point).ceil() as i32 - x).clamp(0, width);
            let bottom = ((rect.max.y * pixels_per_point).ceil() as i32 - y).clamp(0, height);
            (left < right && top < bottom).then_some([left, top, right, bottom])
        })
        .collect();
    cutouts.sort_unstable();
    cutouts.dedup();
    cutouts
}

fn paragraph_runs(text: &str) -> Vec<(usize, usize, bool)> {
    let mut runs: Vec<(usize, usize, bool)> = Vec::new();
    let mut start = 0;
    for paragraph in native_line_endings(text).split('\r') {
        let end = start + paragraph.encode_utf16().count();
        let rtl = paragraph_is_rtl(paragraph);
        if let Some(last) = runs.last_mut().filter(|last| last.2 == rtl) {
            last.1 = end;
        } else {
            runs.push((start, end, rtl));
        }
        start = end + 1;
    }
    runs
}

fn paragraph_is_rtl(text: &str) -> bool {
    let utf16: Vec<_> = text.encode_utf16().collect();
    let Ok(length) = i32::try_from(utf16.len()) else {
        return false;
    };
    if length == 0 {
        return false;
    }
    let mut classes = vec![0u16; utf16.len()];
    // SAFETY: NLS reads exactly length UTF-16 units and writes one class per unit.
    let classified =
        unsafe { GetStringTypeW(CT_CTYPE2, utf16.as_ptr(), length, classes.as_mut_ptr()) != 0 };
    let mut offset = 0;
    for c in text.chars() {
        // Some Windows NLS versions classify supplementary characters by their
        // surrogates. Retain the RTL scripts' strong direction in that case,
        // including Adlam; BMP classification always comes from system Unicode.
        if c.is_alphabetic()
            && matches!(c as u32, 0x10800..=0x10FFF | 0x1E800..=0x1E95F | 0x1EE00..=0x1EEFF)
        {
            return true;
        }
        if classified {
            match u32::from(classes[offset]) {
                C2_LEFTTORIGHT => return false,
                C2_RIGHTTOLEFT => return true,
                _ => {}
            }
        }
        offset += c.len_utf16();
    }
    false
}

fn color_ref(color: Color32) -> u32 {
    color.r() as u32 | ((color.g() as u32) << 8) | ((color.b() as u32) << 16)
}

fn read_text(hwnd: HWND) -> Option<String> {
    let length = GetTextLengthEx {
        flags: 2 | 8,
        codepage: 1200,
    };
    // SAFETY: SDK length structure is valid for this synchronous read-only message.
    let count = unsafe { SendMessageW(hwnd, EM_GETTEXTLENGTHEX, &length as *const _ as usize, 0) };
    if !(0..=i32::MAX as isize).contains(&count) {
        return None;
    }
    let mut buffer = vec![0u16; count as usize + 1];
    let options = GetTextEx {
        cb: u32::try_from(buffer.len().checked_mul(2)?).ok()?,
        flags: 4, // GT_RAWTEXT: don't silently filter Unicode format characters.
        codepage: 1200,
        default_char: null(),
        used_default_char: null_mut(),
    };
    // SAFETY: Size is the byte capacity of this writable UTF-16 buffer. No code
    // page conversion takes place and the return value excludes its terminator.
    let copied = unsafe {
        SendMessageW(
            hwnd,
            EM_GETTEXTEX,
            &options as *const _ as usize,
            buffer.as_mut_ptr() as isize,
        )
    };
    let copied = usize::try_from(copied)
        .ok()?
        .min(buffer.len().saturating_sub(1));
    String::from_utf16(&buffer[..copied])
        .ok()
        .map(|text| text.replace("\r\n", "\n").replace('\r', "\n"))
}

#[cfg(any(debug_assertions, test))]
fn print_client(hwnd: HWND, width: i32, height: i32) -> Option<Vec<Color32>> {
    if width <= 0 || height <= 0 {
        return None;
    }
    let count = (width as usize).checked_mul(height as usize)?;
    if count > 64 * 1024 * 1024 {
        return None;
    }
    // SAFETY: BITMAPINFO is SDK POD; initialize a top-down 32-bit RGB bitmap.
    let mut info: BITMAPINFO = unsafe { zeroed() };
    info.bmiHeader.biSize = size_of_val_header(&info);
    info.bmiHeader.biWidth = width;
    info.bmiHeader.biHeight = -height;
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    info.bmiHeader.biCompression = BI_RGB;
    let mut data = null_mut();
    // SAFETY: A memory DC requires no external screen/window capture.
    let dc = unsafe { CreateCompatibleDC(null_mut()) };
    if dc.is_null() {
        return None;
    }
    // SAFETY: Valid bitmap metadata; data receives the bitmap's allocated pixels.
    let bitmap = unsafe { CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut data, null_mut(), 0) };
    if bitmap.is_null() || data.is_null() {
        // SAFETY: Free the memory DC allocated above on this failure path.
        unsafe { DeleteDC(dc) };
        return None;
    }
    // SAFETY: Select only our allocated bitmap, ask only our child to render into
    // its memory DC, flush GDI before CPU reads, and restore/free all GDI handles.
    let pixels = unsafe {
        let old = SelectObject(dc, bitmap);
        std::ptr::write_bytes(data, 0, count * 4);
        RedrawWindow(
            hwnd,
            null(),
            null_mut(),
            RDW_INVALIDATE | RDW_ERASE | RDW_UPDATENOW,
        );
        let saved = SaveDC(dc);
        let success = PrintWindow(hwnd, dc, PW_CLIENTONLY | 2) != 0;
        GdiFlush();
        RestoreDC(dc, saved);
        let empty = std::slice::from_raw_parts(data as *const u8, count * 4)
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| p[0] == 0 && p[1] == 0 && p[2] == 0);
        if !success || empty {
            // Hidden test fixtures are not composited by PrintWindow. RichEdit's
            // complete WM_PRINT path can still lay out and paint its own client.
            let saved = SaveDC(dc);
            SendMessageW(
                hwnd,
                WM_PRINTCLIENT,
                dc as usize,
                (PRF_CLIENT | PRF_ERASEBKGND) as isize,
            );
            RestoreDC(dc, saved);
        }
        GdiFlush();
        let bytes = std::slice::from_raw_parts(data as *const u8, count * 4);
        let pixels: Vec<Color32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| Color32::from_rgb(p[2], p[1], p[0]))
            .collect();
        SelectObject(dc, old);
        DeleteObject(bitmap);
        DeleteDC(dc);
        if pixels.iter().any(|pixel| *pixel != Color32::BLACK) {
            Some(pixels)
        } else {
            None
        }
    };
    pixels
}

#[cfg(any(debug_assertions, test))]
fn size_of_val_header(info: &BITMAPINFO) -> u32 {
    std::mem::size_of_val(&info.bmiHeader) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestParent(HWND);

    impl TestParent {
        fn new() -> Self {
            // SAFETY: STATIC is a system window class. The test owns this hidden
            // top-level fixture and never touches the user's live application.
            let hwnd = unsafe {
                CreateWindowExW(
                    0,
                    wide("STATIC").as_ptr(),
                    wide("").as_ptr(),
                    0,
                    0,
                    0,
                    800,
                    600,
                    null_mut(),
                    null_mut(),
                    null_mut(),
                    null(),
                )
            };
            assert!(!hwnd.is_null());
            Self(hwnd)
        }
    }

    impl Drop for TestParent {
        fn drop(&mut self) {
            // SAFETY: Destroys only the test-created hidden parent.
            unsafe { DestroyWindow(self.0) };
        }
    }

    #[test]
    fn ime_preedit_does_not_change_model_selection_or_format_until_commit() {
        let parent = TestParent::new();
        let ctx = egui::Context::default();
        let mut editors = NativeEditors::new(parent.0 as usize, ctx.clone()).unwrap();
        for editor in &mut editors.editors {
            let mut model = "prefix ".to_owned();
            editor.set_text(&model);
            let count = editor.wake.format_passes.get();
            let original_style = editor.style;
            // SAFETY: Model an IME-owned provisional span in this hidden test
            // control. No desktop key injection or system clipboard is used.
            unsafe {
                SendMessageW(editor.hwnd, 0x00B1, 7, 7);
                SendMessageW(editor.hwnd, WM_IME_STARTCOMPOSITION, 0, 0);
                SendMessageW(editor.hwnd, 0x00C2, 1, wide("ni").as_ptr() as isize);
            }
            assert!(editor.wake.ime_composing.get());
            let mut before = [0i32; 2];
            // SAFETY: Read the owned control's UTF-16 selection into local storage.
            unsafe { SendMessageW(editor.hwnd, WM_USER + 52, 0, before.as_mut_ptr() as isize) };
            for _ in 0..12 {
                editor.sync_edits(&mut model);
                editor.apply_style(2.0, false, &Palette::get(&ctx));
            }
            let mut after = [0i32; 2];
            // SAFETY: Read only the test control's selection and modification bit.
            unsafe { SendMessageW(editor.hwnd, WM_USER + 52, 0, after.as_mut_ptr() as isize) };
            assert_eq!(model, "prefix ");
            assert_eq!(editor.synced, "prefix ");
            assert_eq!(after, before);
            assert_eq!(editor.wake.format_passes.get(), count);
            assert_eq!(editor.style, original_style);
            // SAFETY: Replace only the synthetic provisional span, then deliver
            // the same end notification that follows a native IME commit.
            unsafe {
                SendMessageW(editor.hwnd, 0x00B1, 7, 9);
                SendMessageW(editor.hwnd, 0x00C2, 1, wide("你好").as_ptr() as isize);
                SendMessageW(editor.hwnd, WM_IME_ENDCOMPOSITION, 0, 0);
            }
            editor.sync_edits(&mut model);
            assert_eq!(model, "prefix 你好");
            assert_eq!(editor.wake.format_passes.get(), count + 1);
            assert!(!editor.wake.ime_composing.get());
        }
    }

    #[test]
    fn ime_cancel_and_new_results_do_not_publish_provisional_text() {
        let parent = TestParent::new();
        let mut editors = NativeEditors::new(parent.0 as usize, egui::Context::default()).unwrap();
        for editor in &mut editors.editors {
            let mut model = "stable".to_owned();
            editor.set_text(&model);
            // SAFETY: Synthetic composition and cancellation target our control.
            unsafe {
                SendMessageW(editor.hwnd, 0x00B1, 6, 6);
                SendMessageW(editor.hwnd, WM_IME_STARTCOMPOSITION, 0, 0);
                SendMessageW(editor.hwnd, 0x00C2, 1, wide("n").as_ptr() as isize);
            }
            editor.sync_edits(&mut model);
            assert_eq!(model, "stable");
            // SAFETY: Remove the provisional span before IME end/cancel.
            unsafe {
                SendMessageW(editor.hwnd, 0x00B1, 6, 7);
                SendMessageW(editor.hwnd, WM_CLEAR, 0, 0);
                SendMessageW(editor.hwnd, WM_IME_ENDCOMPOSITION, 0, 0);
            }
            editor.sync_edits(&mut model);
            assert_eq!(model, "stable");
            // A newer worker result still wins over an outstanding edit.
            // SAFETY: Begin composition in this owned hidden fixture.
            unsafe { SendMessageW(editor.hwnd, WM_IME_STARTCOMPOSITION, 0, 0) };
            model = "new OCR".into();
            editor.set_text(&model);
            // SAFETY: A late end notification must not restore old preedit text.
            unsafe { SendMessageW(editor.hwnd, WM_IME_ENDCOMPOSITION, 0, 0) };
            editor.sync_edits(&mut model);
            assert_eq!(model, "new OCR");
        }
    }

    #[test]
    fn ime_context_recovers_from_host_disable_and_remains_attached_across_frames() {
        use windows_sys::Win32::UI::Input::Ime::{
            ImmAssociateContextEx, ImmGetContext, ImmReleaseContext, IACE_CHILDREN,
        };
        let parent = TestParent::new();
        let ctx = egui::Context::default();
        let mut editors = NativeEditors::new(parent.0 as usize, ctx.clone()).unwrap();
        let placement = Placement {
            rect: Rect::from_min_size(egui::pos2(30.0, 45.0), egui::vec2(320.0, 200.0)),
            enabled: true,
            focus: true,
        };
        for index in 0..2 {
            let hwnd = editors.editors[index].hwnd;
            editors.editors[index].place(placement, 1.0, &Palette::get(&ctx));
            // SAFETY: Reproduce winit's IME disable path on our own fixture only.
            unsafe {
                ImmAssociateContextEx(parent.0, null_mut(), IACE_CHILDREN);
                assert!(ImmGetContext(hwnd).is_null());
            }
            for _ in 0..6 {
                editors.editors[index].requested = Some(placement);
                editors.end_frame(&ctx, false);
                assert!(ctx.output(|output| output.ime.is_some()));
                // SAFETY: The acquired context belongs to our focused fixture.
                unsafe {
                    let context = ImmGetContext(hwnd);
                    assert!(!context.is_null());
                    ImmReleaseContext(hwnd, context);
                }
            }
            let state = editors.ime_state(index);
            assert!(!state.0);
            assert_eq!(state.1, 0);
        }
    }

    #[test]
    fn tsf_notifications_guard_only_their_editor_until_composition_actually_ends() {
        let parent = TestParent::new();
        let mut editors = NativeEditors::new(parent.0 as usize, egui::Context::default()).unwrap();
        for index in 0..2 {
            let hwnd = editors.editors[index].hwnd;
            let mut model = String::new();
            let mut event = EndCompositionNotify {
                header: NotifyHeader {
                    hwnd_from: hwnd,
                    id_from: 0x5340 + index,
                    code: EN_STARTCOMPOSITION,
                },
                code: 0,
            };
            // SAFETY: Send real-shaped TSF notifications through our test parent;
            // Windows/RichEdit, not egui, owns the provisional document text.
            unsafe {
                SendMessageW(
                    parent.0,
                    WM_NOTIFY,
                    event.header.id_from,
                    &event as *const _ as isize,
                );
                SendMessageW(hwnd, 0x00C2, 1, wide("ni").as_ptr() as isize);
            }
            let count = editors.editors[index].wake.format_passes.get();
            editors.editors[index].sync_edits(&mut model);
            assert!(model.is_empty());
            assert!(editors.ime_state(index).0);
            assert!(!editors.ime_state(1 - index).0);
            event.header.code = EN_ENDCOMPOSITION;
            event.code = 2; // ECN_NEWTEXT is not ECN_ENDCOMPOSITION.
                            // SAFETY: The synchronous notification points to a live SDK layout.
            unsafe {
                SendMessageW(
                    parent.0,
                    WM_NOTIFY,
                    event.header.id_from,
                    &event as *const _ as isize,
                )
            };
            editors.editors[index].sync_edits(&mut model);
            assert!(editors.ime_state(index).0);
            assert!(model.is_empty());
            assert_eq!(editors.ime_state(index).3, count);
            event.code = 1;
            // SAFETY: Commit the provisional text, then notify only its owner.
            unsafe {
                SendMessageW(hwnd, 0x00B1, 0, -1);
                SendMessageW(hwnd, 0x00C2, 1, wide("你").as_ptr() as isize);
                SendMessageW(
                    parent.0,
                    WM_NOTIFY,
                    event.header.id_from,
                    &event as *const _ as isize,
                );
            }
            editors.editors[index].sync_edits(&mut model);
            assert_eq!(model, "你");
            assert!(!editors.ime_state(index).0);
            assert_eq!(editors.ime_state(index).1, 1);
            assert_eq!(editors.ime_state(index).2, 1);
            assert_eq!(editors.ime_state(index).3, count + 1);
        }
    }

    #[test]
    fn unicode_results_roundtrip_without_reordering_or_normalization() {
        let parent = TestParent::new();
        let mut editors = NativeEditors::new(parent.0 as usize, egui::Context::default()).unwrap();
        let samples = [
            "안녕하세요 你好 日本語",
            "العربية 123 English עברית",
            "हिन्दी क्षि বাংলা தமிழ் తెలుగు ಕನ್ನಡ മലയാളം",
            "ภาษาไทย สวัสดี ສະບາຍດີ ខ្មែរ မြန်မာ",
            "සිංහල བོད་ཡིག ᠮᠣᠩᠭᠣᠯ ქართული հայերեն አማርኛ",
            "e\u{301} a\u{30a} Tiếng Việt Ω Ж 𠀀 😀 👨‍👩‍👧‍👦",
            "abc \u{2067}אבג 123\u{2069} def\u{200d}\u{fe0f}",
            "{\\rtf1 literal OCR text}\\n\\[x^2\\]",
            "first\nsecond\n\nlast\n",
        ];
        for text in samples {
            editors.editors[0].set_text(text);
            assert_eq!(read_text(editors.editors[0].hwnd).as_deref(), Some(text));
        }
    }

    #[test]
    fn keyboard_characters_edit_both_result_models() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, PeekMessageW, PM_REMOVE, WM_LBUTTONDOWN,
        };

        let parent = TestParent::new();
        let ctx = egui::Context::default();
        let mut editors = NativeEditors::new(parent.0 as usize, ctx.clone()).unwrap();
        let mut models = [String::new(), String::new()];
        for (index, model) in models.iter_mut().enumerate() {
            let editor = &mut editors.editors[index];
            editor.place(
                Placement {
                    rect: Rect::from_min_size(egui::pos2(20.0, 20.0), egui::vec2(300.0, 200.0)),
                    enabled: true,
                    focus: true,
                },
                1.0,
                &Palette::get(&ctx),
            );
            // SAFETY: Input and message dispatch are restricted to this fixture's
            // native control; no global input or clipboard is touched.
            unsafe {
                SendMessageW(editor.hwnd, WM_LBUTTONDOWN, 1, 8 | (8 << 16));
                SendMessageW(editor.hwnd, WM_LBUTTONUP, 0, 8 | (8 << 16));
                for character in "Manual 原文译文".encode_utf16() {
                    PostMessageW(editor.hwnd, WM_CHAR, character as usize, 1);
                }
                let mut message = zeroed();
                while PeekMessageW(&mut message, editor.hwnd, WM_CHAR, WM_CHAR, PM_REMOVE) != 0 {
                    DispatchMessageW(&message);
                }
            }
            editor.sync_edits(model);
            assert_eq!(model, "Manual 原文译文", "editor {index}");
        }
    }

    #[test]
    fn busy_readonly_clears_before_keyboard_input_resumes_in_both_editors() {
        let parent = TestParent::new();
        let ctx = egui::Context::default();
        let mut editors = NativeEditors::new(parent.0 as usize, ctx.clone()).unwrap();
        for editor in &mut editors.editors {
            let mut text = String::from("existing");
            editor.set_text(&text);
            let mut placement = Placement {
                rect: Rect::from_min_size(egui::pos2(20.0, 20.0), egui::vec2(300.0, 200.0)),
                enabled: false,
                focus: false,
            };
            editor.place(placement, 1.0, &Palette::get(&ctx));
            // SAFETY: Attempt native keyboard input only in this owned fixture.
            unsafe { SendMessageW(editor.hwnd, WM_CHAR, b'X' as usize, 1) };
            editor.sync_edits(&mut text);
            assert_eq!(text, "existing");

            placement.enabled = true;
            placement.focus = true;
            editor.place(placement, 1.0, &Palette::get(&ctx));
            // SAFETY: Send an ordinary character after the job returns to idle.
            unsafe { SendMessageW(editor.hwnd, WM_CHAR, b'X' as usize, 1) };
            editor.sync_edits(&mut text);
            assert_eq!(text, "Xexisting");

            // Exercise the Unicode selection insertion used by native text paste,
            // without changing or reading the user's system clipboard contents.
            let pasted = wide("粘贴\rالعربية 😀");
            // SAFETY: Replace this fixture's selection with stack-owned UTF-16.
            unsafe {
                SendMessageW(editor.hwnd, 0x00B1, 0, -1);
                SendMessageW(editor.hwnd, 0x00C2, 1, pasted.as_ptr() as isize);
            }
            editor.sync_edits(&mut text);
            assert_eq!(text, "粘贴\nالعربية 😀");
        }
    }

    #[test]
    fn native_edits_sync_but_stale_edits_never_replace_new_ocr() {
        let parent = TestParent::new();
        let mut editors = NativeEditors::new(parent.0 as usize, egui::Context::default()).unwrap();
        let editor = &mut editors.editors[0];
        let mut text = "original\r\n안녕".to_owned();
        editor.set_text(&text);
        editor.sync_edits(&mut text);
        assert_eq!(
            text, "original\r\n안녕",
            "unmodified line endings must remain exact"
        );
        let replacement = wide("العربية\rहिन्दी");
        // SAFETY: Test messages target only its hidden control with valid UTF-16.
        unsafe {
            SendMessageW(editor.hwnd, WM_SETTEXT, 0, replacement.as_ptr() as isize);
            SendMessageW(editor.hwnd, EM_SETMODIFY, 1, 0);
        }
        editor.sync_edits(&mut text);
        assert_eq!(text, "العربية\nहिन्दी");
        text = "new OCR result".to_owned();
        // SAFETY: Set the fixture's modified bit to emulate an obsolete edit.
        unsafe { SendMessageW(editor.hwnd, EM_SETMODIFY, 1, 0) };
        editor.sync_edits(&mut text);
        assert_eq!(text, "new OCR result");
    }

    #[test]
    fn theme_and_dpi_changes_do_not_modify_untouched_unicode_or_line_endings() {
        let parent = TestParent::new();
        let ctx = egui::Context::default();
        let mut editors = NativeEditors::new(parent.0 as usize, ctx.clone()).unwrap();
        let mut source = "مرحبا\r\nहिन्दी e\u{301}\r\n안녕 😀\r\n".to_owned();
        let original = source.clone();
        let editor = &mut editors.editors[0];
        editor.set_text(&source);
        for dark in [false, true, false] {
            ctx.set_visuals(if dark {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            });
            for scale in [1.0, 1.5, 2.0] {
                editor.place(
                    Placement {
                        rect: Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(300.0, 200.0)),
                        enabled: true,
                        focus: false,
                    },
                    scale,
                    &Palette::get(&ctx),
                );
                editor.sync_edits(&mut source);
                assert_eq!(source, original);
            }
        }
    }

    #[test]
    fn long_results_and_selected_unicode_are_not_truncated_or_visually_reordered() {
        let parent = TestParent::new();
        let mut editors = NativeEditors::new(parent.0 as usize, egui::Context::default()).unwrap();
        let editor = &mut editors.editors[0];
        let sample = "안녕하세요 العربية עברית हिन्दी e\u{301} 😀 𠀀\n";
        let long = sample.repeat(3000);
        editor.set_text(&long);
        assert_eq!(read_text(editor.hwnd).as_deref(), Some(long.as_str()));
        editor.set_text(sample.trim_end());
        // EM_GETSELTEXT exercises precisely the logical text range used by native
        // copy without replacing or inspecting the user's real clipboard.
        let mut buffer = vec![0u16; sample.encode_utf16().count() + 1];
        // SAFETY: Select the complete fixture text, then read into a buffer sized
        // for all its UTF-16 units plus terminator, including surrogate pairs.
        let copied = unsafe {
            SendMessageW(editor.hwnd, 0x00B1, 0, -1); // EM_SETSEL
            SendMessageW(editor.hwnd, WM_USER + 62, 0, buffer.as_mut_ptr() as isize)
        };
        assert!(copied > 0);
        assert_eq!(
            String::from_utf16(&buffer[..copied as usize]).unwrap(),
            sample.trim_end()
        );
    }

    #[test]
    fn destroyed_parent_releases_subclass_and_drops_safely() {
        let parent = TestParent::new();
        let editors = NativeEditors::new(parent.0 as usize, egui::Context::default()).unwrap();
        let first = editors.editors[0].wake.clone();
        let second = editors.editors[1].wake.clone();
        assert!(first.attached.get() && second.attached.get());
        drop(parent);
        assert!(!first.attached.get() && !second.attached.get());
        assert!(!editors.wheel_router.attached.get());
        drop(editors);
        assert_eq!(Rc::strong_count(&first), 1);
        assert_eq!(Rc::strong_count(&second), 1);
    }

    #[test]
    #[ignore = "requires the release validation Windows font stack and RichEdit behavior"]
    fn edited_scripts_preserve_caret_and_undo_while_receiving_correct_fonts() {
        let parent = TestParent::new();
        let ctx = egui::Context::default();
        let mut editors = NativeEditors::new(parent.0 as usize, ctx.clone()).unwrap();
        let original = "Original français: é è ç À Œ";
        let addition = "\rعُمان قطر الكويت\rภาษาไทย हिन्दी 한국어";
        editors.editors[1].set_text(original);
        let baseline = editors.editors[1].hwnd;
        let editor = &mut editors.editors[0];
        let mut model = original.to_owned();
        editor.set_text(&model);
        let guard = FormatGuard::new(editor.hwnd);
        assert!(
            guard.frozen && guard.suspended,
            "TOM must batch layout and suspend application formatting undo"
        );
        drop(guard);
        let utf16 = wide(addition);
        let insertion_offset = original.encode_utf16().count();
        let mut before_selection = [0i32; 2];
        // SAFETY: Simulate native user insertion into the owned test document;
        // this never accesses the system clipboard or sends keyboard input.
        unsafe {
            // EM_SETSEL(-1, -1) only deselects; explicitly place both fixtures
            // at their UTF-16 end before testing insertion and formatting.
            for target in [editor.hwnd, baseline] {
                SendMessageW(target, 0x00B1, insertion_offset, insertion_offset as isize);
                let mut selection = [0i32; 2];
                SendMessageW(target, WM_USER + 52, 0, selection.as_mut_ptr() as isize);
                assert_eq!(selection, [insertion_offset as i32; 2]);
            }
            SendMessageW(editor.hwnd, 0x00C2, 1, utf16.as_ptr() as isize);
            SendMessageW(baseline, 0x00C2, 1, utf16.as_ptr() as isize);
            SendMessageW(
                editor.hwnd,
                WM_USER + 52,
                0,
                before_selection.as_mut_ptr() as isize,
            );
        }
        editor.sync_edits(&mut model);
        assert_eq!(model, format!("{original}{}", addition.replace('\r', "\n")));
        let mut after_selection = [0i32; 2];
        // SAFETY: Read the fixture's selection immediately after app formatting.
        unsafe {
            SendMessageW(
                editor.hwnd,
                WM_USER + 52,
                0,
                after_selection.as_mut_ptr() as isize,
            )
        };
        assert_eq!(
            after_selection, before_selection,
            "formatting must not move the editing caret"
        );
        for (start, end, expected) in native_font_runs::font_runs(&native_line_endings(&model)) {
            // SAFETY: Read only the exact SDK-sized character format for this run.
            unsafe {
                let mut format: CharFormatW = zeroed();
                format.size = size_of::<CharFormatW>() as u32;
                SendMessageW(editor.hwnd, 0x00B1, start as usize, end as isize);
                SendMessageW(editor.hwnd, WM_USER + 58, 1, &mut format as *mut _ as isize);
                let end = format.face.iter().position(|c| *c == 0).unwrap_or(32);
                assert_eq!(String::from_utf16(&format.face[..end]).unwrap(), expected);
            }
        }
        // RichEdit can split programmatic EM_REPLACESEL at script boundaries;
        // compare one native Undo with the same unformatted baseline first.
        // SAFETY: Undo targets only the two owned fixture documents.
        unsafe {
            SendMessageW(editor.hwnd, 0x00C7, 0, 0);
            SendMessageW(baseline, 0x00C7, 0, 0);
        }
        let expected = read_text(baseline).unwrap();
        assert_eq!(read_text(editor.hwnd).unwrap(), expected);
        editor.sync_edits(&mut model);
        assert_eq!(model, expected, "Undo must also reach the Rust OCR model");
        let original = model.clone();
        let inserted = wide("X");
        // SAFETY: Emulate one ordinary character edit at the beginning of the
        // fixture, then undo it after app formatting has processed the change.
        unsafe {
            SendMessageW(editor.hwnd, 0x00B1, 0, 0);
            SendMessageW(editor.hwnd, 0x00C2, 1, inserted.as_ptr() as isize);
        }
        editor.sync_edits(&mut model);
        assert_eq!(model, format!("X{original}"));
        // SAFETY: Undo modifies only this test-created native editor.
        unsafe { SendMessageW(editor.hwnd, 0x00C7, 0, 0) };
        editor.sync_edits(&mut model);
        assert_eq!(read_text(editor.hwnd).unwrap(), original);
        assert_eq!(
            model, original,
            "Undo must restore the original Rust model too"
        );
    }

    #[test]
    #[ignore = "requires a stable interactive desktop frame; run on the release validation image"]
    fn native_rendering_paints_shaped_text_without_using_the_clipboard() {
        let parent = TestParent::new();
        let ctx = egui::Context::default();
        let mut editors = NativeEditors::new(parent.0 as usize, ctx.clone()).unwrap();
        let editor = &mut editors.editors[0];
        editor.set_text("العربية עברית हिन्दी ภาษาไทย 안녕하세요");
        editor.place(
            Placement {
                rect: Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(700.0, 130.0)),
                enabled: true,
                focus: false,
            },
            1.0,
            &Palette::get(&ctx),
        );
        let pixels = print_client(editor.hwnd, 700, 130).unwrap();
        let background = Palette::get(&ctx).panel;
        assert!(
            pixels.iter().filter(|p| **p == background).count() > 1_000,
            "the private bitmap must contain the editor background, not an empty DC"
        );
        assert!(
            pixels.iter().filter(|p| **p != background).count() > 150,
            "RichEdit must actually paint its multilingual glyphs into the private bitmap"
        );
        editor.hide();
        assert!(!editor.visible);
        assert!(!editors.is_focused(0));
        let mut image = ColorImage::new([800, 600], Color32::GREEN);
        assert!(editors.composite_screenshot(&mut image).unwrap());
        assert!(image.pixels.iter().all(|p| *p == Color32::GREEN));
    }

    #[test]
    #[ignore = "requires a stable interactive desktop frame; run on the release validation image"]
    fn multilingual_children_render_independently_at_nonzero_offsets() {
        let parent = TestParent::new();
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let mut editors = NativeEditors::new(parent.0 as usize, ctx.clone()).unwrap();
        let text = "العربية: السلام عليكم 123\nفارسی: سلام دنیا ۱۲۳\nעברית: שלום עולם 123\nहिन्दी: नमस्ते दुनिया क्ष त्र ज्ञ\nภาษาไทย: สวัสดีชาวโลก\nភាសាខ្មែរ: សួស្តីពិភពលោក\nᠮᠣᠩᠭᠣᠯ\nTiếng Việt: Nguyễn Trường\nবাংলা: নমস্কার বিশ্ব\nதமிழ்: வணக்கம் உலகம்\n한국어: 안녕하세요\n中文、日本語、Українська: Ґ Ї Є\nMixed: العربية 123 中文 English";
        let p = Palette::get(&ctx);
        let mut composite = ColorImage::new([1050, 750], p.panel);
        for (index, editor) in editors.editors.iter_mut().enumerate() {
            editor.set_text(text);
            editor.place(
                Placement {
                    rect: Rect::from_min_size(
                        egui::pos2(20.0 + index as f32 * 520.0, 20.0),
                        egui::vec2(500.0, 700.0),
                    ),
                    enabled: true,
                    focus: false,
                },
                1.0,
                &p,
            );
            let pixels = print_client(editor.hwnd, 500, 700).unwrap();
            if index == 0 {
                let native = native_line_endings(text);
                let start = native[..native.find('ᠮ').unwrap()].encode_utf16().count();
                let mut a = windows_sys::Win32::Foundation::POINTL { x: 0, y: 0 };
                let mut b = windows_sys::Win32::Foundation::POINTL { x: 0, y: 0 };
                // SAFETY: Read-only geometry for the test fixture's Mongolian run.
                unsafe {
                    SendMessageW(
                        editor.hwnd,
                        WM_USER + 38,
                        &mut a as *mut _ as usize,
                        start as isize,
                    );
                    SendMessageW(
                        editor.hwnd,
                        WM_USER + 38,
                        &mut b as *mut _ as usize,
                        (start + 6) as isize,
                    );
                }
                assert!(
                    b.x - a.x >= 20,
                    "Mongolian must retain its nominal font size"
                );
                let ink: Vec<_> = (a.y as usize..a.y as usize + 20)
                    .filter(|y| {
                        (a.x as usize..b.x as usize).any(|x| pixels[*y * 500 + x].r() > 150)
                    })
                    .collect();
                assert!(
                    ink.last().unwrap() - ink.first().unwrap() + 1 >= 8,
                    "Mongolian ink must not collapse after other script runs"
                );
            }
            assert!(
                pixels.iter().filter(|c| **c == p.panel).count() > 1000,
                "child {index} must paint its background at nonzero parent offset"
            );
            assert!(
                pixels.iter().filter(|c| **c != p.panel).count() > 500,
                "child {index} must paint text"
            );
        }
        assert!(!editors.composite_screenshot(&mut composite).unwrap());
        dispatch_snapshot_messages();
        assert!(editors.composite_screenshot(&mut composite).unwrap());
        let bytes: Vec<u8> = composite.pixels.iter().flat_map(|p| p.to_array()).collect();
        image::save_buffer(
            "target/native-multilingual-regression.png",
            &bytes,
            1050,
            750,
            image::ColorType::Rgba8,
        )
        .unwrap();
    }

    #[test]
    #[ignore = "requires a stable interactive desktop frame; run on the release validation image"]
    fn complex_script_caret_positions_follow_native_shaping() {
        use windows_sys::Win32::Foundation::POINTL;
        fn caret(hwnd: HWND, index: isize) -> POINTL {
            let mut point = POINTL { x: 0, y: 0 };
            // SAFETY: RichEdit 4.1 writes the requested logical UTF-16 character
            // position to a valid local POINTL; no clipboard or user input involved.
            unsafe { SendMessageW(hwnd, WM_USER + 38, &mut point as *mut _ as usize, index) };
            point
        }
        let parent = TestParent::new();
        let ctx = egui::Context::default();
        let mut editors = NativeEditors::new(parent.0 as usize, ctx.clone()).unwrap();
        let editor = &mut editors.editors[0];
        editor.place(
            Placement {
                rect: Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(700.0, 200.0)),
                enabled: true,
                focus: false,
            },
            1.0,
            &Palette::get(&ctx),
        );
        editor.set_text("مرحبا");
        let _ = print_client(editor.hwnd, 700, 200);
        let start = caret(editor.hwnd, 0);
        let next = caret(editor.hwnd, 1);
        assert!(
            start.x > next.x,
            "Arabic logical indices must advance right to left: {} -> {}",
            start.x,
            next.x
        );
        assert_eq!(start.y, next.y);
        editor.set_text("e\u{301}X");
        let base = caret(editor.hwnd, 0);
        let accent = caret(editor.hwnd, 1);
        let after = caret(editor.hwnd, 2);
        assert_eq!(base.y, accent.y);
        assert!(after.x > base.x, "accented grapheme has a positive advance");
        assert!(
            accent.x <= after.x,
            "combining mark must stay inside its base cluster"
        );
        // A scalar-only renderer places the preceding-i matra after the consonant;
        // the Windows shaper handles glyph reordering while retaining logical text.
        editor.set_text("कि क्षि हिन्दी");
        assert_eq!(read_text(editor.hwnd).unwrap(), "कि क्षि हिन्दी");
        assert_eq!(caret(editor.hwnd, 0).y, caret(editor.hwnd, 9).y);
        editor.set_text("한");
        let _ = print_client(editor.hwnd, 700, 200).unwrap();
        assert!(caret(editor.hwnd, 1).x > caret(editor.hwnd, 0).x);
        // Modern decomposed Jamo remain visible as separate characters in the
        // Windows shaper. Do not normalize OCR/copy data to satisfy a glyph test.
        // https://learn.microsoft.com/typography/script-development/hangul
        editor.set_text("한");
        let _ = print_client(editor.hwnd, 700, 200).unwrap();
        let actual_width = caret(editor.hwnd, 3).x - caret(editor.hwnd, 0).x;
        assert_eq!(
            read_text(editor.hwnd).unwrap(),
            "한",
            "Jamo data remains decomposed"
        );
        assert!(actual_width > 0, "decomposed Jamo must remain visible");
    }

    #[test]
    fn paragraph_direction_follows_first_strong_unicode_character() {
        assert!(paragraph_is_rtl("123 البحرين English"));
        assert!(paragraph_is_rtl("עברית English"));
        assert!(paragraph_is_rtl("123 𞤀𞤣𞤤𞤢𞤥"));
        assert!(!paragraph_is_rtl("English العربية"));
        assert!(!paragraph_is_rtl("Français, français العربية"));
        assert!(!paragraph_is_rtl("中文 العربية"));
        assert!(!paragraph_is_rtl("123 !?"));
        assert!(!paragraph_is_rtl(""));
    }

    #[test]
    fn script_formats_survive_theme_and_document_replacements() {
        let literal = |name: &str| {
            let prefix = format!("const {name}: &str = ");
            let line = include_str!("../ui_smoke.rs")
                .lines()
                .find_map(|line| line.strip_prefix(&prefix))
                .unwrap();
            serde_json::from_str::<String>(line.trim_end_matches(';')).unwrap()
        };
        let countries = literal("COUNTRY_SOURCE");
        let multilingual = literal("MULTILINGUAL_SOURCE");
        let parent = TestParent::new();
        let ctx = egui::Context::default();
        let mut editors = NativeEditors::new(parent.0 as usize, ctx.clone()).unwrap();
        for (stage, texts, dark) in [
            ("main", ["안녕하세요", ""], false),
            ("korean-dark", ["안녕하세요", ""], true),
            ("multi", [multilingual.as_str(), ""], false),
            (
                "multi-dark",
                [multilingual.as_str(), multilingual.as_str()],
                true,
            ),
            ("countries", [countries.as_str(), ""], false),
            (
                "countries-dark",
                [countries.as_str(), countries.as_str()],
                true,
            ),
        ] {
            ctx.set_visuals(if dark {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            });
            let p = Palette::get(&ctx);
            for (index, editor) in editors.editors.iter_mut().enumerate() {
                editor.set_text(texts[index]);
                editor.place(
                    Placement {
                        rect: Rect::from_min_size(
                            egui::pos2(20.0, 100.0),
                            egui::vec2(600.0, 500.0),
                        ),
                        enabled: true,
                        focus: false,
                    },
                    1.5,
                    &p,
                );
                let pixels = print_client(editor.hwnd, 900, 750).unwrap();
                assert!(pixels.contains(&p.panel));
                assert_eq!(read_text(editor.hwnd).unwrap(), texts[index]);
                let native = native_line_endings(texts[index]);
                for sample in ["الأردن", "ᠪᠢᠴᠢᠭ"] {
                    if let Some(pos) = native.find(sample) {
                        let start = native[..pos].encode_utf16().count();
                        for offset in start..start + sample.encode_utf16().count() {
                            // SAFETY: SDK POD query buffer initialized before use.
                            let mut cf: CharFormatW = unsafe { zeroed() };
                            cf.size = size_of::<CharFormatW>() as u32;
                            // SAFETY: Query only the test-owned child's character range.
                            unsafe {
                                SendMessageW(editor.hwnd, 0xB1, offset, (offset + 1) as isize);
                                SendMessageW(
                                    editor.hwnd,
                                    WM_USER + 58,
                                    1,
                                    &mut cf as *mut _ as isize,
                                );
                            }
                            assert_eq!(
                                cf.text_color,
                                color_ref(p.text),
                                "{stage} {index} {sample} {offset} color"
                            );
                            assert_eq!(
                                cf.height,
                                editor.style.unwrap().0,
                                "{stage} {index} {sample} {offset} height"
                            );
                            assert_eq!(cf.effects, 0, "{stage} {index} {sample} {offset} effects");
                        }
                    }
                }
                // SAFETY: Restore this test-owned editor's selection to its start.
                unsafe {
                    SendMessageW(editor.hwnd, 0xB1, 0, 0);
                }
            }
        }
    }

    fn dispatch_snapshot_messages() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, PeekMessageW, PM_REMOVE,
        };
        // SAFETY: Drain only our private test snapshot messages on this thread.
        unsafe {
            let mut message = zeroed();
            while PeekMessageW(
                &mut message,
                null_mut(),
                SNAPSHOT_MESSAGE,
                SNAPSHOT_MESSAGE,
                PM_REMOVE,
            ) != 0
            {
                DispatchMessageW(&message);
            }
        }
    }

    #[test]
    fn asynchronous_snapshot_rejects_stale_text_and_keeps_framebuffer_until_ready() {
        let parent = TestParent::new();
        let ctx = egui::Context::default();
        let mut editors = NativeEditors::new(parent.0 as usize, ctx.clone()).unwrap();
        editors.editors[0].set_text("Jordan · الأردن");
        editors.editors[0].place(
            Placement {
                rect: Rect::from_min_size(egui::pos2(20.0, 20.0), egui::vec2(500.0, 200.0)),
                enabled: true,
                focus: false,
            },
            1.0,
            &Palette::get(&ctx),
        );
        let mut image = ColorImage::new([600, 300], Color32::GREEN);
        assert!(!editors.composite_screenshot(&mut image).unwrap());
        assert!(image.pixels.iter().all(|pixel| *pixel == Color32::GREEN));
        editors.editors[0].set_text("المملكة العربية السعودية");
        dispatch_snapshot_messages();
        assert!(
            !editors.composite_screenshot(&mut image).unwrap(),
            "a snapshot queued before replacement cannot be reused"
        );
        dispatch_snapshot_messages();
        assert!(editors.composite_screenshot(&mut image).unwrap());
        assert_eq!(
            read_text(editors.editors[0].hwnd).unwrap(),
            "المملكة العربية السعودية"
        );
        assert!(image.pixels.iter().any(|pixel| *pixel != Color32::GREEN));
    }

    #[test]
    fn late_frame_redraw_is_coalesced_and_never_revives_hidden_editor() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, PeekMessageW, PM_REMOVE,
        };
        let parent = TestParent::new();
        let ctx = egui::Context::default();
        let mut editors = NativeEditors::new(parent.0 as usize, ctx.clone()).unwrap();
        let editor = &mut editors.editors[0];
        editor.set_text("Recognition completed · الأردن 안녕하세요");
        editor.place(
            Placement {
                rect: Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(500.0, 200.0)),
                enabled: true,
                focus: false,
            },
            1.0,
            &Palette::get(&ctx),
        );
        editor.queue_redraw_after_frame();
        editor.queue_redraw_after_frame();
        assert!(editor.wake.redraw_pending.get());
        editor.hide();
        // SAFETY: Dispatch only this owned editor's private queued frame message.
        unsafe {
            let mut message = zeroed();
            assert_ne!(
                PeekMessageW(
                    &mut message,
                    editor.hwnd,
                    REDRAW_MESSAGE,
                    REDRAW_MESSAGE,
                    PM_REMOVE
                ),
                0
            );
            DispatchMessageW(&message);
            assert_eq!(
                PeekMessageW(
                    &mut message,
                    editor.hwnd,
                    REDRAW_MESSAGE,
                    REDRAW_MESSAGE,
                    PM_REMOVE
                ),
                0
            );
            assert_eq!(IsWindowVisible(editor.hwnd), 0);
        }
        assert!(!editor.visible);
        assert!(!editor.wake.redraw_pending.get());
    }

    #[test]
    fn popup_cutouts_round_outward_and_clip_to_the_child_at_fractional_dpi() {
        let popup = Rect::from_min_max(egui::pos2(49.5, 29.75), egui::pos2(150.1, 100.2));
        assert_eq!(
            popup_cutouts(&[popup, popup], [75, 45, 120, 80], 1.5),
            vec![[0, 0, 120, 80]]
        );
        let popup = Rect::from_min_max(egui::pos2(70.1, 50.1), egui::pos2(99.9, 79.9));
        assert_eq!(
            popup_cutouts(&[popup], [75, 45, 120, 80], 1.5),
            vec![[30, 30, 75, 75]]
        );
        assert!(popup_cutouts(&[popup], [500, 500, 120, 80], 1.5).is_empty());
    }

    #[test]
    fn unknown_popup_uses_the_safe_hidden_fallback() {
        let ctx = egui::Context::default();
        assert_eq!(active_popup_rects(&ctx), Some(Vec::new()));
        ctx.memory_mut(|memory| memory.open_popup(egui::Id::new("unmeasured-popup")));
        assert!(active_popup_rects(&ctx).is_none());
        ctx.memory_mut(|memory| memory.close_popup());
        assert_eq!(active_popup_rects(&ctx), Some(Vec::new()));
    }

    #[test]
    #[ignore = "requires a stable interactive desktop frame; run on the release validation image"]
    fn native_popup_hole_preserves_text_and_restores_the_full_window_region() {
        use windows_sys::Win32::Graphics::Gdi::{GetWindowRgn, PtInRegion};
        let parent = TestParent::new();
        let ctx = egui::Context::default();
        let mut editors = NativeEditors::new(parent.0 as usize, ctx.clone()).unwrap();
        let editor = &mut editors.editors[0];
        editor.set_text("الأردن · 안녕하세요 · हिन्दी");
        editor.place(
            Placement {
                rect: Rect::from_min_size(egui::pos2(20.0, 20.0), egui::vec2(400.0, 200.0)),
                enabled: true,
                focus: false,
            },
            1.0,
            &Palette::get(&ctx),
        );
        let popup = Rect::from_min_max(egui::pos2(220.0, 20.0), egui::pos2(420.0, 120.0));
        assert!(editor.apply_occlusion(&[popup], 1.0));
        assert!(editor.visible);
        assert!(!editor.fully_occluded);
        assert_eq!(editor.cutouts, vec![[200, 0, 400, 100]]);
        // SAFETY: Query only the test-owned child's region into our owned temporary.
        unsafe {
            let region = CreateRectRgn(0, 0, 0, 0);
            assert_ne!(GetWindowRgn(editor.hwnd, region), 0);
            assert_ne!(PtInRegion(region, 10, 10), 0);
            assert_eq!(PtInRegion(region, 250, 50), 0);
            DeleteObject(region);
        }
        let mut image = ColorImage::new([450, 250], Color32::GREEN);
        assert!(!editors.composite_screenshot(&mut image).unwrap());
        dispatch_snapshot_messages();
        assert!(editors.composite_screenshot(&mut image).unwrap());
        assert_eq!(
            image[(270, 70)],
            Color32::GREEN,
            "native composition must preserve popup pixels"
        );
        assert_ne!(
            image[(30, 30)],
            Color32::GREEN,
            "uncovered body remains native text"
        );
        let editor = &mut editors.editors[0];
        assert!(editor.apply_occlusion(&[], 1.0));
        assert!(editor.cutouts.is_empty());
        assert!(!editor.fully_occluded);
        assert_eq!(
            read_text(editor.hwnd).unwrap(),
            "الأردن · 안녕하세요 · हिन्दी"
        );
        // SAFETY: A removed custom region is reported as ERROR/zero, not a hole.
        unsafe {
            let region = CreateRectRgn(0, 0, 0, 0);
            assert_eq!(GetWindowRgn(editor.hwnd, region), 0);
            DeleteObject(region);
        }
        assert!(
            !editors.composite_screenshot(&mut image).unwrap(),
            "restoring the region invalidates the cropped snapshot"
        );
        dispatch_snapshot_messages();
        assert!(editors.composite_screenshot(&mut image).unwrap());
        assert_ne!(image[(270, 70)], Color32::GREEN);
    }

    #[test]
    #[ignore = "requires a stable interactive desktop frame; run on the release validation image"]
    fn opening_a_popup_returns_keyboard_focus_without_changing_editor_selection() {
        let parent = TestParent::new();
        let ctx = egui::Context::default();
        let mut editors = NativeEditors::new(parent.0 as usize, ctx.clone()).unwrap();
        let text = "Country · الأردن · 안녕하세요";
        let placement = Placement {
            rect: Rect::from_min_size(egui::pos2(20.0, 20.0), egui::vec2(400.0, 200.0)),
            enabled: true,
            focus: true,
        };
        editors.editors[0].set_text(text);
        editors.editors[0].place(placement, 1.0, &Palette::get(&ctx));
        // SAFETY: Select only UTF-16 text in this test-owned RichEdit child.
        unsafe {
            SendMessageW(editors.editors[0].hwnd, 0xB1, 10, 16);
        }
        assert!(editors.is_focused(0));
        let popup = egui::Id::new("native-focus-popup");
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            ctx.memory_mut(|memory| memory.open_popup(popup));
            egui::Area::new(popup)
                .order(egui::Order::Foreground)
                .fixed_pos(egui::pos2(250.0, 30.0))
                .show(ctx, |ui| {
                    ui.label("English / العربية");
                });
            editors.editors[0].requested = Some(placement);
            editors.end_frame(ctx, false);
        });
        assert!(!editors.popup_rects().is_empty());
        assert!(editors.editors[0].visible);
        assert!(!editors.is_focused(0));
        let mut selection = [0i32; 2];
        // SAFETY: Read this same child's logical selection into a sized local buffer.
        unsafe {
            SendMessageW(
                editors.editors[0].hwnd,
                WM_USER + 52,
                0,
                selection.as_mut_ptr() as isize,
            );
        }
        assert_eq!(selection, [10, 16]);
        assert_eq!(read_text(editors.editors[0].hwnd).unwrap(), text);
    }

    #[test]
    fn stable_frames_do_not_repaint_native_text_and_changes_repaint_once() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, PeekMessageW, PM_REMOVE,
        };
        fn finish_frame(editors: &mut NativeEditors, ctx: &egui::Context, placement: Placement) {
            editors.editors[0].requested = Some(placement);
            editors.end_frame(ctx, false);
            // SAFETY: Dispatch only our fixture's queued rendering messages.
            unsafe {
                let mut message = zeroed();
                while PeekMessageW(
                    &mut message,
                    editors.editors[0].hwnd,
                    REDRAW_MESSAGE,
                    REDRAW_MESSAGE,
                    PM_REMOVE,
                ) != 0
                {
                    DispatchMessageW(&message);
                }
            }
        }
        let parent = TestParent::new();
        let ctx = egui::Context::default();
        let mut editors = NativeEditors::new(parent.0 as usize, ctx.clone()).unwrap();
        let mut placement = Placement {
            rect: Rect::from_min_size(egui::pos2(20.0, 20.0), egui::vec2(400.0, 200.0)),
            enabled: true,
            focus: false,
        };
        editors.editors[0].set_text("한국어 العربية हिन्दी 中文");
        finish_frame(&mut editors, &ctx, placement);
        assert_eq!(editors.native_redraw_requests(), [1, 0]);
        for _ in 0..120 {
            finish_frame(&mut editors, &ctx, placement);
        }
        assert_eq!(
            editors.native_redraw_requests(),
            [1, 0],
            "stable parent frames must not erase/redraw child text"
        );
        editors.editors[0].set_text("New · 안녕하세요 الأردن हिन्दी");
        finish_frame(&mut editors, &ctx, placement);
        assert_eq!(editors.native_redraw_requests(), [2, 0]);
        ctx.set_visuals(egui::Visuals::light());
        finish_frame(&mut editors, &ctx, placement);
        let before = editors.native_redraw_requests()[0];
        placement.enabled = false;
        finish_frame(&mut editors, &ctx, placement);
        assert!(editors.editors[0].read_only);
        assert_eq!(editors.native_redraw_requests()[0], before + 1);
        for _ in 0..120 {
            finish_frame(&mut editors, &ctx, placement);
        }
        assert_eq!(editors.native_redraw_requests()[0], before + 1);
        placement.rect = placement.rect.translate(egui::vec2(5.0, 5.0));
        finish_frame(&mut editors, &ctx, placement);
        assert_eq!(editors.native_redraw_requests()[0], before + 2);
        editors.editors[0].hide();
        finish_frame(&mut editors, &ctx, placement);
        assert_eq!(editors.native_redraw_requests()[0], before + 3);
    }

    #[test]
    fn wheel_coordinates_preserve_negative_monitor_positions() {
        for (x, y) in [(-1920, -240), (127, -1080), (32767, -32768)] {
            let packed = (x as u16 as usize | ((y as u16 as usize) << 16)) as isize;
            let point = wheel_screen_point(packed);
            assert_eq!((point.x, point.y), (x, y));
        }
    }

    #[test]
    fn wheel_routing_does_not_deliver_to_hidden_editors_and_nested_delivery_is_once() {
        let parent = TestParent::new();
        let ctx = egui::Context::default();
        let mut editors = NativeEditors::new(parent.0 as usize, ctx.clone()).unwrap();
        let text = (0..100)
            .map(|line| format!("{line}: العربية 한국어 中文\n"))
            .collect::<String>();
        editors.editors[0].set_text(&text);
        editors.editors[0].place(
            Placement {
                rect: Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 150.0)),
                enabled: false,
                focus: false,
            },
            1.0,
            &Palette::get(&ctx),
        );
        let wheel = (-120i16 as u16 as usize) << 16;
        let outside = (-30000i16 as u16 as usize | ((-30000i16 as u16 as usize) << 16)) as isize;
        assert!(editors.wheel_router.target_at(outside).is_none());
        // SAFETY: Both messages stay within the test-owned parent/hidden children.
        unsafe {
            SendMessageW(parent.0, WM_MOUSEWHEEL, wheel, outside);
            SendMessageW(editors.editors[0].hwnd, WM_MOUSEWHEEL, wheel, outside);
        }
        assert_eq!(editors.wheel_delivery_counts(), [0, 0]);
        let before = editors.scroll_positions();
        let hwnd = editors.editors[0].hwnd;
        {
            // Emulate the protected inner delivery from our parent router. A
            // native control bubbling back to its parent cannot be routed twice.
            let _guard = WheelDispatchGuard::new(&editors.wheel_router.dispatching);
            assert!(editors
                .wheel_router
                .forward(parent.0, WM_MOUSEWHEEL, wheel, outside)
                .is_none());
            // SAFETY: Deliver one wheel event to the owned native fixture only.
            unsafe {
                SendMessageW(hwnd, WM_MOUSEWHEEL, wheel, 10 | (10 << 16));
            }
        }
        assert!(!editors.wheel_router.dispatching.get());
        assert_eq!(editors.wheel_delivery_counts(), [1, 0]);
        assert_eq!(editors.scroll_positions()[1], before[1]);
        assert!(
            editors.editors[0].read_only,
            "routing does not change editability"
        );
        assert_eq!(read_text(hwnd).unwrap(), text);
        editors.wheel_router.uninstall();
        assert!(!editors.wheel_router.attached.get());
    }
}
