//! Debug-only evidence of the pixels already displayed by our own foreground UI.
//!
//! Unlike WM_PRINT/PrintWindow, this does not ask a control to draw again. Reading
//! its client display DC makes GL-over-child and stale-frame bugs visible in the
//! smoke result instead of replacing them with a freshly rendered ideal image.

#![cfg(any(debug_assertions, test))]

use anyhow::{ensure, Context, Result};
use eframe::egui::{Color32, ColorImage};
use std::{mem::size_of, ptr::null_mut};
use windows_sys::Win32::{
    Foundation::{HWND, RECT},
    Graphics::{
        Dwm::DwmFlush,
        Gdi::{
            BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, GetDC,
            ReleaseDC, SelectObject, BITMAPINFO, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
            SRCCOPY,
        },
    },
    UI::{
        HiDpi::{
            SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT,
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        },
        WindowsAndMessaging::{
            GetClientRect, GetForegroundWindow, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
        },
    },
};

pub(super) fn capture(hwnd: usize) -> Result<ColorImage> {
    let hwnd = hwnd as HWND;
    ensure_owned_visible_foreground(hwnd)?;
    // Client coordinates and the display DC must both use physical pixels even
    // when a test runner inherited a different thread DPI awareness context.
    let _dpi = DpiContext::physical_pixels()?;
    // Synchronize already-submitted desktop composition; never invalidate,
    // UpdateWindow, send WM_PAINT, or otherwise repair the frame being inspected.
    // SAFETY: DwmFlush only waits for the calling process's composition work.
    unsafe { DwmFlush() };
    ensure_owned_visible_foreground(hwnd)?;
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    ensure!(
        // SAFETY: hwnd was verified as this process's existing, visible window.
        unsafe { GetClientRect(hwnd, &mut rect) } != 0,
        "无法读取本程序客户区尺寸"
    );
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    ensure!(width > 0 && height > 0, "本程序客户区不可见或尺寸为空");
    let count = (width as usize)
        .checked_mul(height as usize)
        .context("本程序客户区尺寸溢出")?;
    ensure!(count <= 64 * 1024 * 1024, "本程序客户区超过截图大小限制");
    // GetDC(hwnd), not GetDC(NULL), restricts the source to this known client
    // display DC. Neither the desktop nor a different window is ever requested.
    let source = WindowDc {
        hwnd,
        // SAFETY: The matching RAII owner releases this DC with the same hwnd.
        dc: unsafe { GetDC(hwnd) },
    };
    ensure!(!source.dc.is_null(), "无法读取本程序已显示的客户区");
    let surface = Surface::new(source.dc, width, height)?;
    // SAFETY: Initialize only our private destination storage. Any untransferred
    // pixels stay black; no source window is cleared, invalidated or repainted.
    unsafe { std::ptr::write_bytes(surface.pixels as *mut u8, 0, count * 4) };
    ensure_owned_visible_foreground(hwnd)?;
    ensure!(
        // SAFETY: Both live DCs and the selected bitmap belong to this operation.
        // The source origin is exactly the verified window's client origin; SRCCOPY
        // reads existing pixels without invoking window painting or other programs.
        unsafe { BitBlt(surface.dc, 0, 0, width, height, source.dc, 0, 0, SRCCOPY) } != 0,
        "无法复制本程序已显示的客户区像素"
    );
    // SAFETY: Complete this thread's GDI transfer before reading its DIB memory.
    unsafe { GdiFlush() };
    // If the user switched to another program while composition/copy completed,
    // discard the pixels instead of returning a potentially obscured result.
    ensure_owned_visible_foreground(hwnd)?;
    // SAFETY: The top-down 32-bit DIB owns count * 4 bytes until surface drops.
    let bytes = unsafe { std::slice::from_raw_parts(surface.pixels, count * 4) };
    let pixels = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| Color32::from_rgb(pixel[2], pixel[1], pixel[0]))
        .collect();
    // Preserve even a wholly black or stale frame: that is valid diagnostic
    // evidence, not a reason to substitute freshly rendered control contents.
    Ok(ColorImage {
        size: [width as usize, height as usize],
        pixels,
    })
}

fn ensure_owned_visible_foreground(hwnd: HWND) -> Result<()> {
    ensure!(!hwnd.is_null(), "未指定本程序窗口");
    let mut owner = 0;
    // SAFETY: GetWindowThreadProcessId validates the opaque HWND and only writes
    // the supplied local process id. No pixels or external window text are read.
    unsafe { GetWindowThreadProcessId(hwnd, &mut owner) };
    ensure!(owner == std::process::id(), "只允许截取本程序拥有的窗口");
    ensure!(
        // SAFETY: These calls only inspect visibility for the verified owned window.
        unsafe { IsWindowVisible(hwnd) != 0 && IsIconic(hwnd) == 0 },
        "本程序窗口尚未可见或已最小化"
    );
    let mut foreground_owner = 0;
    // SAFETY: Only the foreground HWND's ownership is queried; no foreign pixels
    // or text are retrieved. The target itself is never activated or moved.
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), &mut foreground_owner) };
    ensure!(
        foreground_owner == std::process::id(),
        "真实显示检查需要本程序保持前台"
    );
    Ok(())
}

struct DpiContext(DPI_AWARENESS_CONTEXT);

impl DpiContext {
    fn physical_pixels() -> Result<Self> {
        // SAFETY: Changes only the calling thread and is restored by Drop.
        let previous =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        ensure!(!previous.is_null(), "无法统一真实显示截图的 DPI 坐标");
        Ok(Self(previous))
    }
}

impl Drop for DpiContext {
    fn drop(&mut self) {
        // SAFETY: Restores the valid context returned by this same API/thread.
        unsafe { SetThreadDpiAwarenessContext(self.0) };
    }
}

struct WindowDc {
    hwnd: HWND,
    dc: HDC,
}

impl Drop for WindowDc {
    fn drop(&mut self) {
        if !self.dc.is_null() {
            // SAFETY: This owner releases exactly its GetDC(hwnd) acquisition.
            unsafe { ReleaseDC(self.hwnd, self.dc) };
        }
    }
}

struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    pixels: *const u8,
}

impl Surface {
    fn new(source: HDC, width: i32, height: i32) -> Result<Self> {
        // SAFETY: BITMAPINFO is SDK POD; all fields are zeroed before initialization.
        let mut info: BITMAPINFO = unsafe { std::mem::zeroed() };
        info.bmiHeader.biSize =
            size_of::<windows_sys::Win32::Graphics::Gdi::BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = width;
        info.bmiHeader.biHeight = -height;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;
        // SAFETY: Creates a private memory DC compatible with the owned window DC.
        let dc = unsafe { CreateCompatibleDC(source) };
        ensure!(!dc.is_null(), "无法创建真实显示截图缓冲区");
        let mut data = null_mut();
        // SAFETY: The initialized descriptor allocates a top-down BGRA surface.
        let bitmap =
            unsafe { CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut data, null_mut(), 0) };
        if bitmap.is_null() || data.is_null() {
            // SAFETY: The memory DC has not selected any owned GDI object yet.
            unsafe {
                if !bitmap.is_null() {
                    DeleteObject(bitmap);
                }
                DeleteDC(dc);
            }
            anyhow::bail!("无法分配真实显示截图像素缓冲区");
        }
        // SAFETY: The bitmap and DC were allocated by this operation.
        let previous = unsafe { SelectObject(dc, bitmap) };
        if previous.is_null() || previous as isize == -1 {
            // SAFETY: Selection failed, so the bitmap is not selected into dc.
            unsafe {
                DeleteObject(bitmap);
                DeleteDC(dc);
            }
            anyhow::bail!("无法连接真实显示截图缓冲区");
        }
        Ok(Self {
            dc,
            bitmap,
            previous,
            pixels: data.cast(),
        })
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        // SAFETY: Restore the DC's original object before deleting our bitmap/DC.
        unsafe {
            SelectObject(self.dc, self.previous);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_missing_or_foreign_window_before_acquiring_pixels() {
        assert!(capture(0).is_err());
        // SAFETY: Reads only the desktop's system HWND. capture rejects its PID
        // before any DC is acquired, so no desktop pixels are accessed.
        let desktop = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetDesktopWindow() };
        assert!(capture(desktop as usize).is_err());
    }
}
