//! Clipboard writes that offer plain text and HTML together, so a spreadsheet
//! or word processor can pick the richer form and everything else the text.

use anyhow::{bail, Result};
use std::time::Duration;
use windows_sys::Win32::{
    Foundation::{GlobalFree, HANDLE, HWND},
    System::{
        DataExchange::{
            CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW,
            SetClipboardData,
        },
        Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
    },
};

const CF_UNICODETEXT: u32 = 13;

/// Replace the clipboard with `text` (CF_UNICODETEXT) and an HTML fragment
/// ("HTML Format"). Nothing is changed when the clipboard cannot be opened.
pub fn copy_text_and_html(hwnd: usize, text: &str, html_fragment: &str) -> Result<()> {
    let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let mut html = cf_html(html_fragment).into_bytes();
    html.push(0);
    let name: Vec<u16> = "HTML Format".encode_utf16().chain([0]).collect();
    // SAFETY: `name` is a live NUL-terminated UTF-16 string.
    let html_format = unsafe { RegisterClipboardFormatW(name.as_ptr()) };
    if html_format == 0 {
        bail!("无法注册 HTML 剪贴板格式");
    }
    let _clipboard = Clipboard::open(hwnd as HWND)?;
    // SAFETY: the clipboard is open by this thread (guard above).
    if unsafe { EmptyClipboard() } == 0 {
        bail!("无法清空剪贴板");
    }
    set(CF_UNICODETEXT, bytes_of(&wide))?;
    set(html_format, &html)
}

struct Clipboard;

impl Clipboard {
    fn open(owner: HWND) -> Result<Self> {
        // Another program may hold the clipboard for a moment.
        for _ in 0..10 {
            // SAFETY: `owner` is our window or null; OpenClipboard validates it.
            if unsafe { OpenClipboard(owner) } != 0 {
                return Ok(Self);
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        bail!("剪贴板正被其他程序占用")
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        // SAFETY: the guard exists only while this thread holds the clipboard.
        unsafe { CloseClipboard() };
    }
}

fn bytes_of(wide: &[u16]) -> &[u8] {
    // SAFETY: u16 has no padding; the byte view covers exactly the same memory.
    unsafe { std::slice::from_raw_parts(wide.as_ptr().cast(), std::mem::size_of_val(wide)) }
}

/// Copy `data` into movable global memory and hand it to the clipboard.
fn set(format: u32, data: &[u8]) -> Result<()> {
    // SAFETY: plain allocation; a null result is checked below.
    let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, data.len()) };
    if memory.is_null() {
        bail!("剪贴板内存不足");
    }
    // SAFETY: `memory` is a live movable block of `data.len()` bytes.
    let target = unsafe { GlobalLock(memory) };
    if target.is_null() {
        // SAFETY: the block is still ours and unlocked.
        unsafe { GlobalFree(memory) };
        bail!("无法锁定剪贴板内存");
    }
    // SAFETY: `target` points to at least `data.len()` writable bytes that do
    // not overlap `data`; the block is unlocked before ownership passes on.
    unsafe {
        std::ptr::copy_nonoverlapping(data.as_ptr(), target.cast::<u8>(), data.len());
        GlobalUnlock(memory);
    }
    // SAFETY: the clipboard is open and emptied by this thread; on success the
    // system owns `memory`, on failure it is still ours to free.
    if unsafe { SetClipboardData(format, memory as HANDLE) }.is_null() {
        // SAFETY: SetClipboardData failed, so ownership did not pass on.
        unsafe { GlobalFree(memory) };
        bail!("无法写入剪贴板");
    }
    Ok(())
}

/// Wrap a fragment in the CF_HTML header, whose offsets count UTF-8 bytes.
fn cf_html(fragment: &str) -> String {
    const PREFIX: &str = "<html><head><meta charset=\"utf-8\"></head><body><!--StartFragment-->";
    const SUFFIX: &str = "<!--EndFragment--></body></html>";
    let header = |start_html: usize, end_html: usize, start: usize, end: usize| {
        format!(
            "Version:0.9\r\nStartHTML:{start_html:010}\r\nEndHTML:{end_html:010}\r\n\
             StartFragment:{start:010}\r\nEndFragment:{end:010}\r\n"
        )
    };
    let start_html = header(0, 0, 0, 0).len();
    let start = start_html + PREFIX.len();
    let end = start + fragment.len();
    let end_html = end + SUFFIX.len();
    format!(
        "{}{PREFIX}{fragment}{SUFFIX}",
        header(start_html, end_html, start, end)
    )
}

#[cfg(test)]
mod tests {
    use super::cf_html;

    #[test]
    fn cf_html_offsets_point_at_the_fragment() {
        let fragment = "<table><tr><td>合并</td></tr></table>";
        let html = cf_html(fragment);
        let offset = |key: &str| -> usize {
            let start = html.find(key).unwrap() + key.len();
            html[start..start + 10].parse().unwrap()
        };
        assert_eq!(
            &html[offset("StartFragment:")..offset("EndFragment:")],
            fragment
        );
        assert!(html[offset("StartHTML:")..].starts_with("<html>"));
        assert_eq!(offset("EndHTML:"), html.len());
    }
}
