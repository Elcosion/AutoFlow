use super::types::{CaptureBackend, CaptureFrame, Point, ScreenRect, VisionError, WindowId};

#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(windows)]
use std::sync::{Arc, Condvar, Mutex};
#[cfg(windows)]
use std::time::{Duration, Instant};

#[cfg(windows)]
use windows::core::BOOL;
#[cfg(windows)]
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
#[cfg(windows)]
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
#[cfg(windows)]
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, HDC, HMONITOR, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

#[cfg(windows)]
use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
#[cfg(windows)]
use windows_capture::dxgi_duplication_api::{
    DxgiDuplicationApi, DxgiDuplicationFormat, Error as DxgiError,
};
#[cfg(windows)]
use windows_capture::frame::Frame;
#[cfg(windows)]
use windows_capture::graphics_capture_api::InternalCaptureControl;
#[cfg(windows)]
use windows_capture::monitor::Monitor;
#[cfg(windows)]
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};
#[cfg(windows)]
use windows_capture::window::Window;

#[cfg(windows)]
const CAPTURE_WAIT: Duration = Duration::from_millis(500);
#[cfg(windows)]
const CAPTURE_FRAME_INTERVAL: Duration = Duration::from_millis(50);
#[cfg(windows)]
const DXGI_FRAME_TIMEOUT_MS: u32 = 150;

#[cfg(windows)]
struct FrameMailbox {
    latest: Mutex<Option<CaptureFrame>>,
    failure: Mutex<Option<String>>,
    closed: AtomicBool,
    changed: Condvar,
}

#[cfg(windows)]
impl FrameMailbox {
    fn new() -> Self {
        Self {
            latest: Mutex::new(None),
            failure: Mutex::new(None),
            closed: AtomicBool::new(false),
            changed: Condvar::new(),
        }
    }

    fn publish(&self, frame: CaptureFrame) -> Result<(), String> {
        let mut latest = self
            .latest
            .lock()
            .map_err(|_| "捕获帧状态异常".to_string())?;
        *latest = Some(frame);
        self.changed.notify_all();
        Ok(())
    }

    fn fail(&self, message: impl Into<String>) {
        if let Ok(mut failure) = self.failure.lock() {
            *failure = Some(message.into());
        }
        self.closed.store(true, Ordering::SeqCst);
        self.changed.notify_all();
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.changed.notify_all();
    }

    fn wait(&self, cancel: &AtomicBool) -> Result<CaptureFrame, VisionError> {
        let deadline = Instant::now() + CAPTURE_WAIT;
        let mut latest = self
            .latest
            .lock()
            .map_err(|_| VisionError::new("capture_device_lost", "捕获帧状态异常，请重试"))?;
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err(VisionError::cancelled());
            }
            if self.closed.load(Ordering::SeqCst) {
                let detail = self
                    .failure
                    .lock()
                    .ok()
                    .and_then(|failure| failure.clone())
                    .unwrap_or_else(|| "捕获目标已关闭".to_string());
                return Err(VisionError::new("window_closed", detail));
            }
            if let Some(frame) = latest.as_ref() {
                return Ok(frame.clone());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(VisionError::new(
                    "capture_timeout",
                    "等待捕获帧超时，请确认目标窗口仍可见",
                ));
            }
            let wait_for = remaining.min(Duration::from_millis(25));
            let (guard, _) = self
                .changed
                .wait_timeout(latest, wait_for)
                .map_err(|_| VisionError::new("capture_device_lost", "捕获等待状态异常"))?;
            latest = guard;
        }
    }
}

#[cfg(windows)]
struct CaptureFlags {
    mailbox: Arc<FrameMailbox>,
    origin: Point,
}

#[cfg(windows)]
struct CaptureHandler {
    mailbox: Arc<FrameMailbox>,
    origin: Point,
}

#[cfg(windows)]
impl GraphicsCaptureApiHandler for CaptureHandler {
    type Flags = CaptureFlags;
    type Error = String;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self {
            mailbox: ctx.flags.mailbox,
            origin: ctx.flags.origin,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        _capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let width = frame.width();
        let height = frame.height();
        let mut packed = Vec::new();
        let buffer = match frame.buffer() {
            Ok(buffer) => buffer,
            Err(error) => {
                let message = format!("读取 Windows Graphics Capture 帧失败：{error}");
                self.mailbox.fail(message.clone());
                return Err(message);
            }
        };
        let bytes = buffer.as_nopadding_buffer(&mut packed).to_vec();
        let captured = match CaptureFrame::from_bgra(self.origin, width, height, bytes) {
            Ok(captured) => captured,
            Err(error) => {
                let message = error.to_string();
                self.mailbox.fail(message.clone());
                return Err(message);
            }
        };
        if let Err(error) = self.mailbox.publish(captured) {
            let message = format!("发布捕获帧失败：{error}");
            self.mailbox.fail(message.clone());
            return Err(message);
        }
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        self.mailbox.close();
        Ok(())
    }
}

#[cfg(windows)]
struct WgcSession {
    target: CaptureTarget,
    mailbox: Arc<FrameMailbox>,
    control: CaptureControl<CaptureHandler, String>,
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureTarget {
    Window(WindowId),
    Monitor(isize),
}

#[cfg(windows)]
impl WgcSession {
    fn stop(self) {
        let _ = self.control.stop();
    }
}

#[cfg(windows)]
struct DxgiSession {
    monitor_id: isize,
    monitor: Monitor,
    duplication: DxgiDuplicationApi,
}

#[cfg(windows)]
pub struct WindowsCaptureBackend {
    window_session: Option<WgcSession>,
    monitor_sessions: std::collections::HashMap<isize, WgcSession>,
    dxgi_session: Option<DxgiSession>,
}

#[cfg(windows)]
impl Default for WindowsCaptureBackend {
    fn default() -> Self {
        Self {
            window_session: None,
            monitor_sessions: std::collections::HashMap::new(),
            dxgi_session: None,
        }
    }
}

#[cfg(windows)]
impl WindowsCaptureBackend {
    pub fn new() -> Self {
        Self::default()
    }

    fn stop_window_session(&mut self) {
        if let Some(session) = self.window_session.take() {
            session.stop();
        }
    }

    fn stop_monitor_session(&mut self, monitor_id: isize) {
        if let Some(session) = self.monitor_sessions.remove(&monitor_id) {
            session.stop();
        }
    }

    fn stop_all_monitor_sessions(&mut self) {
        for (_, session) in std::mem::take(&mut self.monitor_sessions) {
            session.stop();
        }
    }

    fn ensure_monitor_session(
        &mut self,
        monitor_id: isize,
        monitor_rect: ScreenRect,
    ) -> Result<(), VisionError> {
        let existing = self.monitor_sessions.get(&monitor_id).map(|session| {
            session.target == CaptureTarget::Monitor(monitor_id) && !session.control.is_finished()
        });
        if existing == Some(true) {
            return Ok(());
        }
        self.stop_monitor_session(monitor_id);
        self.monitor_sessions
            .insert(monitor_id, start_monitor_capture(monitor_id, monitor_rect)?);
        Ok(())
    }

    fn ensure_window_session(
        &mut self,
        window: WindowId,
        window_rect: ScreenRect,
    ) -> Result<(), VisionError> {
        let existing = self
            .window_session
            .as_ref()
            .map(|session| session.target == CaptureTarget::Window(window));
        if existing == Some(true)
            && !self
                .window_session
                .as_ref()
                .map(|session| session.control.is_finished())
                .unwrap_or(true)
        {
            return Ok(());
        }
        self.stop_window_session();
        self.window_session = Some(start_window_capture(window, window_rect)?);
        Ok(())
    }

    fn capture_monitor_wgc_with_cancel(
        &mut self,
        monitor_id: isize,
        monitor_rect: ScreenRect,
        region: ScreenRect,
        cancel: &AtomicBool,
    ) -> Result<CaptureFrame, VisionError> {
        self.ensure_monitor_session(monitor_id, monitor_rect)?;
        let session = self
            .monitor_sessions
            .get(&monitor_id)
            .ok_or_else(|| VisionError::new("capture_not_supported", "无法创建显示器捕获会话"))?;
        let frame = session.mailbox.wait(cancel)?;
        frame.crop(region)
    }

    fn capture_monitor_with_fallback(
        &mut self,
        monitor_id: isize,
        monitor_rect: ScreenRect,
        region: ScreenRect,
        cancel: &AtomicBool,
    ) -> Result<CaptureFrame, VisionError> {
        match self.capture_monitor_wgc_with_cancel(monitor_id, monitor_rect, region, cancel) {
            Ok(frame) => Ok(frame),
            Err(wgc_error) => {
                if cancel.load(Ordering::SeqCst) || wgc_error.code == "vision_cancelled" {
                    return Err(wgc_error);
                }
                log::debug!("WGC 捕获失败，尝试 DXGI 回退：{wgc_error}");
                self.stop_monitor_session(monitor_id);
                self.capture_monitor_dxgi(monitor_id, monitor_rect, region)
                    .map_err(|dxgi_error| {
                        VisionError::new(
                            dxgi_error.code.clone(),
                            format!("WGC：{}；DXGI：{}", wgc_error.message, dxgi_error.message),
                        )
                    })
            }
        }
    }

    fn capture_region_tiles(
        &mut self,
        region: ScreenRect,
        cancel: &AtomicBool,
    ) -> Result<CaptureFrame, VisionError> {
        let tiles = monitor_tiles(region)?;
        let expected = usize::try_from(region.width)
            .ok()
            .and_then(|width| {
                usize::try_from(region.height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| VisionError::new("capture_region_invalid", "捕获区域尺寸溢出"))?;
        let mut pixels = vec![0u8; expected];
        for tile in tiles {
            if cancel.load(Ordering::SeqCst) {
                return Err(VisionError::cancelled());
            }
            let frame = self.capture_monitor_with_fallback(
                tile.monitor_id,
                tile.monitor_rect,
                tile.region,
                cancel,
            )?;
            copy_frame_into_region(&frame, region, &mut pixels)?;
        }
        CaptureFrame::from_bgra(
            Point {
                x: region.x,
                y: region.y,
            },
            region.width,
            region.height,
            pixels,
        )
    }

    fn capture_monitor_dxgi(
        &mut self,
        monitor_id: isize,
        monitor_rect: ScreenRect,
        region: ScreenRect,
    ) -> Result<CaptureFrame, VisionError> {
        let needs_new = self
            .dxgi_session
            .as_ref()
            .map(|session| session.monitor_id != monitor_id)
            .unwrap_or(true);
        if needs_new {
            self.dxgi_session = None;
            let monitor = Monitor::from_raw_hmonitor(monitor_id as *mut std::ffi::c_void);
            let duplication =
                DxgiDuplicationApi::new_options(monitor, &[DxgiDuplicationFormat::Bgra8])
                    .map_err(dxgi_error)?;
            self.dxgi_session = Some(DxgiSession {
                monitor_id,
                monitor,
                duplication,
            });
        }
        let session = self
            .dxgi_session
            .as_mut()
            .ok_or_else(|| VisionError::new("capture_not_supported", "无法创建 DXGI 捕获会话"))?;
        let mut frame = match session
            .duplication
            .acquire_next_frame(DXGI_FRAME_TIMEOUT_MS)
        {
            Ok(frame) => frame,
            Err(DxgiError::AccessLost) => {
                let old = self.dxgi_session.take().ok_or_else(|| {
                    VisionError::new("capture_device_lost", "DXGI 捕获设备已丢失")
                })?;
                let duplication = old
                    .duplication
                    .recreate_options(&[DxgiDuplicationFormat::Bgra8])
                    .map_err(dxgi_error)?;
                self.dxgi_session = Some(DxgiSession {
                    monitor_id,
                    monitor: old.monitor,
                    duplication,
                });
                let session = self.dxgi_session.as_mut().ok_or_else(|| {
                    VisionError::new("capture_device_lost", "DXGI 捕获设备重建失败")
                })?;
                session
                    .duplication
                    .acquire_next_frame(DXGI_FRAME_TIMEOUT_MS)
                    .map_err(dxgi_error)?
            }
            Err(error) => return Err(dxgi_error(error)),
        };
        let width = frame.width();
        let height = frame.height();
        if !matches!(frame.format(), DxgiDuplicationFormat::Bgra8) {
            return Err(VisionError::new(
                "capture_not_supported",
                "当前 DXGI 输出格式不是 BGRA8",
            ));
        }
        let mut packed = Vec::new();
        let bytes = frame
            .buffer()
            .map_err(dxgi_error)?
            .as_nopadding_buffer(&mut packed)
            .to_vec();
        CaptureFrame::from_bgra(
            Point {
                x: monitor_rect.x,
                y: monitor_rect.y,
            },
            width,
            height,
            bytes,
        )
        .and_then(|frame| frame.crop(region))
    }
}

#[cfg(windows)]
impl CaptureBackend for WindowsCaptureBackend {
    fn capture_region(&mut self, region: ScreenRect) -> Result<CaptureFrame, VisionError> {
        region.validate()?;
        self.capture_region_tiles(region, &AtomicBool::new(false))
    }

    fn capture_window(&mut self, window: WindowId) -> Result<CaptureFrame, VisionError> {
        self.capture_window_with_cancel(window, &AtomicBool::new(false))
    }

    fn capture_region_with_cancel(
        &mut self,
        region: ScreenRect,
        cancel: &AtomicBool,
    ) -> Result<CaptureFrame, VisionError> {
        region.validate()?;
        self.capture_region_tiles(region, cancel)
    }

    fn capture_window_with_cancel(
        &mut self,
        window: WindowId,
        cancel: &AtomicBool,
    ) -> Result<CaptureFrame, VisionError> {
        let rect = window_rect(window.clone())?;
        self.ensure_window_session(window, rect)?;
        let session = self
            .window_session
            .as_ref()
            .ok_or_else(|| VisionError::new("capture_not_supported", "无法创建窗口捕获会话"))?;
        session.mailbox.wait(cancel)
    }

    fn reset(&mut self) {
        self.stop_window_session();
        self.stop_all_monitor_sessions();
        self.dxgi_session = None;
    }
}

#[cfg(windows)]
fn start_monitor_capture(monitor_id: isize, rect: ScreenRect) -> Result<WgcSession, VisionError> {
    let mailbox = Arc::new(FrameMailbox::new());
    let monitor = Monitor::from_raw_hmonitor(monitor_id as *mut std::ffi::c_void);
    let settings = Settings::new(
        monitor,
        CursorCaptureSettings::WithoutCursor,
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Exclude,
        MinimumUpdateIntervalSettings::Custom(CAPTURE_FRAME_INTERVAL),
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        CaptureFlags {
            mailbox: Arc::clone(&mailbox),
            origin: Point {
                x: rect.x,
                y: rect.y,
            },
        },
    );
    let control = CaptureHandler::start_free_threaded(settings).map_err(|error| {
        VisionError::new(
            "capture_not_supported",
            format!("启动 WGC 显示器捕获失败：{error}"),
        )
    })?;
    Ok(WgcSession {
        target: CaptureTarget::Monitor(monitor_id),
        mailbox,
        control,
    })
}

#[cfg(windows)]
fn start_window_capture(window: WindowId, rect: ScreenRect) -> Result<WgcSession, VisionError> {
    let mailbox = Arc::new(FrameMailbox::new());
    let source = Window::from_raw_hwnd(window.0 as *mut std::ffi::c_void);
    let settings = Settings::new(
        source,
        CursorCaptureSettings::WithoutCursor,
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Exclude,
        MinimumUpdateIntervalSettings::Custom(CAPTURE_FRAME_INTERVAL),
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        CaptureFlags {
            mailbox: Arc::clone(&mailbox),
            origin: Point {
                x: rect.x,
                y: rect.y,
            },
        },
    );
    let control = CaptureHandler::start_free_threaded(settings).map_err(|error| {
        VisionError::new(
            "capture_not_supported",
            format!("启动 WGC 窗口捕获失败：{error}"),
        )
    })?;
    Ok(WgcSession {
        target: CaptureTarget::Window(window),
        mailbox,
        control,
    })
}

#[cfg(windows)]
struct MonitorTile {
    monitor_id: isize,
    monitor_rect: ScreenRect,
    region: ScreenRect,
}

#[cfg(windows)]
fn monitor_tiles(region: ScreenRect) -> Result<Vec<MonitorTile>, VisionError> {
    let clip = RECT {
        left: region.x,
        top: region.y,
        right: i32::try_from(region.right_i64()?)
            .map_err(|_| VisionError::new("capture_region_invalid", "捕获区域坐标溢出"))?,
        bottom: i32::try_from(region.bottom_i64()?)
            .map_err(|_| VisionError::new("capture_region_invalid", "捕获区域坐标溢出"))?,
    };
    let mut tiles = Vec::<MonitorTile>::new();
    let mut state = (&mut tiles, region);
    let result = unsafe {
        EnumDisplayMonitors(
            None,
            Some(&clip as *const RECT),
            Some(enum_monitor_callback),
            LPARAM((&mut state as *mut (&mut Vec<MonitorTile>, ScreenRect)) as isize),
        )
    };
    if !result.as_bool() {
        return Err(VisionError::new(
            "capture_region_invalid",
            "无法枚举显示器范围",
        ));
    }
    tiles.sort_by_key(|tile| (tile.region.y, tile.region.x));
    let requested_pixels = u64::from(region.width) * u64::from(region.height);
    let covered_pixels: u64 = tiles
        .iter()
        .map(|tile| u64::from(tile.region.width) * u64::from(tile.region.height))
        .sum();
    if tiles.is_empty() || covered_pixels < requested_pixels {
        if let Some(tile) = monitor_tile_from_point(region)? {
            tiles.clear();
            tiles.push(tile);
        }
    }
    let covered_pixels: u64 = tiles
        .iter()
        .map(|tile| u64::from(tile.region.width) * u64::from(tile.region.height))
        .sum();
    if tiles.is_empty() || covered_pixels < requested_pixels {
        return Err(VisionError::new(
            "capture_region_invalid",
            "捕获区域不在可用显示器范围内",
        ));
    }
    Ok(tiles)
}

#[cfg(windows)]
fn monitor_tile_from_point(region: ScreenRect) -> Result<Option<MonitorTile>, VisionError> {
    let monitor = unsafe {
        MonitorFromPoint(
            POINT {
                x: region.x,
                y: region.y,
            },
            MONITOR_DEFAULTTONEAREST,
        )
    };
    if monitor.0.is_null() {
        return Ok(None);
    }
    let mut info = MONITORINFO {
        cbSize: u32::try_from(std::mem::size_of::<MONITORINFO>())
            .map_err(|_| VisionError::new("capture_region_invalid", "显示器信息大小溢出"))?,
        ..MONITORINFO::default()
    };
    let result = unsafe { GetMonitorInfoW(monitor, &mut info) };
    if !result.as_bool() {
        return Ok(None);
    }
    let rect = info.rcMonitor;
    let width = rect
        .right
        .checked_sub(rect.left)
        .ok_or_else(|| VisionError::new("capture_region_invalid", "显示器宽度无效"))?;
    let height = rect
        .bottom
        .checked_sub(rect.top)
        .ok_or_else(|| VisionError::new("capture_region_invalid", "显示器高度无效"))?;
    let monitor_rect = ScreenRect::new(
        rect.left,
        rect.top,
        u32::try_from(width)
            .map_err(|_| VisionError::new("capture_region_invalid", "显示器宽度无效"))?,
        u32::try_from(height)
            .map_err(|_| VisionError::new("capture_region_invalid", "显示器高度无效"))?,
    )?;
    Ok(monitor_rect
        .intersection(&region)
        .map(|intersection| MonitorTile {
            monitor_id: monitor.0 as isize,
            monitor_rect,
            region: intersection,
        }))
}

#[cfg(windows)]
unsafe extern "system" fn enum_monitor_callback(
    monitor: HMONITOR,
    _hdc: HDC,
    rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    if monitor.0.is_null() || rect.is_null() {
        return BOOL(1);
    }
    let state = unsafe { &mut *(data.0 as *mut (&mut Vec<MonitorTile>, ScreenRect)) };
    let monitor_rect = unsafe { *rect };
    let width = monitor_rect.right.saturating_sub(monitor_rect.left);
    let height = monitor_rect.bottom.saturating_sub(monitor_rect.top);
    let Ok(width) = u32::try_from(width) else {
        return BOOL(1);
    };
    let Ok(height) = u32::try_from(height) else {
        return BOOL(1);
    };
    let Ok(monitor_rect) = ScreenRect::new(monitor_rect.left, monitor_rect.top, width, height)
    else {
        return BOOL(1);
    };
    if let Some(intersection) = monitor_rect.intersection(&state.1) {
        state.0.push(MonitorTile {
            monitor_id: monitor.0 as isize,
            monitor_rect,
            region: intersection,
        });
    }
    BOOL(1)
}

#[cfg(windows)]
fn copy_frame_into_region(
    frame: &CaptureFrame,
    requested: ScreenRect,
    output: &mut [u8],
) -> Result<(), VisionError> {
    let frame_rect =
        ScreenRect::from_parts(frame.origin.x, frame.origin.y, frame.width, frame.height);
    let Some(intersection) = frame_rect.intersection(&requested) else {
        return Err(VisionError::new(
            "capture_region_invalid",
            "显示器捕获帧与请求区域没有交集",
        ));
    };
    let output_stride = usize::try_from(requested.width)
        .ok()
        .and_then(|width| width.checked_mul(4))
        .ok_or_else(|| VisionError::new("capture_region_invalid", "捕获区域宽度溢出"))?;
    let source_stride = usize::try_from(frame.width)
        .ok()
        .and_then(|width| width.checked_mul(4))
        .ok_or_else(|| VisionError::new("capture_region_invalid", "捕获帧宽度溢出"))?;
    let source_x = usize::try_from(i64::from(intersection.x) - i64::from(frame.origin.x))
        .map_err(|_| VisionError::new("capture_region_invalid", "捕获帧横坐标无效"))?;
    let source_y = usize::try_from(i64::from(intersection.y) - i64::from(frame.origin.y))
        .map_err(|_| VisionError::new("capture_region_invalid", "捕获帧纵坐标无效"))?;
    let output_x = usize::try_from(i64::from(intersection.x) - i64::from(requested.x))
        .map_err(|_| VisionError::new("capture_region_invalid", "输出区域横坐标无效"))?;
    let output_y = usize::try_from(i64::from(intersection.y) - i64::from(requested.y))
        .map_err(|_| VisionError::new("capture_region_invalid", "输出区域纵坐标无效"))?;
    let row_bytes = usize::try_from(intersection.width)
        .ok()
        .and_then(|width| width.checked_mul(4))
        .ok_or_else(|| VisionError::new("capture_region_invalid", "捕获交集宽度溢出"))?;
    for row in 0..intersection.height {
        let row = usize::try_from(row)
            .map_err(|_| VisionError::new("capture_region_invalid", "捕获行号溢出"))?;
        let source_start = (source_y + row)
            .checked_mul(source_stride)
            .and_then(|value| value.checked_add(source_x * 4))
            .ok_or_else(|| VisionError::new("capture_region_invalid", "源帧偏移溢出"))?;
        let output_start = (output_y + row)
            .checked_mul(output_stride)
            .and_then(|value| value.checked_add(output_x * 4))
            .ok_or_else(|| VisionError::new("capture_region_invalid", "输出帧偏移溢出"))?;
        let source_end = source_start
            .checked_add(row_bytes)
            .ok_or_else(|| VisionError::new("capture_region_invalid", "源帧范围溢出"))?;
        let output_end = output_start
            .checked_add(row_bytes)
            .ok_or_else(|| VisionError::new("capture_region_invalid", "输出帧范围溢出"))?;
        let source = frame
            .pixels_bgra()
            .get(source_start..source_end)
            .ok_or_else(|| VisionError::new("capture_region_invalid", "捕获帧像素数据不完整"))?;
        let destination = output
            .get_mut(output_start..output_end)
            .ok_or_else(|| VisionError::new("capture_region_invalid", "输出帧像素数据不完整"))?;
        destination.copy_from_slice(source);
    }
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn copies_adjacent_negative_coordinate_monitor_tiles_into_one_roi() {
        let requested = ScreenRect::from_parts(-4, 10, 4, 1);
        let left = CaptureFrame::from_bgra(
            Point { x: -4, y: 10 },
            2,
            1,
            vec![1, 2, 3, 255, 4, 5, 6, 255],
        )
        .expect("left frame");
        let right = CaptureFrame::from_bgra(
            Point { x: -2, y: 10 },
            2,
            1,
            vec![7, 8, 9, 255, 10, 11, 12, 255],
        )
        .expect("right frame");
        let mut output = vec![0; 16];
        copy_frame_into_region(&left, requested, &mut output).expect("left tile");
        copy_frame_into_region(&right, requested, &mut output).expect("right tile");
        assert_eq!(
            output,
            vec![1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255]
        );
    }
}

#[cfg(windows)]
fn window_rect(window: WindowId) -> Result<ScreenRect, VisionError> {
    let hwnd = HWND(window.0 as *mut std::ffi::c_void);
    if !unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(hwnd)).as_bool() } {
        return Err(VisionError::new(
            "window_closed",
            "目标窗口已关闭或句柄无效",
        ));
    }
    if unsafe { windows::Win32::UI::WindowsAndMessaging::IsIconic(hwnd).as_bool() } {
        return Err(VisionError::new("window_minimized", "目标窗口当前已最小化"));
    }
    let mut fallback = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut fallback) }.is_err() {
        return Err(VisionError::new(
            "window_closed",
            "无法读取目标窗口屏幕坐标",
        ));
    }
    let mut extended = RECT::default();
    let rect = if unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut extended as *mut RECT).cast(),
            u32::try_from(std::mem::size_of::<RECT>())
                .map_err(|_| VisionError::new("window_closed", "目标窗口信息大小溢出"))?,
        )
    }
    .is_ok()
    {
        extended
    } else {
        fallback
    };
    let width = rect
        .right
        .checked_sub(rect.left)
        .ok_or_else(|| VisionError::new("window_closed", "目标窗口宽度无效"))?;
    let height = rect
        .bottom
        .checked_sub(rect.top)
        .ok_or_else(|| VisionError::new("window_closed", "目标窗口高度无效"))?;
    ScreenRect::new(
        rect.left,
        rect.top,
        u32::try_from(width).map_err(|_| VisionError::new("window_closed", "目标窗口宽度无效"))?,
        u32::try_from(height).map_err(|_| VisionError::new("window_closed", "目标窗口高度无效"))?,
    )
}

#[cfg(windows)]
fn dxgi_error(error: DxgiError) -> VisionError {
    match error {
        DxgiError::Timeout => VisionError::new("capture_timeout", "等待显示器帧超时"),
        DxgiError::AccessLost => VisionError::new("capture_device_lost", "显示器捕获设备已丢失"),
        other => VisionError::new("capture_not_supported", format!("DXGI 捕获失败：{other}")),
    }
}

#[cfg(not(windows))]
#[derive(Default)]
pub struct WindowsCaptureBackend;

#[cfg(not(windows))]
impl WindowsCaptureBackend {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(not(windows))]
impl CaptureBackend for WindowsCaptureBackend {
    fn capture_region(&mut self, _region: ScreenRect) -> Result<CaptureFrame, VisionError> {
        Err(VisionError::new(
            "capture_not_supported",
            "屏幕捕获目前只支持 Windows",
        ))
    }

    fn capture_window(&mut self, _window: WindowId) -> Result<CaptureFrame, VisionError> {
        Err(VisionError::new(
            "capture_not_supported",
            "窗口捕获目前只支持 Windows",
        ))
    }

    fn reset(&mut self) {}
}
