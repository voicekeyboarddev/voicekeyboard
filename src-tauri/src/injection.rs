use crate::{context, settings::Settings, types::Action};
use anyhow::{anyhow, bail};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

#[derive(Clone)]
pub struct InjectionAbort {
    flag: Arc<AtomicBool>,
}

impl InjectionAbort {
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn abort(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    pub fn reset(&self) {
        self.flag.store(false, Ordering::SeqCst);
    }

    fn is_aborted(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

pub fn inject(
    actions: &[Action],
    settings: &Settings,
    abort: &InjectionAbort,
) -> anyhow::Result<()> {
    if settings.dry_run {
        return Ok(());
    }
    abort.reset();
    let start_cursor = context::cursor_position();
    for action in actions {
        check_abort(settings, abort, start_cursor)?;
        match action {
            Action::Text { value } => send_text(value)?,
            Action::Shortcut { keys } => send_shortcut(keys)?,
        }
        thread::sleep(Duration::from_millis(settings.injection_delay_ms));
    }
    Ok(())
}

pub fn begin_streaming_injection() -> Option<(i32, i32)> {
    context::cursor_position()
}

pub fn inject_text_chunk(
    text: &str,
    settings: &Settings,
    abort: &InjectionAbort,
    start_cursor: Option<(i32, i32)>,
) -> anyhow::Result<()> {
    if settings.dry_run || text.is_empty() {
        return Ok(());
    }
    check_abort(settings, abort, start_cursor)?;
    send_text(text)
}

pub fn dismiss_context_menu_soon() {
    #[cfg(windows)]
    {
        thread::spawn(|| {
            for delay in [45_u64, 90, 140, 220] {
                thread::sleep(Duration::from_millis(delay));
                if context::foreground_window_is_native_menu() {
                    let _ = send_shortcut(&["Escape".to_string()]);
                    break;
                }
            }
        });
    }
}

fn check_abort(
    settings: &Settings,
    abort: &InjectionAbort,
    start_cursor: Option<(i32, i32)>,
) -> anyhow::Result<()> {
    if settings.kill_switch_enabled && abort.is_aborted() {
        bail!("injection aborted by Escape");
    }
    if settings.kill_switch_enabled {
        if let (Some(start), Some(now)) = (start_cursor, context::cursor_position()) {
            let dx = (now.0 - start.0) as f64;
            let dy = (now.1 - start.1) as f64;
            if (dx * dx + dy * dy).sqrt() > settings.movement_tolerance_px * 2.0 {
                bail!("injection aborted by cursor movement");
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn send_text(text: &str) -> anyhow::Result<()> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
        KEYEVENTF_UNICODE, VIRTUAL_KEY,
    };

    let mut inputs = Vec::new();
    for unit in text.encode_utf16() {
        inputs.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: unit,
                    dwFlags: KEYEVENTF_UNICODE,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        });
        inputs.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: unit,
                    dwFlags: KEYBD_EVENT_FLAGS(KEYEVENTF_UNICODE.0 | KEYEVENTF_KEYUP.0),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        });
    }

    send_inputs(&inputs)
}

#[cfg(windows)]
fn send_shortcut(keys: &[String]) -> anyhow::Result<()> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
    };

    let mut vks = Vec::new();
    for key in keys {
        vks.push(vk_for_key(key).ok_or_else(|| anyhow!("unsupported key: {key}"))?);
    }
    let mut inputs = Vec::new();
    for vk in &vks {
        inputs.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: *vk,
                    wScan: 0,
                    dwFlags: Default::default(),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        });
    }
    for vk in vks.iter().rev() {
        inputs.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: *vk,
                    wScan: 0,
                    dwFlags: KEYEVENTF_KEYUP,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        });
    }
    send_inputs(&inputs)
}

#[cfg(windows)]
fn send_inputs(
    inputs: &[windows::Win32::UI::Input::KeyboardAndMouse::INPUT],
) -> anyhow::Result<()> {
    use std::mem::size_of;
    use windows::Win32::UI::Input::KeyboardAndMouse::SendInput;
    let sent = unsafe {
        SendInput(
            inputs,
            size_of::<windows::Win32::UI::Input::KeyboardAndMouse::INPUT>() as i32,
        )
    };
    if sent == inputs.len() as u32 {
        Ok(())
    } else {
        bail!("SendInput sent {sent}/{} events", inputs.len())
    }
}

#[cfg(windows)]
fn vk_for_key(key: &str) -> Option<windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY> {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    let normalized = key.trim().to_ascii_lowercase();
    Some(match normalized.as_str() {
        "ctrl" | "control" => VK_CONTROL,
        "shift" => VK_SHIFT,
        "alt" => VK_MENU,
        "win" | "windows" | "cmd" | "command" | "meta" => VK_LWIN,
        "enter" | "return" => VK_RETURN,
        "tab" => VK_TAB,
        "escape" | "esc" => VK_ESCAPE,
        "space" => VK_SPACE,
        "backspace" => VK_BACK,
        "delete" | "del" => VK_DELETE,
        "insert" | "ins" => VK_INSERT,
        "left" => VK_LEFT,
        "right" => VK_RIGHT,
        "up" => VK_UP,
        "down" => VK_DOWN,
        "home" => VK_HOME,
        "end" => VK_END,
        "pageup" => VK_PRIOR,
        "pagedown" => VK_NEXT,
        "f1" => VK_F1,
        "f2" => VK_F2,
        "f3" => VK_F3,
        "f4" => VK_F4,
        "f5" => VK_F5,
        "f6" => VK_F6,
        "f7" => VK_F7,
        "f8" => VK_F8,
        "f9" => VK_F9,
        "f10" => VK_F10,
        "f11" => VK_F11,
        "f12" => VK_F12,
        single if single.len() == 1 => {
            let ch = single.chars().next()?.to_ascii_uppercase();
            VIRTUAL_KEY(ch as u16)
        }
        _ => return None,
    })
}

#[cfg(not(windows))]
fn send_text(_text: &str) -> anyhow::Result<()> {
    bail!("text injection is not implemented for this platform yet")
}

#[cfg(not(windows))]
fn send_shortcut(_keys: &[String]) -> anyhow::Result<()> {
    bail!("shortcut injection is not implemented for this platform yet")
}

pub fn send_undo() -> anyhow::Result<()> {
    send_shortcut(&["Ctrl".to_string(), "Z".to_string()])
}
