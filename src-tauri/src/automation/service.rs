use super::assets::{AssetCacheStats, AssetStore};
use super::capture::WindowsCaptureBackend;
use super::types::{
    AutomationAsset, CaptureBackend, CaptureFrame, ImageMatch, Point, RgbColor, ScreenRect,
    VisionApi, VisionError, VisionMatcher, VisionPollOptions, WindowProvider, WindowRectValue,
    MAX_POLL_MS, MAX_WAIT_MS, MIN_POLL_MS,
};
use super::vision::ImageProcVisionMatcher;
use super::window::WindowsWindowProvider;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use windows::Win32::Foundation::FILETIME;
#[cfg(windows)]
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
#[cfg(windows)]
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

const DEFAULT_CAPTURE_INTERVAL: Duration = Duration::from_millis(50);
const WAIT_SLICE: Duration = Duration::from_millis(25);

pub struct VisionService {
    window: Arc<dyn WindowProvider>,
    capture: Mutex<Box<dyn CaptureBackend>>,
    matcher: Arc<dyn VisionMatcher>,
    assets: Arc<AssetStore>,
    catalog: RwLock<HashMap<String, AutomationAsset>>,
    last_capture: Mutex<Option<Instant>>,
    min_capture_interval: Duration,
}

impl VisionService {
    pub fn new(asset_root: PathBuf) -> Arc<Self> {
        Self::with_parts(
            Arc::new(WindowsWindowProvider::new()),
            Box::new(WindowsCaptureBackend::new()),
            Arc::new(ImageProcVisionMatcher::new()),
            Arc::new(AssetStore::new(asset_root)),
            DEFAULT_CAPTURE_INTERVAL,
        )
    }

    pub fn with_parts(
        window: Arc<dyn WindowProvider>,
        capture: Box<dyn CaptureBackend>,
        matcher: Arc<dyn VisionMatcher>,
        assets: Arc<AssetStore>,
        min_capture_interval: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            window,
            capture: Mutex::new(capture),
            matcher,
            assets,
            catalog: RwLock::new(HashMap::new()),
            last_capture: Mutex::new(None),
            min_capture_interval,
        })
    }

    pub fn set_assets(&self, assets: &[AutomationAsset]) {
        if let Ok(mut catalog) = self.catalog.write() {
            catalog.clear();
            for asset in assets {
                catalog.insert(asset.id.clone(), asset.clone());
            }
        }
    }

    pub fn asset_store(&self) -> Arc<AssetStore> {
        Arc::clone(&self.assets)
    }

    pub fn cache_stats(&self) -> AssetCacheStats {
        self.assets.cache_stats()
    }

    fn asset(&self, asset_id: &str) -> Result<AutomationAsset, VisionError> {
        if asset_id.trim().is_empty() {
            return Err(VisionError::new("asset_not_found", "资源 ID 不能为空"));
        }
        self.catalog
            .read()
            .map_err(|_| VisionError::new("asset_not_found", "图像资源目录状态异常"))?
            .get(asset_id)
            .cloned()
            .ok_or_else(|| {
                VisionError::new("asset_not_found", format!("找不到图像资源：{asset_id}"))
            })
    }

    fn wait_for_capture_slot(&self, cancel: &AtomicBool) -> Result<(), VisionError> {
        let wait_for = self
            .last_capture
            .lock()
            .map_err(|_| VisionError::new("capture_device_lost", "捕获限频状态异常"))?
            .and_then(|last| {
                self.min_capture_interval
                    .checked_sub(last.elapsed())
                    .filter(|duration| !duration.is_zero())
            });
        if let Some(wait_for) = wait_for {
            if !interruptible_wait(wait_for, cancel) {
                return Err(VisionError::cancelled());
            }
        }
        if let Ok(mut last) = self.last_capture.lock() {
            *last = Some(Instant::now());
        }
        Ok(())
    }

    fn capture_region(
        &self,
        region: ScreenRect,
        cancel: &AtomicBool,
    ) -> Result<CaptureFrame, VisionError> {
        if cancel.load(Ordering::SeqCst) {
            return Err(VisionError::cancelled());
        }
        region.validate()?;
        self.wait_for_capture_slot(cancel)?;
        let mut capture = self
            .capture
            .lock()
            .map_err(|_| VisionError::new("capture_device_lost", "捕获后端状态异常"))?;
        let result = capture.capture_region_with_cancel(region, cancel);
        if let Err(error) = &result {
            if matches!(
                error.code.as_str(),
                "capture_device_lost" | "capture_timeout"
            ) {
                capture.reset();
            }
        }
        result
    }

    pub fn capture_window(
        &self,
        title_query: &str,
        cancel: &AtomicBool,
    ) -> Result<CaptureFrame, VisionError> {
        if title_query.trim().is_empty() {
            return Err(VisionError::new("window_not_found", "窗口标题查询不能为空"));
        }
        let windows = self.window.find_windows(title_query)?;
        let Some(window) = windows
            .iter()
            .find(|window| window.visible)
            .or_else(|| windows.iter().find(|window| window.minimized))
        else {
            return Err(VisionError::new(
                "window_not_found",
                "找不到可捕获的目标窗口",
            ));
        };
        if window.minimized {
            return Err(VisionError::new("window_minimized", "目标窗口当前已最小化"));
        }
        if cancel.load(Ordering::SeqCst) {
            return Err(VisionError::cancelled());
        }
        self.wait_for_capture_slot(cancel)?;
        let mut capture = self
            .capture
            .lock()
            .map_err(|_| VisionError::new("capture_device_lost", "捕获后端状态异常"))?;
        let result = capture.capture_window_with_cancel(window.id, cancel);
        if let Err(error) = &result {
            if matches!(
                error.code.as_str(),
                "capture_device_lost" | "capture_timeout"
            ) {
                capture.reset();
            }
        }
        result
    }

    fn find_image_once(
        &self,
        asset_id: &str,
        region: ScreenRect,
        threshold: f32,
        cancel: &AtomicBool,
    ) -> Result<Option<ImageMatch>, VisionError> {
        let asset = self.asset(asset_id)?;
        let template = self.assets.load_template(&asset)?;
        let frame = self.capture_region(region, cancel)?;
        self.matcher.find_template(&frame, &template, threshold)
    }
}

impl VisionApi for VisionService {
    fn active_window_title(&self) -> Result<String, VisionError> {
        Ok(self
            .window
            .active_window()?
            .map(|window| window.title)
            .unwrap_or_default())
    }

    fn window_exists(&self, title_query: &str) -> Result<bool, VisionError> {
        if title_query.trim().is_empty() {
            return Ok(false);
        }
        Ok(!self.window.find_windows(title_query)?.is_empty())
    }

    fn window_rect(&self, title_query: &str) -> Result<WindowRectValue, VisionError> {
        if title_query.trim().is_empty() {
            return Ok(WindowRectValue::not_found());
        }
        Ok(self
            .window
            .window_rect(title_query)?
            .map(WindowRectValue::found)
            .unwrap_or_else(WindowRectValue::not_found))
    }

    fn wait_window(
        &self,
        title_query: &str,
        options: VisionPollOptions<'_>,
    ) -> Result<bool, VisionError> {
        let VisionPollOptions {
            timeout,
            poll,
            cancel,
            budget,
        } = options;
        validate_wait(timeout, poll)?;
        if title_query.trim().is_empty() {
            return Ok(false);
        }
        let deadline = Instant::now() + timeout;
        loop {
            budget.consume()?;
            if !self.window.find_windows(title_query)?.is_empty() {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !interruptible_wait(poll.min(remaining), cancel) {
                return Err(VisionError::cancelled());
            }
        }
    }

    fn pixel_matches(
        &self,
        point: Point,
        expected: RgbColor,
        tolerance: u8,
        cancel: &AtomicBool,
    ) -> Result<bool, VisionError> {
        let region = ScreenRect::new(point.x, point.y, 1, 1)?;
        let frame = self.capture_region(region, cancel)?;
        self.matcher
            .pixel_matches(&frame, point, expected, tolerance)
    }

    fn wait_pixel(
        &self,
        point: Point,
        expected: RgbColor,
        tolerance: u8,
        options: VisionPollOptions<'_>,
    ) -> Result<bool, VisionError> {
        let VisionPollOptions {
            timeout,
            poll,
            cancel,
            budget,
        } = options;
        validate_wait(timeout, poll)?;
        let deadline = Instant::now() + timeout;
        loop {
            budget.consume()?;
            match self.pixel_matches(point, expected, tolerance, cancel) {
                Ok(true) => return Ok(true),
                Ok(false) => {}
                Err(error)
                    if error.code == "capture_device_lost" || error.code == "capture_timeout" =>
                {
                    if Instant::now() >= deadline {
                        return Ok(false);
                    }
                }
                Err(error) => return Err(error),
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !interruptible_wait(poll.min(remaining), cancel) {
                return Err(VisionError::cancelled());
            }
        }
    }

    fn find_image(
        &self,
        asset_id: &str,
        region: ScreenRect,
        threshold: f32,
        cancel: &AtomicBool,
    ) -> Result<Option<ImageMatch>, VisionError> {
        super::vision::validate_threshold(threshold)?;
        region.validate()?;
        self.find_image_once(asset_id, region, threshold, cancel)
    }

    fn wait_image(
        &self,
        asset_id: &str,
        region: ScreenRect,
        threshold: f32,
        options: VisionPollOptions<'_>,
    ) -> Result<Option<ImageMatch>, VisionError> {
        let VisionPollOptions {
            timeout,
            poll,
            cancel,
            budget,
        } = options;
        super::vision::validate_threshold(threshold)?;
        region.validate()?;
        validate_wait(timeout, poll)?;
        let deadline = Instant::now() + timeout;
        loop {
            budget.consume()?;
            match self.find_image_once(asset_id, region, threshold, cancel) {
                Ok(Some(found)) => return Ok(Some(found)),
                Ok(None) => {}
                Err(error)
                    if error.code == "capture_device_lost" || error.code == "capture_timeout" =>
                {
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                }
                Err(error) => return Err(error),
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !interruptible_wait(poll.min(remaining), cancel) {
                return Err(VisionError::cancelled());
            }
        }
    }
}

fn validate_wait(timeout: Duration, poll: Duration) -> Result<(), VisionError> {
    let timeout_ms = timeout.as_millis();
    let poll_ms = poll.as_millis();
    if timeout_ms > u128::from(MAX_WAIT_MS) {
        return Err(VisionError::new(
            "vision_timeout_invalid",
            "等待超时不能超过 120000ms",
        ));
    }
    if poll_ms < u128::from(MIN_POLL_MS) || poll_ms > u128::from(MAX_POLL_MS) {
        return Err(VisionError::new(
            "vision_poll_invalid",
            "轮询间隔必须在 50ms 到 5000ms 之间",
        ));
    }
    Ok(())
}

fn interruptible_wait(duration: Duration, cancel: &AtomicBool) -> bool {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        if cancel.load(Ordering::SeqCst) {
            return false;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        thread::sleep(WAIT_SLICE.min(remaining));
    }
    !cancel.load(Ordering::SeqCst)
}

pub fn run_vision_diagnostic(duration: Duration) -> String {
    let started = Instant::now();
    let metrics_before = process_metrics();
    let idle_started = Instant::now();
    thread::sleep(Duration::from_millis(250));
    let idle_elapsed = idle_started.elapsed();
    let metrics_after = process_metrics();
    let mut report = String::new();
    report.push_str(&format!("idle_wall_ms={}\n", idle_elapsed.as_millis()));
    if let (Some((before_cpu, before_memory)), Some((after_cpu, after_memory))) =
        (metrics_before, metrics_after)
    {
        report.push_str(&format!(
            "idle_cpu_ms={} idle_working_set_before_bytes={} idle_working_set_after_bytes={} idle_working_set_delta_bytes={}\n",
            after_cpu.saturating_sub(before_cpu),
            before_memory,
            after_memory,
            after_memory as i128 - before_memory as i128,
        ));
    } else {
        report.push_str("idle_cpu_ms=unavailable idle_working_set_bytes=unavailable\n");
    }

    let root = std::env::temp_dir().join("autoflow-diagnostic-assets");
    let service = VisionService::new(root);
    let cancel = AtomicBool::new(false);
    let region = ScreenRect::from_parts(0, 0, 1_920, 1_080);
    let actual_capture = match service.capture_region(region, &cancel) {
        Ok(frame) => Ok(("capture_1920x1080", frame)),
        Err(error) => {
            report.push_str(&format!("capture_1920x1080_error={}\n", error));
            match virtual_screen_region() {
                Some(actual_region) if actual_region != region => service
                    .capture_region(actual_region, &cancel)
                    .map(|frame| ("capture_virtual_screen", frame)),
                _ => Err(error),
            }
        }
    };
    match actual_capture {
        Ok(frame) => {
            report.push_str(&format!(
                "{}_ms={} frame={}x{}\n",
                frame.0,
                started.elapsed().as_millis(),
                frame.1.width,
                frame.1.height
            ));
            let matcher = ImageProcVisionMatcher::new();
            let pixel_started = Instant::now();
            let mut matches = 0usize;
            for index in 0..100i32 {
                let x = frame.1.origin.x + index % 10;
                let y = frame.1.origin.y + index / 10;
                if let Some(color) = frame.1.pixel(Point { x, y }) {
                    if matcher
                        .pixel_matches(&frame.1, Point { x, y }, color, 0)
                        .unwrap_or(false)
                    {
                        matches = matches.saturating_add(1);
                    }
                }
            }
            report.push_str(&format!(
                "pixel_checks_100_ms={} matches={}\n",
                pixel_started.elapsed().as_micros() as f64 / 1_000.0,
                matches
            ));
            let template_region =
                ScreenRect::from_parts(frame.1.origin.x, frame.1.origin.y, 16, 16);
            if let Ok(template) = frame.1.crop(template_region) {
                let match_started = Instant::now();
                let result = matcher
                    .find_template(&frame.1, &template, 0.9)
                    .unwrap_or(None);
                report.push_str(&format!(
                    "template_match_small_roi_ms={} found={}\n",
                    match_started.elapsed().as_micros() as f64 / 1_000.0,
                    result.is_some()
                ));
            }
        }
        Err(error) => report.push_str(&format!("capture_diagnostic_error={}\n", error)),
    }
    let trend_started = Instant::now();
    while trend_started.elapsed() < duration {
        thread::sleep(Duration::from_millis(250));
        let stats = service.cache_stats();
        if let Some((cpu_ms, working_set_bytes)) = process_metrics() {
            report.push_str(&format!(
                "trend_elapsed_ms={} process_cpu_ms={} working_set_bytes={} cache_entries={} cache_bytes={}\n",
                trend_started.elapsed().as_millis(),
                cpu_ms,
                working_set_bytes,
                stats.entries,
                stats.bytes
            ));
        } else {
            report.push_str(&format!(
                "trend_elapsed_ms={} process_cpu_ms=unavailable working_set_bytes=unavailable cache_entries={} cache_bytes={}\n",
                trend_started.elapsed().as_millis(),
                stats.entries,
                stats.bytes
            ));
        }
    }
    report.push_str(&format!("total_ms={}\n", started.elapsed().as_millis()));
    report
}

fn process_metrics() -> Option<(u64, usize)> {
    #[cfg(windows)]
    {
        let process = unsafe { GetCurrentProcess() };
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) }
            .ok()?;
        let mut counters = PROCESS_MEMORY_COUNTERS {
            cb: u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS>()).ok()?,
            ..PROCESS_MEMORY_COUNTERS::default()
        };
        let cb = counters.cb;
        unsafe { GetProcessMemoryInfo(process, &mut counters, cb) }.ok()?;
        let cpu_100ns = filetime_value(kernel).saturating_add(filetime_value(user));
        Some((cpu_100ns / 10_000, counters.WorkingSetSize))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

fn virtual_screen_region() -> Option<ScreenRect> {
    #[cfg(windows)]
    {
        let x = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
        let y = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
        let width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
        let height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
        if width <= 0 || height <= 0 {
            return None;
        }
        ScreenRect::new(
            x,
            y,
            u32::try_from(width).ok()?,
            u32::try_from(height).ok()?,
        )
        .ok()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
fn filetime_value(value: FILETIME) -> u64 {
    (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::types::{CaptureBackend, VisionPollBudget, WindowId, WindowInfo};
    use image::codecs::png::PngEncoder;
    use image::{ColorType, ImageEncoder};
    use std::collections::VecDeque;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct MockWindowProvider {
        windows: Mutex<Vec<WindowInfo>>,
    }

    impl WindowProvider for MockWindowProvider {
        fn active_window(&self) -> Result<Option<WindowInfo>, VisionError> {
            Ok(self
                .windows
                .lock()
                .map_err(|_| VisionError::new("test", "lock"))?
                .first()
                .cloned())
        }

        fn find_windows(&self, query: &str) -> Result<Vec<WindowInfo>, VisionError> {
            let query = query.to_lowercase();
            Ok(self
                .windows
                .lock()
                .map_err(|_| VisionError::new("test", "lock"))?
                .iter()
                .filter(|window| window.title.to_lowercase().contains(&query))
                .cloned()
                .collect())
        }

        fn window_rect(&self, query: &str) -> Result<Option<ScreenRect>, VisionError> {
            Ok(self.find_windows(query)?.first().map(|window| window.rect))
        }
    }

    struct MockCaptureBackend {
        frames: VecDeque<CaptureFrame>,
        error: Option<VisionError>,
        resets: usize,
    }

    impl CaptureBackend for MockCaptureBackend {
        fn capture_region(&mut self, _region: ScreenRect) -> Result<CaptureFrame, VisionError> {
            if let Some(error) = self.error.take() {
                self.resets = self.resets.saturating_add(1);
                return Err(error);
            }
            self.frames
                .front()
                .cloned()
                .ok_or_else(|| VisionError::new("capture_timeout", "no frame"))
        }

        fn capture_window(&mut self, _window: WindowId) -> Result<CaptureFrame, VisionError> {
            self.capture_region(ScreenRect::from_parts(0, 0, 1, 1))
        }

        fn reset(&mut self) {
            self.resets = self.resets.saturating_add(1);
        }
    }

    fn service_with(frame: CaptureFrame) -> Arc<VisionService> {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        VisionService::with_parts(
            Arc::new(MockWindowProvider {
                windows: Mutex::new(vec![WindowInfo {
                    id: WindowId(1),
                    title: "Demo Window".to_string(),
                    rect: ScreenRect::from_parts(-100, 20, 10, 10),
                    visible: true,
                    minimized: false,
                    process_id: Some(42),
                }]),
            }),
            Box::new(MockCaptureBackend {
                frames: VecDeque::from([frame]),
                error: None,
                resets: 0,
            }),
            Arc::new(ImageProcVisionMatcher::new()),
            Arc::new(AssetStore::new(
                std::env::temp_dir().join(format!("autoflow-service-tests-{suffix}")),
            )),
            Duration::ZERO,
        )
    }

    fn png_image(width: u32, height: u32, pixels: &[[u8; 4]]) -> Vec<u8> {
        let mut output = Vec::new();
        PngEncoder::new(&mut output)
            .write_image(
                pixels
                    .iter()
                    .flatten()
                    .copied()
                    .collect::<Vec<_>>()
                    .as_slice(),
                width,
                height,
                ColorType::Rgba8.into(),
            )
            .expect("test png");
        output
    }

    #[test]
    fn wait_window_success_timeout_and_cancel_are_interruptible() {
        let frame =
            CaptureFrame::from_bgra(Point { x: 0, y: 0 }, 1, 1, vec![0, 0, 0, 255]).expect("frame");
        let service = service_with(frame);
        let budget = VisionPollBudget::new(None);
        let cancel = AtomicBool::new(false);
        assert!(service
            .wait_window(
                "demo",
                VisionPollOptions::new(
                    Duration::from_millis(100),
                    Duration::from_millis(50),
                    &cancel,
                    &budget,
                ),
            )
            .expect("window wait"));
        assert!(
            !service
                .wait_window(
                    "missing",
                    VisionPollOptions::new(
                        Duration::ZERO,
                        Duration::from_millis(50),
                        &cancel,
                        &budget,
                    ),
                )
                .expect("window timeout")
        );
        cancel.store(true, Ordering::SeqCst);
        assert_eq!(
            service
                .wait_window(
                    "missing",
                    VisionPollOptions::new(
                        Duration::from_millis(100),
                        Duration::from_millis(50),
                        &cancel,
                        &budget,
                    ),
                )
                .expect_err("cancel")
                .code,
            "vision_cancelled"
        );
    }

    #[test]
    fn capture_window_reports_stable_not_found_error() {
        let frame =
            CaptureFrame::from_bgra(Point { x: 0, y: 0 }, 1, 1, vec![0, 0, 0, 255]).expect("frame");
        let service = service_with(frame);
        assert_eq!(
            service
                .capture_window("missing", &AtomicBool::new(false))
                .expect_err("missing window")
                .code,
            "window_not_found"
        );
    }

    #[test]
    fn pixel_wait_success_timeout_and_cancel_work_without_a_display() {
        let frame = CaptureFrame::from_bgra(Point { x: 4, y: -2 }, 1, 1, vec![3, 2, 1, 255])
            .expect("frame");
        let service = service_with(frame);
        let budget = VisionPollBudget::new(None);
        let cancel = AtomicBool::new(false);
        assert!(service
            .wait_pixel(
                Point { x: 4, y: -2 },
                RgbColor {
                    red: 1,
                    green: 2,
                    blue: 3
                },
                0,
                VisionPollOptions::new(
                    Duration::from_millis(100),
                    Duration::from_millis(50),
                    &cancel,
                    &budget,
                ),
            )
            .expect("pixel wait"));
        cancel.store(true, Ordering::SeqCst);
        assert_eq!(
            service
                .wait_pixel(
                    Point { x: 4, y: -2 },
                    RgbColor {
                        red: 1,
                        green: 2,
                        blue: 3
                    },
                    0,
                    VisionPollOptions::new(
                        Duration::from_millis(100),
                        Duration::from_millis(50),
                        &cancel,
                        &budget,
                    ),
                )
                .expect_err("pixel cancel")
                .code,
            "vision_cancelled"
        );
    }

    #[test]
    fn image_wait_success_timeout_and_cancel_use_the_mock_capture_backend() {
        let frame = CaptureFrame::from_bgra(
            Point { x: 0, y: 0 },
            2,
            2,
            [
                3, 2, 1, 255, 60, 50, 40, 255, 80, 70, 60, 255, 120, 110, 100, 255,
            ]
            .to_vec(),
        )
        .expect("frame");
        let service = service_with(frame);
        let root = service.asset_store().root().to_path_buf();
        let asset = super::super::assets::import_asset_file(
            &root,
            &[],
            "button",
            "button.png",
            &png_image(
                2,
                2,
                &[
                    [1, 2, 3, 255],
                    [40, 50, 60, 255],
                    [70, 80, 90, 255],
                    [110, 120, 130, 255],
                ],
            ),
        )
        .expect("asset import");
        service.set_assets(std::slice::from_ref(&asset));
        let cancel = AtomicBool::new(false);
        let budget = VisionPollBudget::new(None);
        assert!(service
            .wait_image(
                &asset.id,
                ScreenRect::from_parts(0, 0, 2, 2),
                0.99,
                VisionPollOptions::new(
                    Duration::from_millis(100),
                    Duration::from_millis(50),
                    &cancel,
                    &budget,
                ),
            )
            .expect("image wait")
            .is_some());

        let mismatch = CaptureFrame::from_bgra(
            Point { x: 0, y: 0 },
            2,
            2,
            [
                130, 120, 110, 255, 100, 90, 80, 255, 50, 40, 30, 255, 20, 10, 0, 255,
            ]
            .to_vec(),
        )
        .expect("mismatch frame");
        let mismatch_service = service_with(mismatch);
        let mismatch_root = mismatch_service.asset_store().root().to_path_buf();
        let mismatch_asset = super::super::assets::import_asset_file(
            &mismatch_root,
            &[],
            "button",
            "button.png",
            &png_image(
                2,
                2,
                &[
                    [1, 2, 3, 255],
                    [40, 50, 60, 255],
                    [70, 80, 90, 255],
                    [110, 120, 130, 255],
                ],
            ),
        )
        .expect("mismatch asset import");
        mismatch_service.set_assets(std::slice::from_ref(&mismatch_asset));
        assert!(mismatch_service
            .wait_image(
                &mismatch_asset.id,
                ScreenRect::from_parts(0, 0, 2, 2),
                0.99,
                VisionPollOptions::new(
                    Duration::ZERO,
                    Duration::from_millis(50),
                    &cancel,
                    &VisionPollBudget::new(None),
                ),
            )
            .expect("image timeout")
            .is_none());
        cancel.store(true, Ordering::SeqCst);
        assert_eq!(
            service
                .wait_image(
                    &asset.id,
                    ScreenRect::from_parts(0, 0, 2, 2),
                    0.99,
                    VisionPollOptions::new(
                        Duration::from_millis(100),
                        Duration::from_millis(50),
                        &cancel,
                        &VisionPollBudget::new(None),
                    ),
                )
                .expect_err("image cancel")
                .code,
            "vision_cancelled"
        );
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(mismatch_root);
    }

    #[test]
    fn transient_capture_error_is_recovered_by_a_wait() {
        let frame =
            CaptureFrame::from_bgra(Point { x: 0, y: 0 }, 1, 1, vec![3, 2, 1, 255]).expect("frame");
        let service = VisionService::with_parts(
            Arc::new(MockWindowProvider {
                windows: Mutex::new(Vec::new()),
            }),
            Box::new(MockCaptureBackend {
                frames: VecDeque::from([frame]),
                error: Some(VisionError::new("capture_device_lost", "device reset")),
                resets: 0,
            }),
            Arc::new(ImageProcVisionMatcher::new()),
            Arc::new(AssetStore::new(
                std::env::temp_dir().join("autoflow-capture-recovery-test"),
            )),
            Duration::ZERO,
        );
        let result = service.wait_pixel(
            Point { x: 0, y: 0 },
            RgbColor {
                red: 1,
                green: 2,
                blue: 3,
            },
            0,
            VisionPollOptions::new(
                Duration::from_millis(100),
                Duration::from_millis(50),
                &AtomicBool::new(false),
                &VisionPollBudget::new(None),
            ),
        );
        assert!(result.expect("recovered wait"));
    }
}
