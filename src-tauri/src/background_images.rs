//! The window background a user picks in Appearance settings.
//!
//! The renderer decodes the picked file and cuts it into a ladder of sizes
//! (`src/lib/backgroundImage.ts`): the browser engine is the decoder that knows
//! every format the platform does, HEIC on macOS included. The host only keeps
//! those tiers and hands one back as a `data:` URL — the CSP is
//! `img-src 'self' data:`, so there is no URL a file on disk could be shown by.
//!
//! One image is a directory, `background-images/<id>/`, holding one file per
//! tier named `<width>x<height>.<jpg|png>`. Tiers arrive one call at a time into
//! a `.staging-<upload>` directory (a whole ladder in one message would outgrow
//! the browser-dev bridge's frame limit) and become an image only when committed
//! by a rename, so a half-finished upload is never mistaken for one.
//!
//! The images are the user's library of backgrounds: an import stays until the
//! user removes it, whichever background the settings point at. The pictures
//! that ship with the app are not here; the renderer bundles them.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use base64::Engine as _;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::content_store::read_regular_file;

const DIRECTORY: &str = "background-images";
const STAGING_PREFIX: &str = ".staging-";
/// A 7680-wide JPEG of a noisy photo stays well under this.
pub const MAX_TIER_BYTES: usize = 24 * 1024 * 1024;
pub const MAX_TIER_DIMENSION: u32 = 16_384;
pub const MAX_TIERS: usize = 12;
/// An upload left behind by a crashed import; one still being written is minutes old at most.
const STALE_STAGING_AGE: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundImage {
    pub id: String,
    /// The largest tier's size, which is the image's own size unless it was over the ladder's top.
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundImageData {
    pub data_url: String,
    pub width: u32,
    pub height: u32,
    /// No larger tier exists, so a bigger window has nothing better to ask for.
    pub largest: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TierFormat {
    Jpeg,
    Png,
}

impl TierFormat {
    fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
        }
    }

    fn mime(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
        }
    }

    fn from_extension(extension: &str) -> Option<Self> {
        match extension {
            "jpg" => Some(Self::Jpeg),
            "png" => Some(Self::Png),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Tier {
    width: u32,
    height: u32,
    format: TierFormat,
    path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct BackgroundImageStore {
    root: PathBuf,
}

impl BackgroundImageStore {
    pub fn new(app_data: &Path) -> Self {
        Self {
            root: app_data.join(DIRECTORY),
        }
    }

    /// Adds one tier to an upload, opening a new upload when `upload_id` is `None`.
    /// Returns the upload's id for the next call.
    pub fn put(&self, upload_id: Option<&str>, bytes: &[u8]) -> Result<String, String> {
        let (format, width, height) = inspect_tier(bytes)?;
        let upload_id = match upload_id {
            Some(id) => {
                validate_upload_id(id)?;
                id.to_owned()
            }
            None => uuid::Uuid::new_v4().simple().to_string(),
        };
        let staging = self.root.join(format!("{STAGING_PREFIX}{upload_id}"));
        fs::create_dir_all(&staging)
            .map_err(|error| format!("无法创建背景图片暂存目录: {error}"))?;
        let existing = list_tiers(&staging)?;
        if existing.len() >= MAX_TIERS {
            return Err(format!("背景图片最多 {MAX_TIERS} 级分辨率"));
        }
        if existing
            .iter()
            .any(|tier| tier.width == width && tier.height == height)
        {
            return Err(format!("背景图片已有 {width}×{height} 这一级"));
        }
        let name = format!("{width}x{height}.{}", format.extension());
        fs::write(staging.join(name), bytes)
            .map_err(|error| format!("无法写入背景图片: {error}"))?;
        Ok(upload_id)
    }

    /// Turns an upload into an image of the library.
    pub fn commit(&self, upload_id: &str) -> Result<BackgroundImage, String> {
        validate_upload_id(upload_id)?;
        let staging = self.root.join(format!("{STAGING_PREFIX}{upload_id}"));
        let tiers = list_tiers(&staging)?;
        let largest = tiers
            .last()
            .cloned()
            .ok_or_else(|| "背景图片上传里没有任何一级分辨率".to_owned())?;
        let aspect = f64::from(largest.width) / f64::from(largest.height);
        for tier in &tiers {
            // Each tier is the same picture; a rounding pixel apart is all a resize may add.
            let expected = f64::from(tier.width) / aspect;
            if (f64::from(tier.height) - expected).abs() > 1.5 {
                return Err("背景图片的各级分辨率宽高比不一致".into());
            }
        }
        let mut hasher = Sha256::new();
        for tier in &tiers {
            let bytes = read_regular_file(&tier.path, MAX_TIER_BYTES, "Background image")
                .map_err(|error| format!("无法读取背景图片: {error}"))?;
            hasher.update(format!("{}x{}\n", tier.width, tier.height));
            hasher.update(Sha256::digest(&bytes));
        }
        let id: String = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let destination = self.root.join(&id);
        if destination.is_dir() {
            // The same ladder was imported before; the staged copy is redundant.
            let _ = fs::remove_dir_all(&staging);
        } else {
            fs::rename(&staging, &destination)
                .map_err(|error| format!("无法保存背景图片: {error}"))?;
        }
        self.prune_staging(STALE_STAGING_AGE);
        Ok(BackgroundImage {
            id,
            width: largest.width,
            height: largest.height,
        })
    }

    /// The smallest tier that covers a `viewport_width`×`viewport_height` device-pixel
    /// window without being scaled up, or the largest one when none does.
    pub fn data(
        &self,
        id: &str,
        viewport_width: u32,
        viewport_height: u32,
    ) -> Result<BackgroundImageData, String> {
        validate_image_id(id)?;
        let tiers = list_tiers(&self.root.join(id))?;
        let chosen = choose_tier(&tiers, viewport_width, viewport_height)
            .ok_or_else(|| "背景图片不存在".to_owned())?;
        let tier = &tiers[chosen];
        let bytes = read_regular_file(&tier.path, MAX_TIER_BYTES, "Background image")
            .map_err(|error| format!("无法读取背景图片: {error}"))?;
        if inspect_tier(&bytes)? != (tier.format, tier.width, tier.height) {
            return Err("背景图片已损坏".into());
        }
        Ok(BackgroundImageData {
            data_url: format!(
                "data:{};base64,{}",
                tier.format.mime(),
                base64::engine::general_purpose::STANDARD.encode(&bytes)
            ),
            width: tier.width,
            height: tier.height,
            largest: chosen + 1 == tiers.len(),
        })
    }

    /// Every image in the library, the most recently imported first.
    pub fn list(&self) -> Result<Vec<BackgroundImage>, String> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("无法读取背景图片目录: {error}")),
        };
        let mut images = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if validate_image_id(name).is_err() || !entry.path().is_dir() {
                continue;
            }
            // A directory without a tier is not an image; the renderer could not show it.
            let Some(largest) = list_tiers(&entry.path())?.pop() else {
                continue;
            };
            // Tiers are never written again once staged, and the largest is sent last,
            // so its file's time is when the picture was imported.
            let imported = fs::metadata(&largest.path)
                .and_then(|metadata| metadata.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            images.push((
                imported,
                BackgroundImage {
                    id: name.to_owned(),
                    width: largest.width,
                    height: largest.height,
                },
            ));
        }
        images.sort_by(|(left_time, left), (right_time, right)| {
            right_time.cmp(left_time).then_with(|| left.id.cmp(&right.id))
        });
        Ok(images.into_iter().map(|(_, image)| image).collect())
    }

    /// Removes an image from the library. One that is already gone is not an error.
    pub fn delete(&self, id: &str) -> Result<(), String> {
        validate_image_id(id)?;
        match fs::remove_dir_all(self.root.join(id)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("无法删除背景图片: {error}")),
        }
    }

    /// Startup pass: an import the app quit in the middle of leaves a staging directory.
    pub fn reconcile(&self) {
        self.prune_staging(Duration::ZERO);
    }

    fn prune_staging(&self, staging_age: Duration) {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return;
        };
        let now = SystemTime::now();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let path = entry.path();
            if !name.starts_with(STAGING_PREFIX) || !path.is_dir() {
                continue;
            }
            let age = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .unwrap_or(Duration::MAX);
            if age >= staging_age {
                if let Err(error) = fs::remove_dir_all(&path) {
                    eprintln!("未能删除残留的背景图片上传 {name}：{error}");
                }
            }
        }
    }
}

fn choose_tier(tiers: &[Tier], viewport_width: u32, viewport_height: u32) -> Option<usize> {
    if tiers.is_empty() {
        return None;
    }
    let width = viewport_width.max(1);
    let height = viewport_height.max(1);
    Some(
        tiers
            .iter()
            .position(|tier| tier.width >= width && tier.height >= height)
            .unwrap_or(tiers.len() - 1),
    )
}

/// Tiers in a directory, smallest first. Files that are not tiers are ignored.
fn list_tiers(directory: &Path) -> Result<Vec<Tier>, String> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("无法读取背景图片目录: {error}")),
    };
    let mut tiers = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some((size, extension)) = name.split_once('.') else {
            continue;
        };
        let Some(format) = TierFormat::from_extension(extension) else {
            continue;
        };
        let Some((width, height)) = size.split_once('x') else {
            continue;
        };
        let (Ok(width), Ok(height)) = (width.parse::<u32>(), height.parse::<u32>()) else {
            continue;
        };
        tiers.push(Tier {
            width,
            height,
            format,
            path: entry.path(),
        });
    }
    tiers.sort_by_key(|tier| (tier.width, tier.height));
    Ok(tiers)
}

fn validate_upload_id(id: &str) -> Result<(), String> {
    if id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("背景图片上传编号无效".into())
    }
}

fn validate_image_id(id: &str) -> Result<(), String> {
    if id.len() == 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err("背景图片编号无效".into())
    }
}

/// Format and size from the header alone; the renderer that wrote the tier already
/// decoded it, and the only thing done with it here is to hand it back.
fn inspect_tier(bytes: &[u8]) -> Result<(TierFormat, u32, u32), String> {
    if bytes.len() > MAX_TIER_BYTES {
        return Err(format!(
            "背景图片单级超过 {} MiB",
            MAX_TIER_BYTES / 1024 / 1024
        ));
    }
    let (format, width, height) = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let (width, height) = png_size(bytes)?;
        (TierFormat::Png, width, height)
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        let (width, height) = jpeg_size(bytes)?;
        (TierFormat::Jpeg, width, height)
    } else {
        return Err("背景图片的每一级只能是 JPEG 或 PNG".into());
    };
    if width == 0 || height == 0 || width > MAX_TIER_DIMENSION || height > MAX_TIER_DIMENSION {
        return Err(format!(
            "背景图片尺寸须在 1–{MAX_TIER_DIMENSION} 像素之间"
        ));
    }
    Ok((format, width, height))
}

fn png_size(bytes: &[u8]) -> Result<(u32, u32), String> {
    if bytes.len() < 24 || &bytes[12..16] != b"IHDR" {
        return Err("PNG 缺少 IHDR".into());
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().expect("four bytes"));
    let height = u32::from_be_bytes(bytes[20..24].try_into().expect("four bytes"));
    Ok((width, height))
}

fn jpeg_size(bytes: &[u8]) -> Result<(u32, u32), String> {
    let mut cursor = 2_usize;
    while cursor < bytes.len() {
        if bytes[cursor] != 0xff {
            return Err("JPEG 标记边界无效".into());
        }
        while bytes.get(cursor) == Some(&0xff) {
            cursor += 1;
        }
        let marker = *bytes
            .get(cursor)
            .ok_or_else(|| "JPEG 标记被截断".to_owned())?;
        cursor += 1;
        match marker {
            0xd8 | 0x01 | 0xd0..=0xd7 => continue,
            0xd9 | 0xda => break,
            _ => {}
        }
        let length = bytes
            .get(cursor..cursor + 2)
            .map(|raw| usize::from(u16::from_be_bytes([raw[0], raw[1]])))
            .filter(|length| *length >= 2)
            .ok_or_else(|| "JPEG 段长度无效".to_owned())?;
        let is_frame = matches!(marker, 0xc0..=0xcf) && !matches!(marker, 0xc4 | 0xc8 | 0xcc);
        if is_frame {
            let frame = bytes
                .get(cursor + 2..cursor + 7)
                .ok_or_else(|| "JPEG 帧头被截断".to_owned())?;
            let height = u32::from(u16::from_be_bytes([frame[1], frame[2]]));
            let width = u32::from(u16::from_be_bytes([frame[3], frame[4]]));
            return Ok((width, height));
        }
        cursor += length;
    }
    Err("JPEG 缺少帧头".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 2, 0, 0, 0]);
        bytes
    }

    fn jpeg(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = vec![0xff, 0xd8];
        // An APP0 segment before the frame, as every encoder writes one.
        bytes.extend_from_slice(&[0xff, 0xe0, 0x00, 0x04, 0x4a, 0x46]);
        bytes.extend_from_slice(&[0xff, 0xc0, 0x00, 0x11, 0x08]);
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&[0x03, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        bytes.extend_from_slice(&[0xff, 0xd9]);
        bytes
    }

    #[test]
    fn reads_sizes_from_headers() {
        assert_eq!(inspect_tier(&png(640, 360)), Ok((TierFormat::Png, 640, 360)));
        assert_eq!(
            inspect_tier(&jpeg(1920, 1080)),
            Ok((TierFormat::Jpeg, 1920, 1080))
        );
        assert!(inspect_tier(b"GIF89a....").is_err());
        assert!(inspect_tier(&png(0, 10)).is_err());
        assert!(inspect_tier(&png(MAX_TIER_DIMENSION + 1, 10)).is_err());
    }

    #[test]
    fn uploads_commit_into_one_image_and_serve_the_covering_tier() {
        let temp = tempfile::tempdir().unwrap();
        let store = BackgroundImageStore::new(temp.path());
        let upload = store.put(None, &jpeg(640, 360)).unwrap();
        assert_eq!(store.put(Some(&upload), &jpeg(1920, 1080)).unwrap(), upload);
        store.put(Some(&upload), &jpeg(3840, 2160)).unwrap();
        assert!(store.put(Some(&upload), &jpeg(1920, 1080)).is_err());
        let image = store.commit(&upload).unwrap();
        assert_eq!((image.width, image.height), (3840, 2160));

        let small = store.data(&image.id, 300, 200).unwrap();
        assert_eq!((small.width, small.largest), (640, false));
        assert!(small.data_url.starts_with("data:image/jpeg;base64,"));
        // A portrait window needs the height, not the width, to be covered.
        assert_eq!(store.data(&image.id, 700, 1000).unwrap().width, 1920);
        let beyond = store.data(&image.id, 6000, 3000).unwrap();
        assert_eq!((beyond.width, beyond.largest), (3840, true));
    }

    #[test]
    fn rejects_tiers_of_different_pictures() {
        let temp = tempfile::tempdir().unwrap();
        let store = BackgroundImageStore::new(temp.path());
        let upload = store.put(None, &jpeg(640, 360)).unwrap();
        store.put(Some(&upload), &jpeg(1000, 1000)).unwrap();
        assert!(store.commit(&upload).is_err());
    }

    #[test]
    fn imports_stay_in_the_library_until_deleted() {
        let temp = tempfile::tempdir().unwrap();
        let store = BackgroundImageStore::new(temp.path());
        assert!(store.list().unwrap().is_empty());
        let import = |width: u32| {
            let upload = store.put(None, &png(width, 100)).unwrap();
            let id = store.commit(&upload).unwrap().id;
            // Import times any filesystem can tell apart.
            let time = SystemTime::UNIX_EPOCH + Duration::from_secs(u64::from(width));
            let tier = temp.path().join(DIRECTORY).join(&id).join(format!("{width}x100.png"));
            fs::OpenOptions::new()
                .write(true)
                .open(tier)
                .unwrap()
                .set_modified(time)
                .unwrap();
            id
        };
        let first = import(100);
        let second = import(200);
        let third = import(300);
        let listed = store.list().unwrap();
        assert_eq!(
            listed.iter().map(|image| image.id.as_str()).collect::<Vec<_>>(),
            [third.as_str(), second.as_str(), first.as_str()]
        );
        assert_eq!((listed[0].width, listed[0].height), (300, 100));

        let abandoned = store.put(None, &png(50, 50)).unwrap();
        store.reconcile();
        assert_eq!(store.list().unwrap().len(), 3);
        assert!(store.commit(&abandoned).is_err());

        store.delete(&second).unwrap();
        assert!(store.data(&second, 1, 1).is_err());
        assert!(store.data(&first, 1, 1).is_ok());
        assert_eq!(store.list().unwrap().len(), 2);
        store.delete(&second).unwrap();
    }

    #[test]
    fn identical_ladders_share_one_image() {
        let temp = tempfile::tempdir().unwrap();
        let store = BackgroundImageStore::new(temp.path());
        let first = store.put(None, &png(64, 64)).unwrap();
        let second = store.put(None, &png(64, 64)).unwrap();
        let a = store.commit(&first).unwrap();
        let b = store.commit(&second).unwrap();
        assert_eq!(a.id, b.id);
        assert!(store.data(&a.id, 1, 1).is_ok());
    }

    #[test]
    fn ids_are_checked_before_touching_the_disk() {
        let temp = tempfile::tempdir().unwrap();
        let store = BackgroundImageStore::new(temp.path());
        assert!(store.data("../../etc", 1, 1).is_err());
        assert!(store.put(Some("../x"), &png(1, 1)).is_err());
        assert!(store.commit("not-an-upload").is_err());
        assert!(store.delete("../../etc").is_err());
    }
}
