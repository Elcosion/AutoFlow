use super::types::{
    AutomationAsset, CaptureFrame, Point, VisionError, MAX_ASSET_BYTES, MAX_ASSET_NAME_LENGTH,
    MAX_CACHED_TEMPLATES, MAX_TEMPLATE_HEIGHT, MAX_TEMPLATE_PIXELS, MAX_TEMPLATE_WIDTH,
};
use image::{guess_format, load_from_memory, GenericImageView, ImageFormat};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileFingerprint {
    length: u64,
    modified: Option<SystemTime>,
}

#[derive(Debug)]
struct CachedTemplate {
    fingerprint: FileFingerprint,
    frame: Arc<CaptureFrame>,
    last_used: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetCacheStats {
    pub entries: usize,
    pub bytes: usize,
}

#[derive(Debug)]
pub struct AssetStore {
    root: PathBuf,
    cache: Mutex<HashMap<String, CachedTemplate>>,
}

impl AssetStore {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn load_template(&self, asset: &AutomationAsset) -> Result<Arc<CaptureFrame>, VisionError> {
        let path = self.resolve_existing(asset)?;
        let fingerprint = file_fingerprint(&path)?;
        if let Ok(mut cache) = self.cache.lock() {
            if let Some(cached) = cache.get_mut(&asset.id) {
                if cached.fingerprint == fingerprint {
                    cached.last_used = Instant::now();
                    return Ok(Arc::clone(&cached.frame));
                }
            }
            cache.remove(&asset.id);
        }

        let bytes = fs::read(&path).map_err(|error| {
            VisionError::new(
                "asset_file_missing",
                format!("无法读取图像资源 {}：{error}", asset.name),
            )
        })?;
        let frame = Arc::new(decode_template(&bytes, asset)?);
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| VisionError::new("asset_cache_failed", "图像资源缓存状态异常"))?;
        if cache.len() >= MAX_CACHED_TEMPLATES {
            if let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, cached)| cached.last_used)
                .map(|(id, _)| id.clone())
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(
            asset.id.clone(),
            CachedTemplate {
                fingerprint,
                frame: Arc::clone(&frame),
                last_used: Instant::now(),
            },
        );
        Ok(frame)
    }

    pub fn read_bytes(&self, asset: &AutomationAsset) -> Result<Vec<u8>, VisionError> {
        let path = self.resolve_existing(asset)?;
        fs::read(path).map_err(|error| {
            VisionError::new(
                "asset_file_missing",
                format!("无法读取图像资源 {}：{error}", asset.name),
            )
        })
    }

    pub fn remove_file(&self, asset: &AutomationAsset) -> Result<(), VisionError> {
        let root = self.canonical_root()?;
        validate_file_name(&asset.file_name)?;
        let candidate = root.join(&asset.file_name);
        let canonical = match fs::canonicalize(&candidate) {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(VisionError::new(
                    "asset_file_missing",
                    format!("无法定位图像资源 {}：{error}", asset.name),
                ))
            }
        };
        if !canonical.starts_with(&root) {
            return Err(VisionError::new(
                "asset_path_unsafe",
                "图像资源路径不在 AutoFlow 托管目录中",
            ));
        }
        fs::remove_file(&canonical).map_err(|error| {
            VisionError::new(
                "asset_delete_failed",
                format!("删除图像资源 {} 失败：{error}", asset.name),
            )
        })?;
        if let Ok(mut cache) = self.cache.lock() {
            cache.remove(&asset.id);
        }
        Ok(())
    }

    pub fn cache_stats(&self) -> AssetCacheStats {
        self.cache
            .lock()
            .map(|cache| AssetCacheStats {
                entries: cache.len(),
                bytes: cache
                    .values()
                    .map(|item| item.frame.pixels_bgra().len())
                    .sum(),
            })
            .unwrap_or(AssetCacheStats {
                entries: 0,
                bytes: 0,
            })
    }

    fn canonical_root(&self) -> Result<PathBuf, VisionError> {
        fs::create_dir_all(&self.root).map_err(|error| {
            VisionError::new(
                "asset_directory_failed",
                format!("无法创建图像资源目录：{error}"),
            )
        })?;
        fs::canonicalize(&self.root).map_err(|error| {
            VisionError::new(
                "asset_directory_failed",
                format!("无法定位图像资源目录：{error}"),
            )
        })
    }

    fn resolve_existing(&self, asset: &AutomationAsset) -> Result<PathBuf, VisionError> {
        let root = self.canonical_root()?;
        validate_file_name(&asset.file_name)?;
        let candidate = root.join(&asset.file_name);
        let canonical = fs::canonicalize(&candidate).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                VisionError::new(
                    "asset_file_missing",
                    format!("图像资源“{}”文件不存在", asset.name),
                )
            } else {
                VisionError::new(
                    "asset_file_missing",
                    format!("无法读取图像资源“{}”：{error}", asset.name),
                )
            }
        })?;
        if !canonical.starts_with(&root) {
            return Err(VisionError::new(
                "asset_path_unsafe",
                "图像资源路径不在 AutoFlow 托管目录中",
            ));
        }
        Ok(canonical)
    }
}

pub fn import_asset_file(
    root: &Path,
    occupied_ids: &[String],
    name: &str,
    original_file_name: &str,
    bytes: &[u8],
) -> Result<AutomationAsset, VisionError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_ASSET_NAME_LENGTH {
        return Err(VisionError::new(
            "asset_name_invalid",
            "图像资源名称不能为空且不能超过 128 个字符",
        ));
    }
    if bytes.is_empty() || bytes.len() > MAX_ASSET_BYTES {
        return Err(VisionError::new(
            "asset_decode_failed",
            "图像文件过大或为空",
        ));
    }
    let format = guess_format(bytes)
        .map_err(|_| VisionError::new("asset_decode_failed", "只支持有效的 PNG 或 JPEG 图像"))?;
    let extension = match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
        _ => {
            return Err(VisionError::new(
                "asset_decode_failed",
                "只支持 PNG 或 JPEG 图像",
            ))
        }
    };
    let decoded = load_from_memory(bytes).map_err(|error| {
        VisionError::new("asset_decode_failed", format!("图像解码失败：{error}"))
    })?;
    let (width, height) = decoded.dimensions();
    validate_template_dimensions(width, height)?;
    let digest = Sha256::digest(bytes);
    let digest_text = format!("{digest:x}");
    let base_id = format!("{}-{}", slugify(name), &digest_text[..12]);
    let mut id = base_id.clone();
    let mut suffix = 2usize;
    while occupied_ids.iter().any(|occupied| occupied == &id) {
        id = format!("{base_id}-{suffix}");
        suffix = suffix.saturating_add(1);
    }
    let file_name = format!("{id}.{extension}");
    validate_file_name(&file_name)?;
    fs::create_dir_all(root).map_err(|error| {
        VisionError::new(
            "asset_directory_failed",
            format!("无法创建图像资源目录：{error}"),
        )
    })?;
    let canonical_root = fs::canonicalize(root).map_err(|error| {
        VisionError::new(
            "asset_directory_failed",
            format!("无法定位图像资源目录：{error}"),
        )
    })?;
    let destination = canonical_root.join(&file_name);
    if !destination.starts_with(&canonical_root) {
        return Err(VisionError::new(
            "asset_path_unsafe",
            "生成的图像资源路径无效",
        ));
    }
    let temporary = destination.with_extension(format!("{extension}.tmp"));
    fs::write(&temporary, bytes).map_err(|error| {
        VisionError::new("asset_write_failed", format!("图像资源写入失败：{error}"))
    })?;
    if let Err(error) = fs::rename(&temporary, &destination) {
        let _ = fs::remove_file(&temporary);
        return Err(VisionError::new(
            "asset_write_failed",
            format!("图像资源保存失败：{error}"),
        ));
    }
    let _ = original_file_name;
    Ok(AutomationAsset {
        id,
        name: name.to_string(),
        file_name,
        width,
        height,
        sha256: Some(digest_text),
    })
}

pub fn asset_file_bytes(root: &Path, asset: &AutomationAsset) -> Result<Vec<u8>, VisionError> {
    AssetStore::new(root.to_path_buf()).read_bytes(asset)
}

fn decode_template(bytes: &[u8], asset: &AutomationAsset) -> Result<CaptureFrame, VisionError> {
    let image = load_from_memory(bytes).map_err(|error| {
        VisionError::new(
            "asset_decode_failed",
            format!("图像资源“{}”解码失败：{error}", asset.name),
        )
    })?;
    let (width, height) = image.dimensions();
    validate_template_dimensions(width, height)?;
    let rgba = image.to_rgba8();
    let pixels = rgba
        .pixels()
        .flat_map(|pixel| [pixel[2], pixel[1], pixel[0], pixel[3]])
        .collect::<Vec<_>>();
    CaptureFrame::from_bgra(Point { x: 0, y: 0 }, width, height, pixels)
}

fn validate_template_dimensions(width: u32, height: u32) -> Result<(), VisionError> {
    if width == 0
        || height == 0
        || width > MAX_TEMPLATE_WIDTH
        || height > MAX_TEMPLATE_HEIGHT
        || u64::from(width) * u64::from(height) > MAX_TEMPLATE_PIXELS
    {
        return Err(VisionError::new(
            "asset_decode_failed",
            "图像模板尺寸超过 2048×2048 或允许的像素上限",
        ));
    }
    Ok(())
}

fn file_fingerprint(path: &Path) -> Result<FileFingerprint, VisionError> {
    let metadata = fs::metadata(path).map_err(|error| {
        VisionError::new("asset_file_missing", format!("图像资源文件不存在：{error}"))
    })?;
    Ok(FileFingerprint {
        length: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

fn validate_file_name(file_name: &str) -> Result<(), VisionError> {
    let path = Path::new(file_name);
    if file_name.is_empty()
        || path.is_absolute()
        || file_name.contains('/')
        || file_name.contains('\\')
        || file_name.split(['/', '\\']).any(|part| part == "..")
    {
        return Err(VisionError::new(
            "asset_path_unsafe",
            "图像资源文件名必须是托管目录中的安全单级文件名",
        ));
    }
    Ok(())
}

fn slugify(value: &str) -> String {
    let mut result = String::new();
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            result.push(character.to_ascii_lowercase());
        } else if !result.ends_with('_') {
            result.push('_');
        }
        if result.len() >= 48 {
            break;
        }
    }
    let result = result.trim_matches('_').to_string();
    if result.is_empty() {
        "asset".to_string()
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::png::PngEncoder;
    use image::{ColorType, ImageEncoder};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root() -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("autoflow-asset-test-{suffix}"))
    }

    fn png(pixel: [u8; 4]) -> Vec<u8> {
        let mut output = Vec::new();
        PngEncoder::new(&mut output)
            .write_image(&pixel, 1, 1, ColorType::Rgba8.into())
            .expect("test png");
        output
    }

    #[test]
    fn asset_path_traversal_is_rejected() {
        let root = temp_root();
        let asset = AutomationAsset {
            id: "unsafe".to_string(),
            name: "不安全".to_string(),
            file_name: "..\\secret.png".to_string(),
            width: 1,
            height: 1,
            sha256: None,
        };
        let error = AssetStore::new(root)
            .read_bytes(&asset)
            .expect_err("path traversal");
        assert_eq!(error.code, "asset_path_unsafe");
    }

    #[test]
    fn template_cache_hits_and_invalidates_after_file_change() {
        let root = temp_root();
        let bytes = png([1, 2, 3, 255]);
        let asset = import_asset_file(&root, &[], "button", "button.png", &bytes).expect("import");
        let store = AssetStore::new(root.clone());
        let first = store.load_template(&asset).expect("first load");
        let second = store.load_template(&asset).expect("cache load");
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(store.cache_stats().entries, 1);
        std::thread::sleep(std::time::Duration::from_millis(5));
        let replacement = png([8, 9, 10, 255]);
        fs::write(root.join(&asset.file_name), replacement).expect("replace");
        let third = store.load_template(&asset).expect("invalidated load");
        assert!(!Arc::ptr_eq(&first, &third));
        let _ = fs::remove_dir_all(root);
    }
}
