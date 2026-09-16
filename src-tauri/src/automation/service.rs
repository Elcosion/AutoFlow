use super::assets::{AssetCacheStats, AssetStore};
use super::capture::WindowsCaptureBackend;
use super::types::{
    AutomationAsset, CaptureBackend, CaptureFrame, ImageMatch, MatcherMode, MatcherOptions, Point,
    RgbColor, ScreenRect, VisionApi, VisionDiagnostics, VisionError, VisionMatcher,
    VisionPollOptions, VisionSearchResult, WindowProvider, WindowRectValue, MAX_POLL_MS,
    MAX_WAIT_MS, MIN_POLL_MS,
};
use super::vision::ImageProcVisionMatcher;
use super::window::WindowsWindowProvider;
use std::collections::{HashMap, HashSet};
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
const MAX_LAST_MATCHES: usize = 128;
const LAST_MATCH_TTL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LastMatchKey {
    resource_id: String,
    file_name: String,
    fingerprint: String,
    region: ScreenRect,
    threshold_bits: u32,
    mode: MatcherMode,
    max_candidates: usize,
    scale_min_bits: u32,
    scale_max_bits: u32,
    scale_step_bits: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
struct LastMatch {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    scale: f32,
    last_used: Instant,
}

pub struct VisionService {
    window: Arc<dyn WindowProvider>,
    capture: Mutex<Box<dyn CaptureBackend>>,
    matcher: Arc<dyn VisionMatcher>,
    assets: Arc<AssetStore>,
    catalog: RwLock<HashMap<String, AutomationAsset>>,
    last_matches: Mutex<HashMap<LastMatchKey, LastMatch>>,
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
            last_matches: Mutex::new(HashMap::new()),
            last_capture: Mutex::new(None),
            min_capture_interval,
        })
    }

    pub fn set_assets(&self, assets: &[AutomationAsset]) {
        // A catalog refresh may include edits, renames, deletions or a file
        // restored under the same name. Clear prepared and scaled templates
        // when the managed metadata actually changes; playback also calls
        // this method, so clearing on every call would defeat the cache.
        let catalog_changed = self
            .catalog
            .read()
            .map(|catalog| {
                let existing_ids = catalog
                    .values()
                    .map(|asset| asset.id.to_lowercase())
                    .collect::<HashSet<_>>();
                existing_ids.len() != assets.len()
                    || assets.iter().any(|asset| {
                        catalog
                            .get(&asset.id.to_lowercase())
                            .is_none_or(|current| current != asset)
                    })
            })
            .unwrap_or(true);
        if catalog_changed {
            self.assets.invalidate_all();
        }
        if let Ok(mut catalog) = self.catalog.write() {
            catalog.clear();
            for asset in assets {
                catalog.insert(asset.file_name.to_lowercase(), asset.clone());
                catalog
                    .entry(asset.id.to_lowercase())
                    .or_insert_with(|| asset.clone());
            }
        }
        let stable_assets = assets
            .iter()
            .map(|asset| (asset.id.to_lowercase(), asset.file_name.to_lowercase()))
            .collect::<HashSet<_>>();
        if let Ok(mut last_matches) = self.last_matches.lock() {
            last_matches.retain(|key, _| {
                stable_assets
                    .contains(&(key.resource_id.to_lowercase(), key.file_name.to_lowercase()))
            });
        }
    }

    pub fn asset_store(&self) -> Arc<AssetStore> {
        Arc::clone(&self.assets)
    }

    pub fn cache_stats(&self) -> AssetCacheStats {
        self.assets.cache_stats()
    }

    fn asset(&self, file_name: &str) -> Result<AutomationAsset, VisionError> {
        let file_name = file_name.trim();
        if file_name.is_empty() {
            return Err(VisionError::new("asset_not_found", "图像文件名不能为空"));
        }
        self.catalog
            .read()
            .map_err(|_| VisionError::new("asset_not_found", "图像资源目录状态异常"))?
            .get(&file_name.to_lowercase())
            .cloned()
            .ok_or_else(|| {
                VisionError::new(
                    "asset_not_found",
                    format!("找不到图像文件：{file_name}（需要包含 .png、.jpg 或 .jpeg 后缀）"),
                )
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
        file_name: &str,
        region: ScreenRect,
        threshold: f32,
        cancel: &AtomicBool,
        options: &MatcherOptions,
    ) -> Result<VisionSearchResult, VisionError> {
        let started = Instant::now();
        let asset = self.asset(file_name)?;
        let prepare_started = Instant::now();
        let prepared = self.assets.load_prepared_template(&asset)?;
        let mut diagnostics = VisionDiagnostics::for_mode(options.mode);
        diagnostics.prepare_ms = prepare_started.elapsed().as_millis() as u64;
        let key = LastMatchKey {
            resource_id: asset.id.clone(),
            file_name: asset.file_name.clone(),
            fingerprint: prepared.fingerprint().to_string(),
            region,
            threshold_bits: threshold.to_bits(),
            mode: options.mode,
            max_candidates: options.max_candidates,
            scale_min_bits: options.scale_min.to_bits(),
            scale_max_bits: options.scale_max.to_bits(),
            scale_step_bits: options.scale_step.map(f32::to_bits),
        };

        if options.prefer_last {
            if let Some(last) = self.take_last_match(&key) {
                if let Some(previous_region) = previous_match_region(region, last) {
                    diagnostics.previous_hit_used = true;
                    let capture_started = Instant::now();
                    let previous_frame = self.capture_region(previous_region, cancel);
                    diagnostics.capture_ms = diagnostics
                        .capture_ms
                        .saturating_add(capture_started.elapsed().as_millis() as u64);
                    match previous_frame {
                        Ok(frame) => {
                            let mut previous_options = options.clone();
                            previous_options.preferred_scale = Some(last.scale);
                            let result = self.matcher.find_prepared_template(
                                &frame,
                                prepared.frame(),
                                Some(prepared.as_ref()),
                                threshold,
                                &previous_options,
                            )?;
                            diagnostics.add_attempt(&result.diagnostics);
                            if let Some(image) = result.image {
                                self.remember_last_match(
                                    &key,
                                    &image,
                                    diagnostics.matched_scale.unwrap_or(1.0),
                                );
                                diagnostics.total_ms = started.elapsed().as_millis() as u64;
                                return Ok(VisionSearchResult {
                                    image: Some(image),
                                    diagnostics,
                                });
                            }
                        }
                        Err(error)
                            if matches!(
                                error.code.as_str(),
                                "capture_device_lost" | "capture_timeout"
                            ) => {}
                        Err(error) => return Err(error),
                    }
                }
            }
        }

        let capture_started = Instant::now();
        let frame = self.capture_region(region, cancel);
        diagnostics.capture_ms = diagnostics
            .capture_ms
            .saturating_add(capture_started.elapsed().as_millis() as u64);
        let frame = frame?;
        let result = self.matcher.find_prepared_template(
            &frame,
            prepared.frame(),
            Some(prepared.as_ref()),
            threshold,
            options,
        )?;
        diagnostics.add_attempt(&result.diagnostics);
        if let Some(image) = result.image {
            self.remember_last_match(&key, &image, diagnostics.matched_scale.unwrap_or(1.0));
            diagnostics.total_ms = started.elapsed().as_millis() as u64;
            return Ok(VisionSearchResult {
                image: Some(image),
                diagnostics,
            });
        }
        // Do not keep retrying a stale location after a complete miss. A
        // subsequent call must perform a fresh search rather than repeatedly
        // treating an old coordinate and size as authoritative.
        self.forget_last_match(&key);
        diagnostics.total_ms = started.elapsed().as_millis() as u64;
        Ok(VisionSearchResult {
            image: None,
            diagnostics,
        })
    }

    fn take_last_match(&self, key: &LastMatchKey) -> Option<LastMatch> {
        let mut cache = self.last_matches.lock().ok()?;
        let cached = cache.get_mut(key)?;
        if cached.last_used.elapsed() > LAST_MATCH_TTL {
            cache.remove(key);
            return None;
        }
        cached.last_used = Instant::now();
        Some(*cached)
    }

    fn remember_last_match(&self, key: &LastMatchKey, image: &ImageMatch, scale: f32) {
        let Ok(mut cache) = self.last_matches.lock() else {
            return;
        };
        cache.retain(|_, value| value.last_used.elapsed() <= LAST_MATCH_TTL);
        while cache.len() >= MAX_LAST_MATCHES {
            let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, value)| value.last_used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            cache.remove(&oldest);
        }
        cache.insert(
            key.clone(),
            LastMatch {
                x: image.x,
                y: image.y,
                width: image.width,
                height: image.height,
                scale,
                last_used: Instant::now(),
            },
        );
    }

    fn forget_last_match(&self, key: &LastMatchKey) {
        if let Ok(mut cache) = self.last_matches.lock() {
            cache.remove(key);
        }
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
        file_name: &str,
        region: ScreenRect,
        threshold: f32,
        cancel: &AtomicBool,
    ) -> Result<Option<ImageMatch>, VisionError> {
        self.find_image_diagnostic(
            file_name,
            region,
            threshold,
            cancel,
            &MatcherOptions::default(),
        )
        .map(|result| result.image)
    }

    fn find_image_diagnostic(
        &self,
        file_name: &str,
        region: ScreenRect,
        threshold: f32,
        cancel: &AtomicBool,
        options: &MatcherOptions,
    ) -> Result<VisionSearchResult, VisionError> {
        super::vision::validate_threshold(threshold)?;
        options.validate()?;
        region.validate()?;
        self.find_image_once(file_name, region, threshold, cancel, options)
    }

    fn wait_image(
        &self,
        file_name: &str,
        region: ScreenRect,
        threshold: f32,
        options: VisionPollOptions<'_>,
    ) -> Result<Option<ImageMatch>, VisionError> {
        self.wait_image_diagnostic(
            file_name,
            region,
            threshold,
            options,
            &MatcherOptions::default(),
        )
        .map(|result| result.image)
    }

    fn wait_image_diagnostic(
        &self,
        file_name: &str,
        region: ScreenRect,
        threshold: f32,
        options: VisionPollOptions<'_>,
        matcher_options: &MatcherOptions,
    ) -> Result<VisionSearchResult, VisionError> {
        let VisionPollOptions {
            timeout,
            poll,
            cancel,
            budget,
        } = options;
        super::vision::validate_threshold(threshold)?;
        matcher_options.validate()?;
        region.validate()?;
        validate_wait(timeout, poll)?;
        let deadline = Instant::now() + timeout;
        let started = Instant::now();
        let mut diagnostics = VisionDiagnostics::for_mode(matcher_options.mode);
        loop {
            budget.consume()?;
            match self.find_image_once(file_name, region, threshold, cancel, matcher_options) {
                Ok(attempt) => {
                    diagnostics.add_attempt(&attempt.diagnostics);
                    diagnostics.previous_hit_used |= attempt.diagnostics.previous_hit_used;
                    if let Some(found) = attempt.image {
                        diagnostics.total_ms = started.elapsed().as_millis() as u64;
                        return Ok(VisionSearchResult {
                            image: Some(found),
                            diagnostics,
                        });
                    }
                }
                Err(error)
                    if error.code == "capture_device_lost" || error.code == "capture_timeout" =>
                {
                    if Instant::now() >= deadline {
                        diagnostics.total_ms = started.elapsed().as_millis() as u64;
                        return Ok(VisionSearchResult {
                            image: None,
                            diagnostics,
                        });
                    }
                }
                Err(error) => return Err(error),
            }
            if cancel.load(Ordering::SeqCst) {
                return Err(VisionError::cancelled());
            }
            if Instant::now() >= deadline {
                diagnostics.total_ms = started.elapsed().as_millis() as u64;
                return Ok(VisionSearchResult {
                    image: None,
                    diagnostics,
                });
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !interruptible_wait(poll.min(remaining), cancel) {
                return Err(VisionError::cancelled());
            }
        }
    }
}

fn previous_match_region(requested: ScreenRect, last: LastMatch) -> Option<ScreenRect> {
    let scale_margin = f64::from(last.scale.max(1.0));
    let margin_x = ((f64::from(last.width) * 2.0 * scale_margin).round() as u64).max(64);
    let margin_y = ((f64::from(last.height) * 2.0 * scale_margin).round() as u64).max(64);
    let margin_x = i64::try_from(margin_x).ok()?;
    let margin_y = i64::try_from(margin_y).ok()?;
    let left = i64::from(last.x).checked_sub(margin_x)?;
    let top = i64::from(last.y).checked_sub(margin_y)?;
    let right = i64::from(last.x)
        .checked_add(i64::from(last.width))?
        .checked_add(margin_x)?;
    let bottom = i64::from(last.y)
        .checked_add(i64::from(last.height))?
        .checked_add(margin_y)?;
    let expanded = ScreenRect::from_parts(
        i32::try_from(left).ok()?,
        i32::try_from(top).ok()?,
        u32::try_from(right.checked_sub(left)?).ok()?,
        u32::try_from(bottom.checked_sub(top)?).ok()?,
    );
    let mut clipped = requested.intersection(&expanded)?;
    if let Some(virtual_screen) = virtual_screen_region() {
        clipped = clipped.intersection(&virtual_screen)?;
    }
    (clipped.width >= last.width && clipped.height >= last.height).then_some(clipped)
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
    use image::imageops::{resize, FilterType};
    use image::{ColorType, ImageEncoder};
    use std::collections::VecDeque;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicUsize;
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

    struct SequenceCaptureBackend {
        frames: VecDeque<CaptureFrame>,
        calls: Arc<AtomicUsize>,
    }

    impl CaptureBackend for SequenceCaptureBackend {
        fn capture_region(&mut self, _region: ScreenRect) -> Result<CaptureFrame, VisionError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.frames
                .pop_front()
                .ok_or_else(|| VisionError::new("capture_timeout", "no frame"))
        }

        fn capture_window(&mut self, _window: WindowId) -> Result<CaptureFrame, VisionError> {
            self.capture_region(ScreenRect::from_parts(0, 0, 1, 1))
        }

        fn reset(&mut self) {}
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

    fn service_with_sequence(
        frames: Vec<CaptureFrame>,
        calls: Arc<AtomicUsize>,
        root: PathBuf,
    ) -> Arc<VisionService> {
        VisionService::with_parts(
            Arc::new(MockWindowProvider {
                windows: Mutex::new(vec![WindowInfo {
                    id: WindowId(1),
                    title: "Demo Window".to_string(),
                    rect: ScreenRect::from_parts(0, 0, 128, 96),
                    visible: true,
                    minimized: false,
                    process_id: Some(42),
                }]),
            }),
            Box::new(SequenceCaptureBackend {
                frames: VecDeque::from(frames),
                calls,
            }),
            Arc::new(ImageProcVisionMatcher::new()),
            Arc::new(AssetStore::new(root)),
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

    fn vision_template_pixels() -> Vec<[u8; 4]> {
        (0..24_u32 * 24)
            .map(|index| {
                let value = ((index.wrapping_mul(37).wrapping_add(19)) % 251) as u8;
                [
                    value.wrapping_add(17),
                    value.wrapping_mul(3).wrapping_add(31),
                    value.wrapping_mul(5).wrapping_add(47),
                    255,
                ]
            })
            .collect()
    }

    fn screen_frame(target: (u32, u32), template: &CaptureFrame) -> CaptureFrame {
        screen_frame_sized(target, template, 128, 96)
    }

    fn screen_frame_sized(
        target: (u32, u32),
        template: &CaptureFrame,
        width: u32,
        height: u32,
    ) -> CaptureFrame {
        let mut pixels = vec![19_u8; (width * height * 4) as usize];
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel[0] = 7;
            pixel[1] = 13;
            pixel[2] = 23;
            pixel[3] = 255;
        }
        for y in 0..template.height {
            for x in 0..template.width {
                let source_index = ((y * template.width + x) * 4) as usize;
                let target_index = (((target.1 + y) * width + target.0 + x) * 4) as usize;
                pixels[target_index..target_index + 4]
                    .copy_from_slice(&template.pixels_bgra()[source_index..source_index + 4]);
            }
        }
        CaptureFrame::from_bgra(Point { x: 0, y: 0 }, width, height, pixels).expect("screen frame")
    }

    fn scaled_screen_frame(
        target: (u32, u32),
        scale: f32,
        template: &CaptureFrame,
        width: u32,
        height: u32,
    ) -> CaptureFrame {
        let mut pixels = vec![0_u8; (width * height * 4) as usize];
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.copy_from_slice(&[7, 13, 23, 255]);
        }
        let gray = ImageProcVisionMatcher::gray(template).expect("template gray");
        let scaled_width = (f64::from(template.width) * f64::from(scale)).round() as u32;
        let scaled_height = (f64::from(template.height) * f64::from(scale)).round() as u32;
        let scaled = resize(&gray, scaled_width, scaled_height, FilterType::Triangle);
        for y in 0..scaled_height {
            for x in 0..scaled_width {
                let value = scaled.get_pixel(x, y).0[0];
                let index = (((target.1 + y) * width + target.0 + x) * 4) as usize;
                pixels[index..index + 4].copy_from_slice(&[value, value, value, 255]);
            }
        }
        CaptureFrame::from_bgra(Point { x: 0, y: 0 }, width, height, pixels)
            .expect("scaled screen frame")
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
        assert_eq!(
            service.asset("BUTTON.PNG").expect("file name lookup").id,
            asset.id
        );
        assert!(service
            .asset("button")
            .expect_err("suffix is required")
            .message
            .contains("需要包含"));
        assert_eq!(
            service.asset(&asset.id).expect("legacy id lookup").id,
            asset.id
        );
        let cancel = AtomicBool::new(false);
        let budget = VisionPollBudget::new(None);
        assert!(service
            .wait_image(
                &asset.file_name,
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
                &mismatch_asset.file_name,
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
                    &asset.file_name,
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
    fn previous_hit_region_is_fast_and_full_search_recovers_after_move() {
        let root = std::env::temp_dir().join(format!(
            "autoflow-vision-last-match-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let rgba = vision_template_pixels();
        let calls = Arc::new(AtomicUsize::new(0));
        let first_template = CaptureFrame::from_bgra(
            Point { x: 0, y: 0 },
            24,
            24,
            rgba.iter()
                .flat_map(|pixel| [pixel[2], pixel[1], pixel[0], pixel[3]])
                .collect(),
        )
        .expect("template frame");
        let first = screen_frame((86, 60), &first_template);
        let moved = screen_frame((20, 10), &first_template);
        let previous_roi = moved
            .crop(ScreenRect::from_parts(22, 0, 106, 96))
            .expect("previous ROI");
        let service = service_with_sequence(
            vec![first, previous_roi, moved],
            Arc::clone(&calls),
            root.clone(),
        );
        let asset = super::super::assets::import_asset_file(
            &root,
            &[],
            "button",
            "button.png",
            &png_image(24, 24, &rgba),
        )
        .expect("asset import");
        service.set_assets(std::slice::from_ref(&asset));
        let cancel = AtomicBool::new(false);
        let first_result = service
            .find_image_diagnostic(
                &asset.file_name,
                ScreenRect::from_parts(0, 0, 128, 96),
                0.99,
                &cancel,
                &MatcherOptions::default(),
            )
            .expect("first image search");
        assert_eq!(
            first_result.image.as_ref().map(|image| (image.x, image.y)),
            Some((86, 60))
        );
        let moved_result = service
            .find_image_diagnostic(
                &asset.file_name,
                ScreenRect::from_parts(0, 0, 128, 96),
                0.99,
                &cancel,
                &MatcherOptions::default(),
            )
            .expect("moved image search");
        assert_eq!(
            moved_result.image.as_ref().map(|image| (image.x, image.y)),
            Some((20, 10))
        );
        assert!(moved_result.diagnostics.previous_hit_used);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn previous_hit_scale_is_prioritized_and_scale_changes_recover_without_old_click() {
        let root = std::env::temp_dir().join(format!(
            "autoflow-vision-scale-last-match-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let rgba = vision_template_pixels();
        let template = CaptureFrame::from_bgra(
            Point { x: 0, y: 0 },
            24,
            24,
            rgba.iter()
                .flat_map(|pixel| [pixel[2], pixel[1], pixel[0], pixel[3]])
                .collect(),
        )
        .expect("template frame");
        let requested = ScreenRect::from_parts(0, 0, 512, 256);
        let first_target = (350, 180);
        let first = screen_frame_sized(first_target, &template, 512, 256);
        let scale_changed = scaled_screen_frame(first_target, 1.5, &template, 512, 256);
        let first_last = LastMatch {
            x: first_target.0 as i32,
            y: first_target.1 as i32,
            width: 24,
            height: 24,
            scale: 1.0,
            last_used: Instant::now(),
        };
        let first_roi = previous_match_region(requested, first_last).expect("first ROI");
        let changed_roi = scale_changed.crop(first_roi).expect("changed ROI");
        let moved_target = (0, 0);
        let moved = scaled_screen_frame(moved_target, 1.5, &template, 512, 256);
        let changed_last = LastMatch {
            x: first_target.0 as i32,
            y: first_target.1 as i32,
            width: 36,
            height: 36,
            scale: 1.5,
            last_used: Instant::now(),
        };
        let moved_roi = moved
            .crop(previous_match_region(requested, changed_last).expect("moved ROI"))
            .expect("moved previous ROI");
        let calls = Arc::new(AtomicUsize::new(0));
        let service = service_with_sequence(
            vec![first, changed_roi, moved_roi, moved],
            Arc::clone(&calls),
            root.clone(),
        );
        let asset = super::super::assets::import_asset_file(
            &root,
            &[],
            "button",
            "button.png",
            &png_image(24, 24, &rgba),
        )
        .expect("asset import");
        service.set_assets(std::slice::from_ref(&asset));
        let cancel = AtomicBool::new(false);

        let first_result = service
            .find_image_diagnostic(
                &asset.file_name,
                requested,
                0.99,
                &cancel,
                &MatcherOptions::default(),
            )
            .expect("first scale search");
        let first_image = first_result.image.expect("first hit");
        assert_eq!(
            (first_image.x, first_image.y),
            (first_target.0 as i32, first_target.1 as i32)
        );
        assert_eq!((first_image.width, first_image.height), (24, 24));
        assert!((first_result.diagnostics.matched_scale.expect("first scale") - 1.0).abs() < 0.01);

        let changed_result = service
            .find_image_diagnostic(
                &asset.file_name,
                requested,
                0.99,
                &cancel,
                &MatcherOptions::default(),
            )
            .expect("same position scale change");
        let changed_image = changed_result.image.expect("changed scale hit");
        assert_eq!(
            (changed_image.x, changed_image.y),
            (first_target.0 as i32, first_target.1 as i32)
        );
        assert_eq!((changed_image.width, changed_image.height), (36, 36));
        assert!(
            (changed_result
                .diagnostics
                .matched_scale
                .expect("changed scale")
                - 1.5)
                .abs()
                < 0.01
        );
        assert!(changed_result.diagnostics.previous_hit_used);
        assert_eq!(
            changed_result.diagnostics.scale_candidates.first(),
            Some(&1.0)
        );

        let moved_result = service
            .find_image_diagnostic(
                &asset.file_name,
                requested,
                0.99,
                &cancel,
                &MatcherOptions::default(),
            )
            .expect("moved and scaled search");
        let moved_image = moved_result.image.expect("moved scale hit");
        assert_eq!(
            (moved_image.x, moved_image.y),
            (moved_target.0 as i32, moved_target.1 as i32)
        );
        assert_eq!((moved_image.width, moved_image.height), (36, 36));
        assert!((moved_result.diagnostics.matched_scale.expect("moved scale") - 1.5).abs() < 0.01);
        assert!(moved_result.diagnostics.previous_hit_used);
        assert_eq!(
            moved_result.diagnostics.scale_candidates.first(),
            Some(&1.5)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        let _ = fs::remove_dir_all(root);
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
