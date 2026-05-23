use crate::{
    settings::Settings,
    types::{CursorScreenshot, FocusedTextContext, WindowContext},
};

const MIN_SCREENSHOT_SIDE: u32 = 64;
const MAX_SCREENSHOT_SIDE: u32 = 768;
const ALWAYS_SEND_IMAGE_MAX_WIDTH: u32 = 640;
const ALWAYS_SEND_IMAGE_MAX_HEIGHT: u32 = 480;

#[derive(Debug, Clone, Copy)]
struct ScreenshotConfig {
    width: u32,
    height: u32,
}

impl ScreenshotConfig {
    fn from_settings(settings: &Settings) -> Self {
        let max_width = if settings.always_send_low_res_image {
            ALWAYS_SEND_IMAGE_MAX_WIDTH
        } else {
            MAX_SCREENSHOT_SIDE
        };
        let max_height = if settings.always_send_low_res_image {
            ALWAYS_SEND_IMAGE_MAX_HEIGHT
        } else {
            MAX_SCREENSHOT_SIDE
        };
        Self {
            width: settings.image_width.clamp(MIN_SCREENSHOT_SIDE, max_width),
            height: settings.image_height.clamp(MIN_SCREENSHOT_SIDE, max_height),
        }
    }
}

#[cfg(windows)]
pub fn active_window_context() -> Option<WindowContext> {
    unsafe {
        let hwnd = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        let mut point = windows::Win32::Foundation::POINT::default();
        let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut point);
        window_context_from_hwnd(hwnd, point.x, point.y)
    }
}

#[cfg(not(windows))]
pub fn active_window_context() -> Option<WindowContext> {
    None
}

pub fn active_window_context_with_screenshot_settings(
    settings: &Settings,
) -> Option<WindowContext> {
    let (x, y) = cursor_position()?;
    window_context_at_point_with_settings(x, y, settings)
}

#[cfg(windows)]
pub fn window_context_at_point_with_settings(
    x: i32,
    y: i32,
    settings: &Settings,
) -> Option<WindowContext> {
    window_context_at_point_with_config(x, y, ScreenshotConfig::from_settings(settings))
}

#[cfg(windows)]
fn window_context_at_point_with_config(
    x: i32,
    y: i32,
    screenshot: ScreenshotConfig,
) -> Option<WindowContext> {
    use windows::Win32::{
        Foundation::POINT,
        UI::WindowsAndMessaging::{GetAncestor, WindowFromPoint, GA_ROOT},
    };

    unsafe {
        let hwnd = WindowFromPoint(POINT { x, y });
        let root = if hwnd.0.is_null() {
            None
        } else {
            let ancestor = GetAncestor(hwnd, GA_ROOT);
            Some(if ancestor.0.is_null() { hwnd } else { ancestor })
        };
        if let Some(hwnd) = root {
            if let Some(mut context) = window_context_from_hwnd(hwnd, x, y) {
                context.focused_text = focused_text_context(6000);
                context.cursor_screenshot = capture_window_screenshot(hwnd, x, y, screenshot)
                    .or_else(|| {
                        capture_cursor_screenshot(screenshot.width, screenshot.height, x, y)
                    });
                return Some(context);
            }
        }
        let mut context = active_window_context()?;
        context.focused_text = focused_text_context(6000);
        context.cursor_screenshot =
            capture_cursor_screenshot(screenshot.width, screenshot.height, x, y);
        Some(context)
    }
}

#[cfg(not(windows))]
pub fn window_context_at_point_with_settings(
    _x: i32,
    _y: i32,
    _settings: &Settings,
) -> Option<WindowContext> {
    None
}

#[cfg(windows)]
pub fn cursor_position() -> Option<(i32, i32)> {
    unsafe {
        let mut point = windows::Win32::Foundation::POINT::default();
        windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut point)
            .is_ok()
            .then_some((point.x, point.y))
    }
}

#[cfg(not(windows))]
pub fn cursor_position() -> Option<(i32, i32)> {
    None
}

#[cfg(windows)]
pub fn left_mouse_button_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0 }
}

#[cfg(not(windows))]
pub fn left_mouse_button_down() -> bool {
    false
}

#[cfg(windows)]
pub fn right_mouse_button_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_RBUTTON};
    unsafe { (GetAsyncKeyState(VK_RBUTTON.0 as i32) as u16 & 0x8000) != 0 }
}

#[cfg(not(windows))]
pub fn right_mouse_button_down() -> bool {
    false
}

#[cfg(windows)]
pub fn foreground_window_is_native_menu() -> bool {
    use windows::Win32::{
        Foundation::POINT,
        UI::WindowsAndMessaging::{GetCursorPos, GetForegroundWindow, WindowFromPoint},
    };

    unsafe {
        let foreground = GetForegroundWindow();
        if is_native_menu_window(foreground) {
            return true;
        }
        let mut point = POINT::default();
        if GetCursorPos(&mut point).is_ok() {
            return is_native_menu_window(WindowFromPoint(point));
        }
        false
    }
}

#[cfg(not(windows))]
pub fn foreground_window_is_native_menu() -> bool {
    false
}

#[cfg(windows)]
unsafe fn is_native_menu_window(hwnd: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
    if hwnd.0.is_null() {
        return false;
    }
    let mut buf = vec![0u16; 128];
    let len = GetClassNameW(hwnd, &mut buf);
    if len <= 0 {
        return false;
    }
    let class_name = String::from_utf16_lossy(&buf[..len as usize]);
    class_name == "#32768"
        || class_name.to_ascii_lowercase().contains("menu")
        || class_name.to_ascii_lowercase().contains("popup")
}

#[cfg(windows)]
pub fn replace_preserved_selection(
    captured: &FocusedTextContext,
    replacement: &str,
) -> anyhow::Result<bool> {
    use windows::core::BSTR;
    use windows::Win32::{
        Foundation::{RPC_E_CHANGED_MODE, S_FALSE, S_OK},
        System::Com::{
            CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
            COINIT_APARTMENTTHREADED,
        },
        UI::Accessibility::{
            CUIAutomation, IUIAutomation, IUIAutomationValuePattern, UIA_ValuePatternId,
        },
    };

    let Some(selected) = captured
        .selected_text
        .as_ref()
        .filter(|text| !text.is_empty())
    else {
        return Ok(false);
    };

    unsafe {
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let should_uninit = hr == S_OK || hr == S_FALSE;
        if hr.is_err() && hr != RPC_E_CHANGED_MODE {
            return Ok(false);
        }

        let result = (|| {
            let automation: IUIAutomation =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
            let element = automation.GetFocusedElement().ok()?;
            if element.CurrentIsPassword().ok()?.as_bool()
                || !focused_element_matches(&element, captured)
            {
                return None;
            }

            let pattern = element
                .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                .ok()?;
            let current = bstr_to_string(pattern.CurrentValue().ok()?);
            let next = replace_selected_text_in_value(&current, captured, selected, replacement)?;
            pattern.SetValue(&BSTR::from(next.as_str())).ok()?;
            Some(true)
        })()
        .unwrap_or(false);

        if should_uninit {
            CoUninitialize();
        }
        Ok(result)
    }
}

#[cfg(not(windows))]
pub fn replace_preserved_selection(
    _captured: &FocusedTextContext,
    _replacement: &str,
) -> anyhow::Result<bool> {
    Ok(false)
}

#[cfg(windows)]
fn window_context_from_hwnd(
    hwnd: windows::Win32::Foundation::HWND,
    cursor_x: i32,
    cursor_y: i32,
) -> Option<WindowContext> {
    use windows::{
        core::PWSTR,
        Win32::{
            Foundation::{CloseHandle, MAX_PATH},
            System::Threading::{
                OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
            },
            UI::WindowsAndMessaging::{
                GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
            },
        },
    };

    unsafe {
        if hwnd.0.is_null() {
            return None;
        }
        let len = GetWindowTextLengthW(hwnd);
        let mut title_buf = vec![0u16; len as usize + 1];
        let title_len = GetWindowTextW(hwnd, &mut title_buf) as usize;
        let title = String::from_utf16_lossy(&title_buf[..title_len]);

        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let app_name = if pid != 0 {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid);
            if let Ok(handle) = process {
                let mut buf = vec![0u16; MAX_PATH as usize];
                let mut size = buf.len() as u32;
                let name = QueryFullProcessImageNameW(
                    handle,
                    Default::default(),
                    PWSTR(buf.as_mut_ptr()),
                    &mut size,
                )
                .ok()
                .map(|_| {
                    let full = String::from_utf16_lossy(&buf[..size as usize]);
                    std::path::Path::new(&full)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or(full)
                })
                .unwrap_or_else(|| format!("pid:{pid}"));
                let _ = CloseHandle(handle);
                name
            } else {
                format!("pid:{pid}")
            }
        } else {
            "unknown".to_string()
        };

        Some(WindowContext {
            title,
            app_name,
            cursor_x,
            cursor_y,
            focused_text: None,
            cursor_screenshot: None,
        })
    }
}

#[cfg(windows)]
fn focused_text_context(limit_chars: usize) -> Option<FocusedTextContext> {
    use windows::Win32::{
        Foundation::{RPC_E_CHANGED_MODE, S_FALSE, S_OK},
        System::Com::{
            CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
            COINIT_APARTMENTTHREADED,
        },
        UI::Accessibility::{
            CUIAutomation, IUIAutomation, IUIAutomationTextPattern, IUIAutomationTextPattern2,
            IUIAutomationValuePattern, UIA_TextPatternId, UIA_ValuePatternId,
        },
    };

    unsafe {
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let should_uninit = hr == S_OK || hr == S_FALSE;
        if hr.is_err() && hr != RPC_E_CHANGED_MODE {
            return None;
        }

        let result = (|| {
            let automation: IUIAutomation =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
            let element = automation.GetFocusedElement().ok()?;
            if element.CurrentIsPassword().ok()?.as_bool() {
                return None;
            }

            let element_name = clean_optional_bstr(element.CurrentName().ok());
            let control_type = element
                .CurrentLocalizedControlType()
                .ok()
                .and_then(|b| clean_optional_bstr(Some(b)));
            let class_name = element
                .CurrentClassName()
                .ok()
                .and_then(|b| clean_optional_bstr(Some(b)));
            let automation_id = element
                .CurrentAutomationId()
                .ok()
                .and_then(|b| clean_optional_bstr(Some(b)));
            let element_bounds = element
                .CurrentBoundingRectangle()
                .ok()
                .map(|r| [r.left, r.top, r.right, r.bottom]);

            // Walk up to the parent so we can identify containers like 'Address bar' or 'Search'
            let (parent_name, parent_class, parent_control_type) = automation
                .ControlViewWalker()
                .ok()
                .and_then(|walker| walker.GetParentElement(&element).ok())
                .map(|parent| {
                    let pn = clean_optional_bstr(parent.CurrentName().ok());
                    let pc = parent
                        .CurrentClassName()
                        .ok()
                        .and_then(|b| clean_optional_bstr(Some(b)));
                    let pct = parent
                        .CurrentLocalizedControlType()
                        .ok()
                        .and_then(|b| clean_optional_bstr(Some(b)));
                    (pn, pc, pct)
                })
                .unwrap_or((None, None, None));

            let apply_meta = |ctx: &mut FocusedTextContext| {
                ctx.element_name = element_name.clone();
                ctx.control_type = control_type.clone();
                ctx.class_name = class_name.clone();
                ctx.automation_id = automation_id.clone();
                ctx.parent_name = parent_name.clone();
                ctx.parent_class = parent_class.clone();
                ctx.parent_control_type = parent_control_type.clone();
                ctx.element_bounds = element_bounds;
            };

            if let Ok(pattern) =
                element.GetCurrentPatternAs::<IUIAutomationTextPattern2>(UIA_TextPatternId)
            {
                if let Some(mut context) = context_from_text_pattern2(pattern, limit_chars) {
                    apply_meta(&mut context);
                    return Some(context);
                }
            }

            if let Ok(pattern) =
                element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
            {
                if let Some(mut context) = context_from_text_pattern(pattern, limit_chars) {
                    apply_meta(&mut context);
                    return Some(context);
                }
            }

            if let Ok(pattern) =
                element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
            {
                let text = clean_text(&bstr_to_string(pattern.CurrentValue().ok()?));
                if !text.is_empty() {
                    let (full_text, truncated) = trim_middle(&text, limit_chars);
                    let mut ctx = FocusedTextContext {
                        source: "UIA ValuePattern".to_string(),
                        element_name: None,
                        control_type: None,
                        class_name: None,
                        automation_id: None,
                        parent_name: None,
                        parent_class: None,
                        parent_control_type: None,
                        text_before_cursor: None,
                        selected_text: None,
                        text_after_cursor: None,
                        full_text: Some(full_text),
                        truncated,
                        cursor_known: false,
                        element_bounds: None,
                    };
                    apply_meta(&mut ctx);
                    return Some(ctx);
                }
            }

            // Element exists but no text patterns — still expose the metadata so the
            // interpretation prompt can detect the field type (search bar, address bar, etc.).
            let mut ctx = FocusedTextContext {
                source: "UIA element only".to_string(),
                element_name: None,
                control_type: None,
                class_name: None,
                automation_id: None,
                parent_name: None,
                parent_class: None,
                parent_control_type: None,
                text_before_cursor: None,
                selected_text: None,
                text_after_cursor: None,
                full_text: None,
                truncated: false,
                cursor_known: false,
                element_bounds: None,
            };
            apply_meta(&mut ctx);
            if ctx.element_name.is_some()
                || ctx.control_type.is_some()
                || ctx.class_name.is_some()
                || ctx.automation_id.is_some()
            {
                Some(ctx)
            } else {
                None
            }
        })();

        if should_uninit {
            CoUninitialize();
        }
        result
    }
}

#[cfg(not(windows))]
fn focused_text_context(_limit_chars: usize) -> Option<FocusedTextContext> {
    None
}

#[cfg(windows)]
unsafe fn context_from_text_pattern2(
    pattern: windows::Win32::UI::Accessibility::IUIAutomationTextPattern2,
    limit_chars: usize,
) -> Option<FocusedTextContext> {
    use windows::core::Interface;
    use windows::Win32::Foundation::BOOL;

    let mut is_active = BOOL(0);
    let caret = pattern.GetCaretRange(&mut is_active).ok()?;
    let base: windows::Win32::UI::Accessibility::IUIAutomationTextPattern = pattern.cast().ok()?;

    // GetCaretRange returns only the cursor insertion point — it has no extent and yields no
    // selected text.  Check GetSelection first; if the selection range contains text, use it
    // so that before/selected/after are split correctly.
    let selection_range = base.GetSelection().ok().and_then(|ranges| {
        ranges
            .Length()
            .ok()
            .filter(|&len| len > 0)
            .and_then(|_| ranges.GetElement(0).ok())
    });

    let effective_range = if let Some(sel) = selection_range {
        // Probe for at least one character to confirm the selection has actual extent.
        let has_extent = sel
            .GetText(1)
            .ok()
            .map(|b| !clean_text(&bstr_to_string(b)).is_empty())
            .unwrap_or(false);
        if has_extent {
            sel
        } else {
            caret
        }
    } else {
        caret
    };

    context_from_text_pattern_and_caret(
        base,
        effective_range,
        is_active.as_bool(),
        "UIA TextPattern2",
        limit_chars,
    )
}

#[cfg(windows)]
unsafe fn context_from_text_pattern(
    pattern: windows::Win32::UI::Accessibility::IUIAutomationTextPattern,
    limit_chars: usize,
) -> Option<FocusedTextContext> {
    let selection = pattern.GetSelection().ok();
    let selected = selection.as_ref().and_then(|ranges| {
        ranges
            .Length()
            .ok()
            .filter(|len| *len > 0)
            .and_then(|_| ranges.GetElement(0).ok())
    });
    if let Some(caret) = selected {
        return context_from_text_pattern_and_caret(
            pattern,
            caret,
            false,
            "UIA TextPattern",
            limit_chars,
        );
    }
    let document = pattern.DocumentRange().ok()?;
    let text = clean_text(&bstr_to_string(document.GetText(limit_chars as i32).ok()?));
    if text.is_empty() {
        return None;
    }
    let (full_text, truncated) = trim_middle(&text, limit_chars);
    Some(FocusedTextContext {
        source: "UIA TextPattern".to_string(),
        element_name: None,
        control_type: None,
        class_name: None,
        automation_id: None,
        parent_name: None,
        parent_class: None,
        parent_control_type: None,
        text_before_cursor: None,
        selected_text: None,
        text_after_cursor: None,
        full_text: Some(full_text),
        truncated,
        cursor_known: false,
        element_bounds: None,
    })
}

#[cfg(windows)]
unsafe fn context_from_text_pattern_and_caret(
    pattern: windows::Win32::UI::Accessibility::IUIAutomationTextPattern,
    caret: windows::Win32::UI::Accessibility::IUIAutomationTextRange,
    is_active: bool,
    source: &str,
    limit_chars: usize,
) -> Option<FocusedTextContext> {
    use windows::Win32::UI::Accessibility::{
        IUIAutomationTextRange, TextPatternRangeEndpoint_End, TextPatternRangeEndpoint_Start,
    };

    let document = pattern.DocumentRange().ok()?;
    let before_range: IUIAutomationTextRange = document.Clone().ok()?;
    before_range
        .MoveEndpointByRange(
            TextPatternRangeEndpoint_End,
            &caret,
            TextPatternRangeEndpoint_Start,
        )
        .ok()?;
    let after_range: IUIAutomationTextRange = document.Clone().ok()?;
    after_range
        .MoveEndpointByRange(
            TextPatternRangeEndpoint_Start,
            &caret,
            TextPatternRangeEndpoint_End,
        )
        .ok()?;

    let before_raw = clean_text(&bstr_to_string(
        before_range.GetText(limit_chars as i32).ok()?,
    ));
    let after_raw = clean_text(&bstr_to_string(
        after_range.GetText(limit_chars as i32).ok()?,
    ));
    let selected_raw = clean_text(&bstr_to_string(caret.GetText(limit_chars as i32).ok()?));
    if before_raw.is_empty() && after_raw.is_empty() && selected_raw.is_empty() {
        return None;
    }

    let before_truncated = before_raw.chars().count() > limit_chars;
    let after_truncated = after_raw.chars().count() > limit_chars;
    Some(FocusedTextContext {
        source: source.to_string(),
        element_name: None,
        control_type: None,
        class_name: None,
        automation_id: None,
        parent_name: None,
        parent_class: None,
        parent_control_type: None,
        text_before_cursor: Some(take_tail(&before_raw, limit_chars)),
        selected_text: (!selected_raw.is_empty()).then(|| take_head(&selected_raw, limit_chars)),
        text_after_cursor: Some(take_head(&after_raw, limit_chars)),
        full_text: None,
        truncated: before_truncated || after_truncated,
        cursor_known: is_active,
        element_bounds: None,
    })
}

#[cfg(windows)]
fn clean_optional_bstr(value: Option<windows::core::BSTR>) -> Option<String> {
    value
        .map(|b| clean_text(&bstr_to_string(b)))
        .filter(|text| !text.is_empty())
}

#[cfg(windows)]
unsafe fn focused_element_matches(
    element: &windows::Win32::UI::Accessibility::IUIAutomationElement,
    captured: &FocusedTextContext,
) -> bool {
    if let Some(expected) = captured.class_name.as_ref() {
        let Some(current) = element
            .CurrentClassName()
            .ok()
            .and_then(|b| clean_optional_bstr(Some(b)))
        else {
            return false;
        };
        if current != *expected {
            return false;
        }
    }

    if let Some(expected) = captured.control_type.as_ref() {
        let Some(current) = element
            .CurrentLocalizedControlType()
            .ok()
            .and_then(|b| clean_optional_bstr(Some(b)))
        else {
            return false;
        };
        if current != *expected {
            return false;
        }
    }

    if let Some(expected) = captured.element_bounds {
        let Ok(current) = element.CurrentBoundingRectangle() else {
            return false;
        };
        let drift = (current.left - expected[0]).abs()
            + (current.top - expected[1]).abs()
            + (current.right - expected[2]).abs()
            + (current.bottom - expected[3]).abs();
        if drift > 32 {
            return false;
        }
    }

    true
}

fn replace_selected_text_in_value(
    current: &str,
    captured: &FocusedTextContext,
    selected: &str,
    replacement: &str,
) -> Option<String> {
    let before = captured.text_before_cursor.as_deref().unwrap_or_default();
    let after = captured.text_after_cursor.as_deref().unwrap_or_default();
    let mut matches = Vec::new();
    let mut search_from = 0;

    while let Some(relative) = current[search_from..].find(selected) {
        let start = search_from + relative;
        let end = start + selected.len();
        let prefix_ok = before.is_empty() || current[..start].ends_with(before);
        let suffix_ok = after.is_empty() || current[end..].starts_with(after);
        if prefix_ok && suffix_ok {
            matches.push((start, end));
        }
        search_from = end;
    }

    let (start, end) = if matches.len() == 1 {
        matches[0]
    } else if matches.is_empty() {
        return None;
    } else {
        return None;
    };

    let mut next = String::with_capacity(current.len() - selected.len() + replacement.len());
    next.push_str(&current[..start]);
    next.push_str(replacement);
    next.push_str(&current[end..]);
    Some(next)
}

#[cfg(windows)]
fn bstr_to_string(value: windows::core::BSTR) -> String {
    value.to_string()
}

fn clean_text(text: &str) -> String {
    text.replace('\r', "").trim_matches('\0').trim().to_string()
}

fn trim_middle(text: &str, limit_chars: usize) -> (String, bool) {
    let count = text.chars().count();
    if count <= limit_chars {
        return (text.to_string(), false);
    }
    let half = (limit_chars / 2).max(1);
    let head = take_head(text, half);
    let tail = take_tail(text, half);
    (format!("{head}\n...\n{tail}"), true)
}

fn take_head(text: &str, limit_chars: usize) -> String {
    text.chars().take(limit_chars).collect()
}

fn take_tail(text: &str, limit_chars: usize) -> String {
    text.chars()
        .rev()
        .take(limit_chars)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

#[cfg(windows)]
fn capture_window_screenshot(
    hwnd: windows::Win32::Foundation::HWND,
    cursor_x: i32,
    cursor_y: i32,
    screenshot: ScreenshotConfig,
) -> Option<CursorScreenshot> {
    use windows::Win32::{Foundation::RECT, UI::WindowsAndMessaging::GetWindowRect};

    unsafe {
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).ok()?;
        let source_width = rect.right - rect.left;
        let source_height = rect.bottom - rect.top;
        if source_width < 40 || source_height < 40 {
            return None;
        }
        let scale = (screenshot.width as f64 / source_width as f64)
            .min(screenshot.height as f64 / source_height as f64)
            .min(1.0);
        let output_width = ((source_width as f64 * scale).round() as u32).max(1);
        let output_height = ((source_height as f64 * scale).round() as u32).max(1);
        capture_screen_rect_to_png(
            rect.left,
            rect.top,
            source_width,
            source_height,
            output_width,
            output_height,
            cursor_x,
            cursor_y,
        )
    }
}

#[cfg(windows)]
fn capture_cursor_screenshot(
    width: u32,
    height: u32,
    cursor_x: i32,
    cursor_y: i32,
) -> Option<CursorScreenshot> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };

    unsafe {
        let virtual_x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let virtual_y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let virtual_width = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let virtual_height = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        if virtual_width <= 0 || virtual_height <= 0 {
            return None;
        }

        let width = width.min(virtual_width as u32) as i32;
        let height = height.min(virtual_height as u32) as i32;
        let max_left = virtual_x + virtual_width - width;
        let max_top = virtual_y + virtual_height - height;
        let left = (cursor_x - width / 2).clamp(virtual_x, max_left);
        let top = (cursor_y - height / 2).clamp(virtual_y, max_top);
        capture_screen_rect_to_png(
            left,
            top,
            width,
            height,
            width as u32,
            height as u32,
            cursor_x,
            cursor_y,
        )
    }
}

#[cfg(windows)]
fn capture_screen_rect_to_png(
    left: i32,
    top: i32,
    source_width: i32,
    source_height: i32,
    output_width: u32,
    output_height: u32,
    cursor_x: i32,
    cursor_y: i32,
) -> Option<CursorScreenshot> {
    use base64::{engine::general_purpose, Engine};
    use std::{ffi::c_void, mem::size_of, ptr::null_mut};
    use windows::Win32::{
        Foundation::HWND,
        Graphics::Gdi::{
            BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
            GetDIBits, ReleaseDC, SelectObject, SetStretchBltMode, StretchBlt, BITMAPINFO,
            BITMAPINFOHEADER, BI_RGB, COLORONCOLOR, DIB_RGB_COLORS, HGDIOBJ, SRCCOPY,
        },
    };

    unsafe {
        let output_width_i32 = output_width as i32;
        let output_height_i32 = output_height as i32;

        let screen_dc = GetDC(HWND(null_mut()));
        if screen_dc.0.is_null() {
            return None;
        }

        let mem_dc = CreateCompatibleDC(screen_dc);
        if mem_dc.0.is_null() {
            let _ = ReleaseDC(HWND(null_mut()), screen_dc);
            return None;
        }

        let bitmap = CreateCompatibleBitmap(screen_dc, output_width_i32, output_height_i32);
        if bitmap.0.is_null() {
            let _ = DeleteDC(mem_dc);
            let _ = ReleaseDC(HWND(null_mut()), screen_dc);
            return None;
        }

        let previous = SelectObject(mem_dc, HGDIOBJ(bitmap.0));
        let _ = SetStretchBltMode(mem_dc, COLORONCOLOR);
        let copied = if source_width == output_width_i32 && source_height == output_height_i32 {
            BitBlt(
                mem_dc,
                0,
                0,
                output_width_i32,
                output_height_i32,
                screen_dc,
                left,
                top,
                SRCCOPY,
            )
            .is_ok()
        } else {
            StretchBlt(
                mem_dc,
                0,
                0,
                output_width_i32,
                output_height_i32,
                screen_dc,
                left,
                top,
                source_width,
                source_height,
                SRCCOPY,
            )
            .as_bool()
        };

        let mut bgra = vec![0u8; (output_width_i32 * output_height_i32 * 4) as usize];
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: output_width_i32,
                biHeight: -output_height_i32,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let read = if copied {
            GetDIBits(
                mem_dc,
                bitmap,
                0,
                output_height,
                Some(bgra.as_mut_ptr().cast::<c_void>()),
                &mut info,
                DIB_RGB_COLORS,
            )
        } else {
            0
        };

        if !previous.0.is_null() {
            let _ = SelectObject(mem_dc, previous);
        }
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(mem_dc);
        let _ = ReleaseDC(HWND(null_mut()), screen_dc);

        if read == 0 {
            return None;
        }

        let mut rgba = Vec::with_capacity(bgra.len());
        for px in bgra.chunks_exact(4) {
            rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
        }

        let marker_x =
            ((cursor_x - left) as f64 * output_width as f64 / source_width as f64).round() as i32;
        let marker_y =
            ((cursor_y - top) as f64 * output_height as f64 / source_height as f64).round() as i32;
        draw_cursor_highlight(&mut rgba, output_width, output_height, marker_x, marker_y);

        let mut png_bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png_bytes, output_width, output_height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().ok()?;
            writer.write_image_data(&rgba).ok()?;
        }

        Some(CursorScreenshot {
            png_base64: general_purpose::STANDARD.encode(png_bytes),
            width: output_width,
            height: output_height,
            cursor_x: marker_x,
            cursor_y: marker_y,
        })
    }
}

#[cfg(not(windows))]
fn capture_cursor_screenshot(
    _width: u32,
    _height: u32,
    _cursor_x: i32,
    _cursor_y: i32,
) -> Option<CursorScreenshot> {
    None
}

fn draw_cursor_highlight(rgba: &mut [u8], width: u32, height: u32, cx: i32, cy: i32) {
    for radius in [18, 19] {
        draw_circle(rgba, width, height, cx, cy, radius, [255, 255, 255, 255]);
    }
    for radius in [15, 16] {
        draw_circle(rgba, width, height, cx, cy, radius, [238, 36, 56, 255]);
    }
    draw_line(
        rgba,
        width,
        height,
        cx - 24,
        cy,
        cx - 7,
        cy,
        [238, 36, 56, 255],
    );
    draw_line(
        rgba,
        width,
        height,
        cx + 7,
        cy,
        cx + 24,
        cy,
        [238, 36, 56, 255],
    );
    draw_line(
        rgba,
        width,
        height,
        cx,
        cy - 24,
        cx,
        cy - 7,
        [238, 36, 56, 255],
    );
    draw_line(
        rgba,
        width,
        height,
        cx,
        cy + 7,
        cx,
        cy + 24,
        [238, 36, 56, 255],
    );
}

fn draw_circle(
    rgba: &mut [u8],
    width: u32,
    height: u32,
    cx: i32,
    cy: i32,
    radius: i32,
    color: [u8; 4],
) {
    let r2 = radius * radius;
    let inner = (radius - 2).max(0);
    let inner2 = inner * inner;
    for y in (cy - radius)..=(cy + radius) {
        for x in (cx - radius)..=(cx + radius) {
            let d2 = (x - cx) * (x - cx) + (y - cy) * (y - cy);
            if d2 <= r2 && d2 >= inner2 {
                set_pixel(rgba, width, height, x, y, color);
            }
        }
    }
}

fn draw_line(
    rgba: &mut [u8],
    width: u32,
    height: u32,
    mut x0: i32,
    mut y0: i32,
    x1: i32,
    y1: i32,
    color: [u8; 4],
) {
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;

    loop {
        set_pixel(rgba, width, height, x0, y0, color);
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}

fn set_pixel(rgba: &mut [u8], width: u32, height: u32, x: i32, y: i32, color: [u8; 4]) {
    if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
        return;
    }
    let index = ((y as u32 * width + x as u32) * 4) as usize;
    rgba[index..index + 4].copy_from_slice(&color);
}
