//! Explicit native tests: they only address windows and hotkeys created by this test process.
use super::*;
use std::time::{Duration, Instant};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThreadId, GetGuiResources, GR_GDIOBJECTS, GR_USEROBJECTS,
};

struct CursorBitmaps(ICONINFO);
impl Drop for CursorBitmaps {
    fn drop(&mut self) {
        // SAFETY: GetIconInfo gives the caller owned bitmap copies, never selected into a DC.
        unsafe {
            if !self.0.hbmMask.is_null() {
                DeleteObject(self.0.hbmMask);
            }
            if !self.0.hbmColor.is_null() {
                DeleteObject(self.0.hbmColor);
            }
        }
    }
}

#[test]
fn native_tray_menu_has_current_shortcut_labels_and_action_order() -> Result<()> {
    // Create and inspect an unshown menu; no tray icon, input, or global hotkey is touched.
    let labels = ["Ctrl+Shift+O".into(), "Alt+F8".into(), "F4".into()];
    let menu = TrayMenu::new(&labels)?;
    // SAFETY: The menu handle is owned and live throughout these read-only queries.
    assert_eq!(unsafe { GetMenuItemCount(menu.0) }, 7);
    let expected_ids = [1, 7, 2, 3, 4, 5, 6];
    for (position, (id, expected)) in tray_items(&labels).into_iter().enumerate() {
        assert_eq!(id, expected_ids[position]);
        // SAFETY: Every queried position is within the seven-item owned menu.
        assert_eq!(unsafe { GetMenuItemID(menu.0, position as i32) }, id as u32);
        let mut text = [0u16; 128];
        // SAFETY: The menu position exists and text is writable for the passed element count.
        let copied = unsafe {
            GetMenuStringW(
                menu.0,
                position as u32,
                text.as_mut_ptr(),
                text.len() as i32,
                MF_BYPOSITION,
            )
        };
        assert!(copied > 0);
        assert_eq!(String::from_utf16(&text[..copied as usize])?, expected);
    }
    let handle = menu.0;
    drop(menu);
    // SAFETY: IsMenu accepts a released opaque handle and only checks whether it is still valid.
    assert_eq!(unsafe { IsMenu(handle) }, 0, "tray menu survived its guard");
    Ok(())
}

#[test]
fn native_cursor_preserves_png_dimensions_hotspot_and_transparency() -> Result<()> {
    let image = image::load_from_memory(native_cursor::CAPTURE_PNG)?.into_rgba8();
    let cursor = native_cursor::CaptureCursor::load()?;
    // SAFETY: ICONINFO is POD. The guard also releases any bitmap copies on assertion failure.
    let mut info = CursorBitmaps(unsafe { zeroed() });
    // SAFETY: The cursor is live and info.0 is writable storage for the copied metadata.
    assert_ne!(unsafe { GetIconInfo(cursor.handle(), &mut info.0) }, 0);
    assert_eq!(info.0.fIcon, 0, "PNG must create a cursor, not an icon");
    assert_eq!(info.0.xHotspot, image.width() / 2);
    assert_eq!(info.0.yHotspot, image.height() / 2);
    // SAFETY: BITMAP is POD and GetObjectW initializes this writable metadata structure.
    let mut bitmap: BITMAP = unsafe { zeroed() };
    assert_ne!(
        // SAFETY: GetIconInfo returned a live bitmap copy; destination has the specified size.
        unsafe {
            GetObjectW(
                info.0.hbmColor,
                size_of::<BITMAP>() as i32,
                (&mut bitmap as *mut BITMAP).cast(),
            )
        },
        0
    );
    assert_eq!(bitmap.bmWidth, image.width() as i32);
    assert_eq!(bitmap.bmHeight, image.height() as i32);
    assert_eq!(bitmap.bmBitsPixel, 32);

    // Draw only into an off-screen memory DIB, never onto the desktop or another window.
    // SAFETY: The desktop DC supplies compatibility information only; ScreenDc releases it.
    let screen = ScreenDc(unsafe { GetDC(null_mut()) });
    if screen.0.is_null() {
        return Err(win_error("创建光标测试设备上下文失败"));
    }
    let surface = BitmapSurface::new(screen.0, image.width() as i32, image.height() as i32)?;
    drop(screen);
    // BGRA channels expose transparency and channel-order errors.
    let background = [23u8, 57, 91];
    // SAFETY: The unaliased DIB allocation covers surface.length bytes and outlives this borrow.
    let pixels = unsafe { std::slice::from_raw_parts_mut(surface.pixels, surface.length) };
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.copy_from_slice(&[background[0], background[1], background[2], 255]);
    }
    assert_ne!(
        // SAFETY: The memory DC, cursor and dimensions are valid; no brush or external HWND is used.
        unsafe {
            DrawIconEx(
                surface.dc,
                0,
                0,
                cursor.handle(),
                image.width() as i32,
                image.height() as i32,
                0,
                null_mut(),
                DI_NORMAL,
            )
        },
        0
    );
    // SAFETY: Complete this thread's GDI writes before inspecting the owned DIB memory.
    unsafe { GdiFlush() };
    assert!(image.pixels().any(|pixel| pixel[3] == 0));
    assert!(image.pixels().any(|pixel| pixel[3] != 0));
    for (position, (actual, source)) in surface
        .bytes()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(image.pixels())
        .enumerate()
    {
        for (channel, original) in [source[2], source[1], source[0]].into_iter().enumerate() {
            let alpha = source[3] as u32;
            let expected =
                (original as u32 * alpha + background[channel] as u32 * (255 - alpha) + 127) / 255;
            assert!(
                (actual[channel] as i32 - expected as i32).abs() <= 1,
                "cursor pixel {position}, channel {channel}: {} != {expected}",
                actual[channel]
            );
        }
    }
    Ok(())
}

struct ReservedHotkey(i32);
impl ReservedHotkey {
    fn acquire(id: i32, text: &str) -> Result<Self> {
        let key = parse_hotkey(text)?;
        // SAFETY: The test owns this thread-local hotkey ID and validates its key combination.
        if unsafe { RegisterHotKey(null_mut(), id, key.modifiers, key.key) } == 0 {
            return Err(win_error("测试快捷键无法注册（可能被其他程序使用）"));
        }
        Ok(Self(id))
    }
}
impl Drop for ReservedHotkey {
    fn drop(&mut self) {
        // SAFETY: The guard is dropped on the same test thread that registered this ID.
        unsafe { UnregisterHotKey(null_mut(), self.0) };
    }
}

#[test]
#[ignore = "temporarily registers global hotkeys; run explicitly on an interactive Windows desktop"]
fn native_hotkey_collision_rollback_and_shutdown() -> Result<()> {
    let prefix = "Ctrl+Alt+Shift+";
    let first = format!("{prefix}F23");
    let second = format!("{prefix}F24");
    let silent = format!("{prefix}F21");
    let blocked = format!("{prefix}F22");
    let replacement = format!("{prefix}F20");
    let _blocker = ReservedHotkey::acquire(0x4100, &blocked)?;
    let (sender, events) = mpsc::channel();
    let platform = Platform::start(&first, &second, &silent, true, sender)?;
    for event in events.try_iter() {
        if let PlatformEvent::Error(message) = event {
            bail!("{message}");
        }
    }
    assert!(!platform.tray_visible());
    let window = platform.hwnd as HWND;
    // SAFETY: IsWindow accepts this opaque HWND without dereferencing any Rust memory.
    assert_ne!(unsafe { IsWindow(window) }, 0);
    // SAFETY: This posts a scalar notification only to the test-owned platform
    // window; initial registration assigns the silent action ID 3.
    assert_ne!(unsafe { PostMessageW(window, WM_HOTKEY, 3, 0) }, 0);
    assert_eq!(
        events.recv_timeout(Duration::from_secs(2))?,
        PlatformEvent::SilentOcr
    );

    // F20 is provisionally acquired, then the third action fails on F22.
    // All three old keys must remain registered and the provisional key released.
    assert!(platform
        .update(&replacement, &second, &blocked, true)
        .is_err());
    assert!(ReservedHotkey::acquire(0x4101, &first).is_err());
    assert!(ReservedHotkey::acquire(0x4101, &second).is_err());
    assert!(ReservedHotkey::acquire(0x4101, &silent).is_err());
    drop(ReservedHotkey::acquire(0x4101, &replacement)?);

    // Reusing an existing combination for the other action must also work natively.
    platform.update(&silent, &first, &second, true)?;
    // The same registered combination keeps ID 3 but now performs ordinary OCR.
    // SAFETY: The destination remains the same live window owned by this test.
    assert_ne!(unsafe { PostMessageW(window, WM_HOTKEY, 3, 0) }, 0);
    assert_eq!(
        events.recv_timeout(Duration::from_secs(2))?,
        PlatformEvent::Ocr
    );
    platform.update(&replacement, &first, &silent, true)?;
    drop(ReservedHotkey::acquire(0x4101, &second)?);
    assert!(ReservedHotkey::acquire(0x4101, &replacement).is_err());
    drop(platform);
    // SAFETY: IsWindow explicitly permits checking a handle after its window was destroyed.
    assert_eq!(unsafe { IsWindow(window) }, 0);
    drop(ReservedHotkey::acquire(0x4101, &first)?);
    drop(ReservedHotkey::acquire(0x4101, &replacement)?);
    drop(ReservedHotkey::acquire(0x4101, &silent)?);
    Ok(())
}

unsafe extern "system" fn find_test_capture_window(hwnd: HWND, parameter: LPARAM) -> BOOL {
    let mut name = [0u16; 128];
    let count = GetClassNameW(hwnd, name.as_mut_ptr(), name.len() as i32);
    // Comparing UTF-16 avoids allocations or panics inside the enumeration callback.
    if count > 0
        && (count as usize) < name.len()
        && name[..count as usize]
            .iter()
            .copied()
            .eq(CAPTURE_CLASS.encode_utf16())
    {
        *(parameter as *mut HWND) = hwnd;
        0
    } else {
        1
    }
}

struct CaptureTestWorker {
    id: u32,
    handle: Option<JoinHandle<()>>,
}
impl Drop for CaptureTestWorker {
    fn drop(&mut self) {
        // SAFETY: This is the thread created by this test, not another process's UI thread.
        // WM_QUIT provides a cleanup fallback if an assertion fires before Escape is sent.
        unsafe { PostThreadMessageW(self.id, WM_QUIT, 0, 0) };
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn cancel_native_capture_once() -> Result<()> {
    let (started, thread_id) = mpsc::sync_channel(1);
    let (finished, output) = mpsc::sync_channel(1);
    let handle = thread::Builder::new()
        .name("capture-integration-test".into())
        .spawn(move || {
            // SAFETY: GetCurrentThreadId has no pointer arguments and identifies this new thread.
            let _ = started.send(unsafe { GetCurrentThreadId() });
            let _ = finished.send(capture_region().map_err(|error| error.to_string()));
        })?;
    let worker = CaptureTestWorker {
        id: thread_id.recv()?,
        handle: Some(handle),
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut found = false;
    loop {
        let mut window: HWND = null_mut();
        // SAFETY: Only this test's capture thread is enumerated. The callback writes our stack HWND.
        unsafe {
            EnumThreadWindows(
                worker.id,
                Some(find_test_capture_window),
                (&mut window as *mut HWND) as LPARAM,
            )
        };
        if !window.is_null() {
            found = true;
            // SAFETY: This known HWND has our capture class on our capture thread; no input is sent elsewhere.
            unsafe { PostMessageW(window, WM_KEYDOWN, VK_ESCAPE as usize, 0) };
        }
        match output.try_recv() {
            Ok(result) => {
                assert!(found, "capture ended before its overlay was observed");
                assert!(result.map_err(|message| anyhow!(message))?.is_none());
                break;
            }
            Err(mpsc::TryRecvError::Disconnected) => bail!("截图测试线程意外退出"),
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if Instant::now() >= deadline {
            bail!("截图窗口未在 10 秒内响应取消");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let id = worker.id;
    drop(worker);
    let mut remaining: HWND = null_mut();
    // SAFETY: The enumeration callback only writes our initialized out-parameter.
    unsafe {
        EnumThreadWindows(
            id,
            Some(find_test_capture_window),
            (&mut remaining as *mut HWND) as LPARAM,
        )
    };
    assert!(
        remaining.is_null(),
        "capture overlay survived its worker thread"
    );
    Ok(())
}

#[test]
#[ignore = "briefly shows then cancels its own capture overlay; run explicitly on an interactive Windows desktop"]
fn native_capture_escape_releases_windows_and_gdi_objects() -> Result<()> {
    set_dpi_awareness();
    // Warm up the registered class and native PNG decoder before measuring handles.
    cancel_native_capture_once()?;
    // SAFETY: The pseudo-handle identifies this test process; GetGuiResources only reads its counters.
    let before = unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) };
    // SAFETY: This only reads the current process's USER handle counter, including cursors.
    let user_before = unsafe { GetGuiResources(GetCurrentProcess(), GR_USEROBJECTS) };
    cancel_native_capture_once()?;
    // SAFETY: The process pseudo-handle is always valid and must not be closed.
    let after = unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) };
    // SAFETY: This process pseudo-handle is valid; the counter includes nonshared cursor objects.
    let user_after = unsafe { GetGuiResources(GetCurrentProcess(), GR_USEROBJECTS) };
    assert_eq!(before, after, "a capture leaked GDI resources");
    assert_eq!(
        user_before, user_after,
        "a capture leaked USER/cursor resources"
    );
    Ok(())
}
