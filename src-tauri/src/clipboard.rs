// Lightweight clipboard helpers used to capture the user's currently-selected text
// when UIA does not expose it (e.g., webpage selections in Edge / Chrome that live on a
// different element from the keyboard-focused one).
//
// Strategy: write a sentinel string to the clipboard, send Ctrl+C to the foreground app,
// wait briefly, read back the clipboard. If the value is unchanged from the sentinel, the
// app had nothing to copy. Restore the original clipboard on the way out so the user
// doesn't notice.

use std::time::Duration;

#[cfg(windows)]
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{HANDLE, HWND},
        System::{
            DataExchange::{
                CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
            },
            Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
            Ole::CF_UNICODETEXT,
        },
        UI::Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_C,
            VK_CONTROL,
        },
    },
};

const SENTINEL: &str = "\u{0001}__VKB_CLIPBOARD_PROBE__\u{0001}";

#[cfg(windows)]
fn read_clipboard_text() -> Option<String> {
    unsafe {
        for _ in 0..6 {
            if OpenClipboard(HWND(std::ptr::null_mut())).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(8));
        }
        let handle = match GetClipboardData(CF_UNICODETEXT.0 as u32) {
            Ok(h) => h,
            Err(_) => {
                let _ = CloseClipboard();
                return None;
            }
        };
        if handle.0.is_null() {
            let _ = CloseClipboard();
            return None;
        }
        let hglobal = windows::Win32::Foundation::HGLOBAL(handle.0 as *mut _);
        let ptr = GlobalLock(hglobal) as *const u16;
        if ptr.is_null() {
            let _ = CloseClipboard();
            return None;
        }
        // Read until null terminator, cap at 64K chars
        let mut len = 0usize;
        while len < 65536 && *ptr.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(ptr, len);
        let text = String::from_utf16_lossy(slice);
        let _ = GlobalUnlock(hglobal);
        let _ = CloseClipboard();
        Some(text)
    }
}

#[cfg(windows)]
fn write_clipboard_text(text: &str) -> bool {
    unsafe {
        for _ in 0..6 {
            if OpenClipboard(HWND(std::ptr::null_mut())).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(8));
        }
        if EmptyClipboard().is_err() {
            let _ = CloseClipboard();
            return false;
        }

        // Encode as UTF-16 with null terminator
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        wide.push(0);
        let bytes = wide.len() * std::mem::size_of::<u16>();

        let hmem = match GlobalAlloc(GMEM_MOVEABLE, bytes) {
            Ok(h) => h,
            Err(_) => {
                let _ = CloseClipboard();
                return false;
            }
        };
        let dest = GlobalLock(hmem) as *mut u16;
        if dest.is_null() {
            let _ = CloseClipboard();
            return false;
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), dest, wide.len());
        let _ = GlobalUnlock(hmem);
        let handle = HANDLE(hmem.0 as *mut _);
        let _ = SetClipboardData(CF_UNICODETEXT.0 as u32, handle);
        let _ = CloseClipboard();
        // Note: ownership of hmem now belongs to the clipboard; do not free.
        // PCWSTR is a no-op here since we already copied via raw pointers.
        let _ = PCWSTR::null();
        true
    }
}

#[cfg(windows)]
fn empty_clipboard() {
    unsafe {
        for _ in 0..6 {
            if OpenClipboard(HWND(std::ptr::null_mut())).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(8));
        }
        let _ = EmptyClipboard();
        let _ = CloseClipboard();
    }
}

#[cfg(windows)]
fn send_ctrl_c() {
    unsafe {
        let mut inputs: [INPUT; 4] = std::mem::zeroed();
        let make = |vk: u16, up: bool| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(vk),
                    wScan: 0,
                    dwFlags: if up {
                        KEYEVENTF_KEYUP
                    } else {
                        Default::default()
                    },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        inputs[0] = make(VK_CONTROL.0, false);
        inputs[1] = make(VK_C.0, false);
        inputs[2] = make(VK_C.0, true);
        inputs[3] = make(VK_CONTROL.0, true);
        let _ = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

/// Probe the foreground app for selected text via the clipboard.
/// Returns Some(text) only if the app actually copied something different from our sentinel
/// (meaning a real selection existed). Restores the original clipboard either way.
#[cfg(windows)]
pub fn capture_selection_via_clipboard() -> Option<String> {
    let saved = read_clipboard_text();

    // Write sentinel so we can detect "no selection / Ctrl+C did nothing".
    if !write_clipboard_text(SENTINEL) {
        return None;
    }
    std::thread::sleep(Duration::from_millis(15));

    send_ctrl_c();
    // Allow the foreground app a small window to process Ctrl+C and update the clipboard.
    std::thread::sleep(Duration::from_millis(70));

    let result = read_clipboard_text();

    // Restore whatever the user had on their clipboard.
    if let Some(prev) = saved {
        write_clipboard_text(&prev);
    } else {
        empty_clipboard();
    }

    match result {
        Some(text) if text != SENTINEL && !text.is_empty() => Some(text),
        _ => None,
    }
}

#[cfg(not(windows))]
pub fn capture_selection_via_clipboard() -> Option<String> {
    None
}
