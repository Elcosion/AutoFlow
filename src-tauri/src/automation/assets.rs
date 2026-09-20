use super::types::{
    AutomationAsset, CaptureFrame, Point, VisionError, MAX_ASSET_BYTES, MAX_ASSET_NAME_LENGTH,
    MAX_CACHED_TEMPLATES, MAX_CACHED_TEMPLATE_BYTES, MAX_TEMPLATE_HEIGHT, MAX_TEMPLATE_PIXELS,
    MAX_TEMPLATE_WIDTH,
};
use super::vision::PreparedTemplate;
use image::{guess_format, load_from_memory, GenericImageView, ImageFormat};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileFingerprint {
    length: u64,
    modified: Option<SystemTime>,
    sha256: [u8; 32],
}

#[derive(Debug)]
struct CachedTemplate {
    file_name: String,
    fingerprint: FileFingerprint,
    prepared: Arc<PreparedTemplate>,
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
        Ok(Arc::clone(self.load_prepared_template(asset)?.frame()))
    }

    pub fn load_prepared_template(
        &self,
        asset: &AutomationAsset,
    ) -> Result<Arc<PreparedTemplate>, VisionError> {
        let path = match self.resolve_existing(asset) {
            Ok(path) => path,
            Err(error) => {
                self.invalidate_asset(&asset.id);
                return Err(error);
            }
        };
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.invalidate_asset(&asset.id);
                return Err(VisionError::new(
                    "asset_file_missing",
                    format!("无法读取图像资源 {}：{error}", asset.name),
                ));
            }
        };
        let fingerprint = file_fingerprint(&path, &bytes)?;
        if let Ok(mut cache) = self.cache.lock() {
            if let Some(cached) = cache.get_mut(&asset.id) {
                if cached.file_name == asset.file_name && cached.fingerprint == fingerprint {
                    cached.last_used = Instant::now();
                    return Ok(Arc::clone(&cached.prepared));
                }
            }
            cache.remove(&asset.id);
        }

        let frame = Arc::new(decode_template(&bytes, asset)?);
        let prepared = Arc::new(PreparedTemplate::from_frame(
            &asset.id,
            &asset.file_name,
            fingerprint
                .sha256
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            Arc::clone(&frame),
        )?);
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| VisionError::new("asset_cache_failed", "图像资源缓存状态异常"))?;
        let prepared_bytes = prepared.memory_bytes();
        if prepared_bytes <= MAX_CACHED_TEMPLATE_BYTES {
            while cache.len() >= MAX_CACHED_TEMPLATES
                || cache
                    .values()
                    .map(|item| item.prepared.memory_bytes())
                    .sum::<usize>()
                    .saturating_add(prepared_bytes)
                    > MAX_CACHED_TEMPLATE_BYTES
            {
                let Some(oldest) = cache
                    .iter()
                    .min_by_key(|(_, cached)| cached.last_used)
                    .map(|(id, _)| id.clone())
                else {
                    break;
                };
                cache.remove(&oldest);
            }
            cache.insert(
                asset.id.clone(),
                CachedTemplate {
                    file_name: asset.file_name.clone(),
                    fingerprint,
                    prepared: Arc::clone(&prepared),
                    last_used: Instant::now(),
                },
            );
        }
        Ok(prepared)
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

    /// Refreshing the managed asset catalog invalidates both the raw prepared
    /// template and every lazily-created scaled template below it. The next
    /// lookup will rebuild them from the current file fingerprint.
    pub fn invalidate_all(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
        }
    }

    fn invalidate_asset(&self, asset_id: &str) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.remove(asset_id);
        }
    }

    pub fn remove_file(&self, asset: &AutomationAsset) -> Result<(), VisionError> {
        let root = self.canonical_root()?;
        validate_file_name(&asset.file_name)?;
        let candidate = root.join(&asset.file_name);
        let canonical = match fs::canonicalize(&candidate) {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.invalidate_asset(&asset.id);
                return Ok(());
            }
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

    pub fn rename_file(
        &self,
        asset: &AutomationAsset,
        next_file_name: &str,
    ) -> Result<(), VisionError> {
        validate_file_name(next_file_name)?;
        let root = self.canonical_root()?;
        let current = self.resolve_existing(asset)?;
        let destination = root.join(next_file_name);
        if destination.exists() {
            return Err(VisionError::new(
                "asset_duplicate_file",
                format!("图像文件已存在：{next_file_name}"),
            ));
        }
        fs::rename(&current, &destination).map_err(|error| {
            VisionError::new(
                "asset_rename_failed",
                format!("图像文件重命名失败：{error}"),
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
                    .map(|item| item.prepared.memory_bytes())
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
    occupied_keys: &[String],
    _name: &str,
    original_file_name: &str,
    bytes: &[u8],
) -> Result<AutomationAsset, VisionError> {
    let requested_file_name = original_file_name.trim();
    validate_file_name(requested_file_name)?;
    if requested_file_name.chars().count() > MAX_ASSET_NAME_LENGTH {
        return Err(VisionError::new(
            "asset_name_invalid",
            "图像资源文件名不能为空且不能超过 128 个字符",
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
        ImageFormat::Png => ["png"].as_slice(),
        ImageFormat::Jpeg => ["jpg", "jpeg"].as_slice(),
        _ => {
            return Err(VisionError::new(
                "asset_decode_failed",
                "只支持 PNG 或 JPEG 图像",
            ))
        }
    };
    let requested_extension = Path::new(requested_file_name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if !extension
        .iter()
        .any(|allowed| requested_extension.eq_ignore_ascii_case(allowed))
    {
        return Err(VisionError::new(
            "asset_extension_mismatch",
            "图像文件扩展名与实际格式不一致",
        ));
    }
    let decoded = load_from_memory(bytes).map_err(|error| {
        VisionError::new("asset_decode_failed", format!("图像解码失败：{error}"))
    })?;
    let (width, height) = decoded.dimensions();
    validate_template_dimensions(width, height)?;
    let digest = Sha256::digest(bytes);
    let digest_text = format!("{digest:x}");
    fs::create_dir_all(root).map_err(|error| {
        VisionError::new(
            "asset_directory_failed",
            format!("无法创建图像资源目录：{error}"),
        )
    })?;
    let file_name = unique_file_name(root, occupied_keys, requested_file_name)?;
    let stem = Path::new(&file_name)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("image");
    let base_id = format!("{}-{}", slugify(stem), &digest_text[..12]);
    let id = unique_id(occupied_keys, &base_id);
    validate_file_name(&file_name)?;
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
    let temporary = destination.with_extension(format!("{requested_extension}.tmp"));
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
    Ok(AutomationAsset {
        id,
        name: file_name.clone(),
        file_name,
        width,
        height,
        sha256: Some(digest_text),
    })
}

pub fn sync_asset_directory(
    root: &Path,
    indexed: &[AutomationAsset],
) -> Result<(Vec<AutomationAsset>, bool), VisionError> {
    fs::create_dir_all(root).map_err(|error| {
        VisionError::new(
            "asset_directory_failed",
            format!("无法创建图像资源目录：{error}"),
        )
    })?;
    let mut files = fs::read_dir(root)
        .map_err(|error| {
            VisionError::new(
                "asset_directory_failed",
                format!("无法读取图像资源目录：{error}"),
            )
        })?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let file_type = entry.file_type().ok()?;
            if !file_type.is_file() {
                return None;
            }
            let file_name = entry.file_name().to_string_lossy().into_owned();
            supported_image_file_name(&file_name).then_some((file_name, entry.path()))
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|(file_name, _)| file_name.to_lowercase());

    let indexed_by_file = indexed
        .iter()
        .map(|asset| (asset.file_name.to_lowercase(), asset))
        .collect::<HashMap<_, _>>();
    let mut used_ids = HashSet::new();
    let mut assets = Vec::new();
    for (file_name, path) in files {
        let bytes = match fs::read(&path) {
            Ok(bytes) if !bytes.is_empty() && bytes.len() <= MAX_ASSET_BYTES => bytes,
            Ok(_) => {
                log::warn!("忽略过大或为空的图像资源文件: {file_name}");
                continue;
            }
            Err(error) => {
                log::warn!("无法读取图像资源文件 {file_name}: {error}");
                continue;
            }
        };
        let format = match guess_format(&bytes) {
            Ok(format) if image_format_matches_file_name(format, &file_name) => format,
            Ok(_) => {
                log::warn!("忽略扩展名与实际格式不一致的图像资源文件: {file_name}");
                continue;
            }
            Err(error) => {
                log::warn!("忽略无法识别格式的图像资源文件 {file_name}: {error}");
                continue;
            }
        };
        if !matches!(format, ImageFormat::Png | ImageFormat::Jpeg) {
            log::warn!("忽略不支持格式的图像资源文件: {file_name}");
            continue;
        }
        let image = match load_from_memory(&bytes) {
            Ok(image) => image,
            Err(error) => {
                log::warn!("忽略无法解码的图像资源文件 {file_name}: {error}");
                continue;
            }
        };
        let (width, height) = image.dimensions();
        if let Err(error) = validate_template_dimensions(width, height) {
            log::warn!("忽略尺寸无效的图像资源文件 {file_name}: {}", error.message);
            continue;
        }
        let digest_text = format!("{:x}", Sha256::digest(&bytes));
        let old = indexed_by_file.get(&file_name.to_lowercase()).copied();
        let base_id = old.map(|asset| asset.id.clone()).unwrap_or_else(|| {
            let stem = Path::new(&file_name)
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("image");
            format!("{}-{}", slugify(stem), &digest_text[..12])
        });
        let id = unique_id_in_set(&used_ids, &base_id);
        used_ids.insert(id.clone());
        assets.push(AutomationAsset {
            id,
            name: file_name.clone(),
            file_name,
            width,
            height,
            sha256: Some(digest_text),
        });
    }
    let changed = assets != indexed;
    Ok((assets, changed))
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

fn file_fingerprint(path: &Path, bytes: &[u8]) -> Result<FileFingerprint, VisionError> {
    let metadata = fs::metadata(path).map_err(|error| {
        VisionError::new("asset_file_missing", format!("图像资源文件不存在：{error}"))
    })?;
    Ok(FileFingerprint {
        length: metadata.len(),
        modified: metadata.modified().ok(),
        sha256: Sha256::digest(bytes).into(),
    })
}

fn validate_file_name(file_name: &str) -> Result<(), VisionError> {
    let path = Path::new(file_name);
    if file_name.is_empty()
        || path.is_absolute()
        || file_name.contains('/')
        || file_name.contains('\\')
        || file_name.split(['/', '\\']).any(|part| part == "..")
        || file_name.ends_with(' ')
        || file_name.ends_with('.')
        || file_name
            .chars()
            .any(|character| character.is_control() || "<>:\"|?*".contains(character))
    {
        return Err(VisionError::new(
            "asset_path_unsafe",
            "图像资源文件名必须是托管目录中的安全单级文件名",
        ));
    }
    Ok(())
}

fn supported_image_file_name(file_name: &str) -> bool {
    Path::new(file_name)
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            ["png", "jpg", "jpeg"]
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
        })
}

fn image_format_matches_file_name(format: ImageFormat, file_name: &str) -> bool {
    let extension = Path::new(file_name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    match format {
        ImageFormat::Png => extension.eq_ignore_ascii_case("png"),
        ImageFormat::Jpeg => {
            extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg")
        }
        _ => false,
    }
}

fn unique_file_name(
    root: &Path,
    occupied_keys: &[String],
    requested: &str,
) -> Result<String, VisionError> {
    let path = Path::new(requested);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| VisionError::new("asset_name_invalid", "图像资源文件名无效"))?;
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .ok_or_else(|| VisionError::new("asset_name_invalid", "图像资源缺少扩展名"))?;
    let occupied = |candidate: &str| {
        occupied_keys
            .iter()
            .any(|value| value.eq_ignore_ascii_case(candidate))
            || root.join(candidate).exists()
    };
    if !occupied(requested) {
        return Ok(requested.to_string());
    }
    let mut suffix = 2usize;
    loop {
        let candidate = format!("{stem} ({suffix}).{extension}");
        validate_file_name(&candidate)?;
        if !occupied(&candidate) {
            return Ok(candidate);
        }
        suffix = suffix.saturating_add(1);
    }
}

fn unique_id(occupied_keys: &[String], base: &str) -> String {
    let occupied = occupied_keys
        .iter()
        .map(|value| value.to_lowercase())
        .collect::<HashSet<_>>();
    unique_id_in_set(&occupied, base)
}

fn unique_id_in_set(occupied: &HashSet<String>, base: &str) -> String {
    if !occupied.contains(&base.to_lowercase()) {
        return base.to_string();
    }
    let mut suffix = 2usize;
    loop {
        let candidate = format!("{base}-{suffix}");
        if !occupied.contains(&candidate.to_lowercase()) {
            return candidate;
        }
        suffix = suffix.saturating_add(1);
    }
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
        fs::remove_file(root.join(&asset.file_name)).expect("delete asset");
        let missing = store
            .load_prepared_template(&asset)
            .expect_err("deleted asset");
        assert_eq!(missing.code, "asset_file_missing");
        assert_eq!(store.cache_stats().entries, 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn import_preserves_file_name_and_avoids_overwriting_duplicates() {
        let root = temp_root();
        let bytes = png([1, 2, 3, 255]);
        let first =
            import_asset_file(&root, &[], "按钮", "确认按钮.png", &bytes).expect("first import");
        assert_eq!(first.file_name, "确认按钮.png");
        assert_eq!(first.name, first.file_name);
        let occupied = [first.id.clone(), first.file_name.clone()];
        let second = import_asset_file(&root, &occupied, "按钮", "确认按钮.png", &bytes)
            .expect("second import");
        assert_eq!(second.file_name, "确认按钮 (2).png");
        assert!(root.join(&first.file_name).exists());
        assert!(root.join(&second.file_name).exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn direct_folder_images_are_discovered_by_file_name() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("asset directory");
        fs::write(root.join("button.png"), png([8, 9, 10, 255])).expect("direct image");
        fs::write(root.join("notes.txt"), b"ignored").expect("unrelated file");
        let (assets, changed) = sync_asset_directory(&root, &[]).expect("scan");
        assert!(changed);
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].file_name, "button.png");
        assert_eq!(assets[0].name, "button.png");
        let (unchanged, changed) = sync_asset_directory(&root, &assets).expect("rescan");
        assert!(!changed);
        assert_eq!(unchanged[0].id, assets[0].id);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn import_requires_a_matching_supported_extension() {
        let root = temp_root();
        let bytes = png([1, 2, 3, 255]);
        let missing =
            import_asset_file(&root, &[], "按钮", "button", &bytes).expect_err("missing extension");
        assert_eq!(missing.code, "asset_extension_mismatch");
        let mismatch = import_asset_file(&root, &[], "按钮", "button.jpg", &bytes)
            .expect_err("mismatched extension");
        assert_eq!(mismatch.code, "asset_extension_mismatch");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rename_changes_the_actual_file_name() {
        let root = temp_root();
        let bytes = png([1, 2, 3, 255]);
        let asset = import_asset_file(&root, &[], "按钮", "before.png", &bytes).expect("import");
        let store = AssetStore::new(root.clone());
        store.load_prepared_template(&asset).expect("cache asset");
        store.rename_file(&asset, "after.png").expect("rename");
        assert!(!root.join("before.png").exists());
        assert!(root.join("after.png").exists());
        assert_eq!(store.cache_stats().entries, 0);
        let _ = fs::remove_dir_all(root);
    }
}
