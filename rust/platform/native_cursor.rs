//! The capture cursor is a native cursor built from the project's original PNG.
//!
//! PNG cursor resources contain two little-endian hotspot coordinates before the PNG.
//! Let Windows decode the alpha channel instead of creating temporary GDI bitmaps.
//! https://devblogs.microsoft.com/oldnewthing/20231025-00/?p=108925

use super::win_error;
use anyhow::{ensure, Context, Result};
use image::GenericImageView;
use std::ptr::null_mut;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateIconFromResourceEx, DestroyCursor, GetCursor, LoadCursorW, SetCursor, HCURSOR, IDC_ARROW,
};

pub(super) const CAPTURE_PNG: &[u8] = include_bytes!("../../assets/cursor/cross.png");

pub(super) struct CaptureCursor(HCURSOR);

impl CaptureCursor {
    pub(super) fn load() -> Result<Self> {
        let image = image::load_from_memory_with_format(CAPTURE_PNG, image::ImageFormat::Png)
            .context("读取截图光标失败")?;
        let (width, height) = image.dimensions();
        ensure!(
            width > 0 && height > 0 && width <= i16::MAX as u32 && height <= i16::MAX as u32,
            "截图光标尺寸无效"
        );
        let mut resource = Vec::with_capacity(CAPTURE_PNG.len() + 4);
        resource.extend_from_slice(&((width / 2) as u16).to_le_bytes());
        resource.extend_from_slice(&((height / 2) as u16).to_le_bytes());
        resource.extend_from_slice(CAPTURE_PNG);
        // SAFETY: The buffer is a complete RT_CURSOR image (hotspot + PNG), valid for
        // its supplied byte length. Windows copies it synchronously; LR_SHARED is absent,
        // so the resulting cursor is exclusively owned and released by this guard.
        let handle = unsafe {
            CreateIconFromResourceEx(
                resource.as_mut_ptr(),
                resource.len() as u32,
                0,
                0x0003_0000,
                width as i32,
                height as i32,
                0,
            )
        };
        if handle.is_null() {
            return Err(win_error("创建截图光标失败"));
        }
        Ok(Self(handle))
    }

    pub(super) fn activate(&self) {
        // SAFETY: The owned cursor remains live throughout this capture window's lifetime.
        unsafe { SetCursor(self.0) };
    }

    #[cfg(test)]
    pub(super) fn handle(&self) -> HCURSOR {
        self.0
    }
}

impl Drop for CaptureCursor {
    fn drop(&mut self) {
        // SAFETY: No window class retains this handle. Capture destroys its overlay first;
        // if the cursor is still selected, remove it before destroying our nonshared handle.
        // LoadCursor returns a shared system arrow, whose ownership stays with Windows.
        unsafe {
            if GetCursor() == self.0 {
                SetCursor(LoadCursorW(null_mut(), IDC_ARROW));
            }
            DestroyCursor(self.0);
        }
    }
}
