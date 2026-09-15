mod assets;
mod benchmark;
mod capture;
mod service;
mod types;
mod vision;
mod window;

pub use assets::{asset_file_bytes, import_asset_file, sync_asset_directory, AssetStore};
pub use benchmark::run_vision_benchmark;
pub use service::{run_vision_diagnostic, VisionService};
pub use types::{
    AutomationAsset, CaptureBackend, CaptureFrame, ImageMatch, MatcherMode, MatcherOptions,
    MatcherResult, Point, RgbColor, ScreenRect, VisionApi, VisionDiagnostics, VisionError,
    VisionMatcher, VisionPollBudget, VisionPollOptions, VisionSearchResult, WindowId, WindowInfo,
    WindowProvider, WindowRectValue, MAX_ASSET_BYTES, MAX_ASSET_NAME_LENGTH, MAX_CACHED_TEMPLATES,
    MAX_CACHED_TEMPLATE_BYTES, MAX_CAPTURE_HEIGHT, MAX_CAPTURE_PIXELS, MAX_CAPTURE_WIDTH,
    MAX_POLL_MS, MAX_TEMPLATE_HEIGHT, MAX_TEMPLATE_PIXELS, MAX_TEMPLATE_WIDTH,
    MAX_VISION_OPERATIONS, MAX_WAIT_MS, MIN_POLL_MS,
};
pub use vision::{ImageProcVisionMatcher, PreparedTemplate};

#[cfg(windows)]
pub use capture::WindowsCaptureBackend;
#[cfg(windows)]
pub use window::WindowsWindowProvider;
