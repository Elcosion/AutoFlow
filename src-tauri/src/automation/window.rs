use super::types::{ScreenRect, VisionError, WindowId, WindowInfo, WindowProvider};

#[cfg(windows)]
use std::sync::Once;

#[cfg(windows)]
use windows::core::BOOL;
#[cfg(windows)]
use windows::Win32::Foundation::{HWND, LPARAM, RECT, TRUE};
#[cfg(windows)]
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
#[cfg(windows)]
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible,
};

#[cfg(windows)]
static DPI_AWARENESS: Once = Once::new();

#[cfg(windows)]
#[derive(Default)]
pub struct WindowsWindowProvider;

#[cfg(windows)]
impl WindowsWindowProvider {
    pub fn new() -> Self {
        Self::ensure_dpi_awareness();
        Self
    }

    fn ensure_dpi_awareness() {
        DPI_AWARENESS.call_once(|| {
            // Tauri may already have selected an awareness mode. Access denied
            // in that case is harmless and the process keeps its existing mode.
            let _ = unsafe {
                SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
            };
        });
    }

    fn read_window(hwnd: HWND) -> Option<WindowInfo> {
        if !unsafe { IsWindow(Some(hwnd)).as_bool() } {
            return None;
        }
        let title = read_title(hwnd);
        let rect = read_screen_rect(hwnd)?;
        let mut process_id = 0u32;
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut process_id));
        }
        Some(WindowInfo {
            id: WindowId(hwnd.0 as isize),
            title,
            rect,
            visible: unsafe { IsWindowVisible(hwnd).as_bool() },
            minimized: unsafe { IsIconic(hwnd).as_bool() },
            process_id: (process_id != 0).then_some(process_id),
        })
    }
}

#[cfg(windows)]
impl WindowProvider for WindowsWindowProvider {
    fn active_window(&self) -> Result<Option<WindowInfo>, VisionError> {
        Self::ensure_dpi_awareness();
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.0.is_null() {
            return Ok(None);
        }
        Ok(Self::read_window(hwnd))
    }

    fn find_windows(&self, title_query: &str) -> Result<Vec<WindowInfo>, VisionError> {
        Self::ensure_dpi_awareness();
        if title_query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let query = title_query.to_lowercase();
        let mut state = (Vec::<WindowInfo>::new(), query);
        let result = unsafe {
            EnumWindows(
                Some(enum_windows_callback),
                LPARAM((&mut state as *mut (Vec<WindowInfo>, String)) as isize),
            )
        };
        result.map_err(|error| {
            VisionError::new("window_query_failed", format!("枚举窗口失败：{error}"))
        })?;
        Ok(state.0)
    }

    fn window_rect(&self, title_query: &str) -> Result<Option<ScreenRect>, VisionError> {
        Ok(self
            .find_windows(title_query)?
            .into_iter()
            .find(|window| window.rect.width > 0 && window.rect.height > 0)
            .map(|window| window.rect))
    }
}

#[cfg(windows)]
unsafe extern "system" fn enum_windows_callback(hwnd: HWND, data: LPARAM) -> BOOL {
    let state = unsafe { &mut *(data.0 as *mut (Vec<WindowInfo>, String)) };
    if let Some(window) = WindowsWindowProvider::read_window(hwnd) {
        if title_contains(&window.title, &state.1) {
            state.0.push(window);
        }
    }
    TRUE
}

#[cfg(windows)]
fn read_title(hwnd: HWND) -> String {
    let length = unsafe { GetWindowTextLengthW(hwnd) };
    if length <= 0 {
        return String::new();
    }
    let capacity = usize::try_from(length)
        .ok()
        .map_or(0, |value| value.saturating_add(1));
    let mut buffer = vec![0u16; capacity];
    let copied = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    if copied <= 0 {
        return String::new();
    }
    let copied = usize::try_from(copied)
        .ok()
        .map_or(0, |value| value.min(buffer.len()));
    String::from_utf16_lossy(&buffer[..copied])
}

#[cfg(windows)]
fn read_screen_rect(hwnd: HWND) -> Option<ScreenRect> {
    let mut fallback = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut fallback) }.is_err() {
        return None;
    }
    let mut extended = RECT::default();
    let extended_result = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut extended as *mut RECT).cast(),
            u32::try_from(std::mem::size_of::<RECT>()).ok()?,
        )
    };
    let rect = if extended_result.is_ok() {
        extended
    } else {
        fallback
    };
    let width = rect.right.checked_sub(rect.left)?;
    let height = rect.bottom.checked_sub(rect.top)?;
    if width <= 0 || height <= 0 {
        return None;
    }
    Some(ScreenRect::from_parts(
        rect.left,
        rect.top,
        u32::try_from(width).ok()?,
        u32::try_from(height).ok()?,
    ))
}

#[cfg(windows)]
pub fn title_contains(title: &str, query: &str) -> bool {
    !query.trim().is_empty() && title.to_lowercase().contains(&query.to_lowercase())
}

#[cfg(not(windows))]
#[derive(Default)]
pub struct WindowsWindowProvider;

#[cfg(not(windows))]
impl WindowsWindowProvider {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(not(windows))]
impl WindowProvider for WindowsWindowProvider {
    fn active_window(&self) -> Result<Option<WindowInfo>, VisionError> {
        Err(VisionError::new(
            "capture_not_supported",
            "窗口检测目前只支持 Windows",
        ))
    }

    fn find_windows(&self, _title_query: &str) -> Result<Vec<WindowInfo>, VisionError> {
        Err(VisionError::new(
            "capture_not_supported",
            "窗口检测目前只支持 Windows",
        ))
    }

    fn window_rect(&self, _title_query: &str) -> Result<Option<ScreenRect>, VisionError> {
        Err(VisionError::new(
            "capture_not_supported",
            "窗口检测目前只支持 Windows",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::title_contains;

    #[test]
    fn title_matching_is_case_insensitive_and_substring_based() {
        assert!(title_contains("Notepad — AutoFlow", "notepad"));
        assert!(title_contains("记事本", "事本"));
        assert!(!title_contains("Calculator", "notepad"));
        assert!(!title_contains("anything", ""));
    }
}
