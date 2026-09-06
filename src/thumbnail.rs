use anyhow::{Context, Result};
use image::imageops::FilterType;
use image::ImageDecoder;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;

use crate::client::SpacetimeClient;
use crate::isolation::run_isolated;
use crate::space_file::SpaceFile;

pub const IMAGE_EXTENSIONS: [&str; 6] = ["jpg", "jpeg", "png", "gif", "webp", "heic"];
pub const VIDEO_EXTENSIONS: [&str; 4] = ["mp4", "mov", "m4v", "webm"];
const THUMBNAIL_MAX_EDGE: u32 = 256;
const THUMBNAIL_JPEG_QUALITY: u8 = 80;
const VIDEO_SEEK_TIMESTAMP: &str = "00:00:01";
const VIDEO_SCALE_FILTER: &str =
    "scale=w='min(256,iw)':h='min(256,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2";

#[derive(Debug)]
pub enum ThumbnailError {
    Io(std::io::Error),
    CommandFailed(String),
    Decode(String),
    UnsupportedExtension(String),
}

impl std::fmt::Display for ThumbnailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ThumbnailError::Io(e) => write!(f, "io error: {e}"),
            ThumbnailError::CommandFailed(s) => write!(f, "command failed: {s}"),
            ThumbnailError::Decode(s) => write!(f, "decode error: {s}"),
            ThumbnailError::UnsupportedExtension(s) => write!(f, "unsupported extension: {s}"),
        }
    }
}

impl std::error::Error for ThumbnailError {}

impl From<std::io::Error> for ThumbnailError {
    fn from(e: std::io::Error) -> Self {
        ThumbnailError::Io(e)
    }
}

impl From<image::ImageError> for ThumbnailError {
    fn from(e: image::ImageError) -> Self {
        ThumbnailError::Decode(e.to_string())
    }
}

pub fn is_thumbnailable(extension: &str) -> bool {
    IMAGE_EXTENSIONS.contains(&extension) || VIDEO_EXTENSIONS.contains(&extension)
}

fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains('\0')
}

pub fn thumbnail_path(vault_path: &Path, id: &str) -> Option<PathBuf> {
    if !is_safe_id(id) {
        return None;
    }
    Some(vault_path.join(".thumbnails").join(format!("{}.jpg", id)))
}

pub fn remove_thumbnail_file(vault_path: &Path, id: &str) {
    let Some(path) = thumbnail_path(vault_path, id) else {
        tracing::warn!("Refusing to remove thumbnail for unsafe id {:?}", id);
        return;
    };
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            tracing::warn!("Could not remove thumbnail {:?}: {}", path, e);
        }
    }
}

pub fn generate_thumbnail(source: &Path, dest: &Path) -> Result<(), ThumbnailError> {
    let Some(extension) = source
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
    else {
        return Err(ThumbnailError::UnsupportedExtension(String::new()));
    };

    if extension == "heic" {
        return generate_heic_thumbnail(source, dest);
    }
    if IMAGE_EXTENSIONS.contains(&extension.as_str()) {
        return generate_image_thumbnail(source, dest);
    }
    if VIDEO_EXTENSIONS.contains(&extension.as_str()) {
        return generate_video_thumbnail(source, dest);
    }

    Err(ThumbnailError::UnsupportedExtension(extension))
}

pub fn generate_image_thumbnail(source: &Path, dest: &Path) -> Result<(), ThumbnailError> {
    let mut decoder = image::ImageReader::open(source)?
        .with_guessed_format()?
        .into_decoder()?;
    let orientation = decoder.orientation()?;
    let mut img = image::DynamicImage::from_decoder(decoder)?;
    img.apply_orientation(orientation);

    let resized = if img.width().max(img.height()) > THUMBNAIL_MAX_EDGE {
        img.resize(THUMBNAIL_MAX_EDGE, THUMBNAIL_MAX_EDGE, FilterType::Lanczos3)
    } else {
        img
    };

    let mut out = std::io::BufWriter::new(std::fs::File::create(dest)?);
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, THUMBNAIL_JPEG_QUALITY);
    resized.to_rgb8().write_with_encoder(encoder)?;
    Ok(())
}

fn generate_heic_thumbnail(source: &Path, dest: &Path) -> Result<(), ThumbnailError> {
    extract_frame(source, dest, None)
}

fn build_ffmpeg_command(source: &Path, dest: &Path, seek: Option<&str>) -> Command {
    let mut command = Command::new("ffmpeg");
    command.args(["-nostdin", "-loglevel", "error", "-y"]);
    if let Some(timestamp) = seek {
        command.args(["-ss", timestamp]);
    }
    command
        .arg("-i")
        .arg(source)
        .args(["-frames:v", "1", "-vf", VIDEO_SCALE_FILTER, "-q:v", "3"])
        .arg(dest);
    command
}

fn run_ffmpeg(source: &Path, dest: &Path, seek: Option<&str>) -> Result<bool, ThumbnailError> {
    let output = build_ffmpeg_command(source, dest, seek).output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(ThumbnailError::CommandFailed(stderr));
    }
    let written = std::fs::metadata(dest).map(|m| m.len() > 0).unwrap_or(false);
    Ok(written)
}

pub fn generate_video_thumbnail(source: &Path, dest: &Path) -> Result<(), ThumbnailError> {
    extract_frame(source, dest, Some(VIDEO_SEEK_TIMESTAMP))
}

fn extract_frame(source: &Path, dest: &Path, seek: Option<&str>) -> Result<(), ThumbnailError> {
    let seeked = match run_ffmpeg(source, dest, seek) {
        Ok(written) => written,
        Err(ThumbnailError::CommandFailed(_)) if seek.is_some() => false,
        Err(e) => return Err(e),
    };
    if seeked {
        return Ok(());
    }
    if seek.is_some() && run_ffmpeg(source, dest, None)? {
        return Ok(());
    }
    Err(ThumbnailError::CommandFailed(
        "ffmpeg exited successfully but produced no output frame".to_string(),
    ))
}

struct ThumbnailJob {
    id: String,
    path: String,
}

pub struct ThumbnailQueue {
    client: Arc<SpacetimeClient>,
    tx: Sender<ThumbnailJob>,
}

impl ThumbnailQueue {
    pub fn start(vault_path: PathBuf, client: Arc<SpacetimeClient>) -> Result<Self> {
        let (tx, rx) = mpsc::channel::<ThumbnailJob>();
        let worker_client = client.clone();
        std::thread::Builder::new()
            .name("thumbnails".to_string())
            .spawn(move || {
                for job in rx {
                    let context = format!("thumbnail {} (ID: {})", job.path, job.id);
                    run_isolated(context, || process_job(&vault_path, &worker_client, &job));
                }
            })
            .context("Failed to start thumbnail worker thread")?;
        Ok(Self { client, tx })
    }

    pub fn enqueue(&self, file: &SpaceFile) {
        if !is_thumbnailable(&file.extension) || self.client.has_thumbnail(&file.id) {
            return;
        }
        let job = ThumbnailJob {
            id: file.id.clone(),
            path: file.path.clone(),
        };
        if self.tx.send(job).is_err() {
            tracing::error!("Thumbnail worker is gone; dropping job for {}", file.path);
        }
    }
}

fn process_job(vault_path: &Path, client: &SpacetimeClient, job: &ThumbnailJob) {
    if client.has_thumbnail(&job.id) {
        return;
    }
    let source = vault_path.join(&job.path);
    if !source.is_file() {
        return;
    }
    let Some(dest) = thumbnail_path(vault_path, &job.id) else {
        tracing::warn!("Refusing thumbnail for unsafe id {:?} ({})", job.id, job.path);
        return;
    };
    if let Some(parent) = dest.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!("Could not create .thumbnails directory: {}", e);
            return;
        }
    }

    match generate_thumbnail(&source, &dest) {
        Ok(()) => {
            client.set_thumbnail_available(&job.id);
            tracing::info!("Generated thumbnail for: {} (ID: {})", job.path, job.id);
        }
        Err(e) => {
            tracing::warn!(
                "Thumbnail generation failed for {} (ID: {}): {}",
                job.path,
                job.id,
                e
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("spacenotes-thumb-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_test_video(dest: &Path, duration: &str) {
        let status = Command::new("ffmpeg")
            .args([
                "-y",
                "-f", "lavfi",
                "-i", &format!("testsrc=duration={duration}:size=320x240:rate=10"),
                "-pix_fmt", "yuv420p",
            ])
            .arg(dest)
            .output()
            .expect("ffmpeg must be on PATH to synthesize the test fixture");
        assert!(status.status.success(), "failed to synthesize test video: {:?}", String::from_utf8_lossy(&status.stderr));
    }

    fn make_test_image(dest: &Path, width: u32, height: u32) {
        let img = image::RgbImage::from_pixel(width, height, image::Rgb([200, 100, 50]));
        image::DynamicImage::ImageRgb8(img)
            .save(dest)
            .expect("failed to write test image fixture");
    }

    fn make_test_image_with_alpha(dest: &Path, width: u32, height: u32) {
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba([200, 100, 50, 128]));
        image::DynamicImage::ImageRgba8(img)
            .save(dest)
            .expect("failed to write test image fixture");
    }

    fn exif_orientation_segment(orientation: u16) -> Vec<u8> {
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II");
        tiff.extend_from_slice(&42u16.to_le_bytes());
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&1u16.to_le_bytes());
        tiff.extend_from_slice(&0x0112u16.to_le_bytes());
        tiff.extend_from_slice(&3u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&orientation.to_le_bytes());
        tiff.extend_from_slice(&0u16.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes());

        let mut payload = Vec::new();
        payload.extend_from_slice(b"Exif\0\0");
        payload.extend_from_slice(&tiff);

        let mut segment = vec![0xFF, 0xE1];
        segment.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        segment.extend_from_slice(&payload);
        segment
    }

    fn make_rotated_test_image(dest: &Path, width: u32, height: u32) {
        let mut jpeg = Vec::new();
        let img = image::RgbImage::from_pixel(width, height, image::Rgb([10, 20, 30]));
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
            .unwrap();
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);

        let mut with_exif = jpeg[..2].to_vec();
        with_exif.extend_from_slice(&exif_orientation_segment(6));
        with_exif.extend_from_slice(&jpeg[2..]);
        std::fs::write(dest, with_exif).unwrap();
    }

    fn decoded(dest: &Path) -> image::DynamicImage {
        let bytes = std::fs::read(dest).expect("thumbnail file should exist");
        assert!(!bytes.is_empty(), "thumbnail file should not be empty");
        image::load_from_memory(&bytes).expect("output should decode as a valid image")
    }

    #[test]
    fn generate_thumbnail_from_video() {
        let dir = temp_dir("video");
        let source = dir.join("source.mp4");
        make_test_video(&source, "2");

        let dest = dir.join("thumb.jpg");
        generate_thumbnail(&source, &dest).expect("thumbnail generation should succeed");

        let img = decoded(&dest);
        assert!(img.width() > 0 && img.height() > 0);
        assert!(img.width().max(img.height()) <= THUMBNAIL_MAX_EDGE);
    }

    #[test]
    fn video_shorter_than_the_seek_point_still_gets_a_thumbnail() {
        let dir = temp_dir("short-video");
        let source = dir.join("short.mp4");
        make_test_video(&source, "0.5");

        let dest = dir.join("thumb.jpg");
        generate_thumbnail(&source, &dest).expect("a sub-second video must fall back to its first frame");

        let img = decoded(&dest);
        assert!(img.width() > 0 && img.height() > 0);
    }

    #[test]
    fn generate_thumbnail_returns_err_on_corrupt_input() {
        let dir = temp_dir("corrupt");
        let source = dir.join("corrupt.mp4");
        std::fs::write(&source, b"this is not a real video file").unwrap();

        let dest = dir.join("thumb.jpg");
        let result = generate_thumbnail(&source, &dest);

        assert!(result.is_err(), "corrupt input must return Err, not panic");
        assert!(!dest.exists(), "a failed generation must not leave a thumbnail behind");
    }

    #[test]
    fn generate_thumbnail_from_image() {
        let dir = temp_dir("image");
        let source = dir.join("source.jpg");
        make_test_image(&source, 1000, 800);

        let dest = dir.join("thumb.jpg");
        generate_thumbnail(&source, &dest).expect("thumbnail generation should succeed");

        let img = decoded(&dest);
        assert!(img.width().max(img.height()) <= THUMBNAIL_MAX_EDGE);

        let input_ratio = 1000.0 / 800.0;
        let output_ratio = img.width() as f64 / img.height() as f64;
        assert!(
            (input_ratio - output_ratio).abs() < 0.05,
            "aspect ratio should be preserved: input {input_ratio}, output {output_ratio}"
        );
    }

    #[test]
    fn image_thumbnail_honours_exif_orientation() {
        let dir = temp_dir("exif");
        let source = dir.join("rotated.jpg");
        make_rotated_test_image(&source, 1000, 800);

        let dest = dir.join("thumb.jpg");
        generate_thumbnail(&source, &dest).expect("thumbnail generation should succeed");

        let img = decoded(&dest);
        assert!(
            img.height() > img.width(),
            "a landscape frame tagged orientation 6 must come out portrait: {}x{}",
            img.width(),
            img.height()
        );
    }

    #[test]
    fn image_with_alpha_channel_encodes_as_jpeg() {
        let dir = temp_dir("alpha");
        let source = dir.join("alpha.png");
        make_test_image_with_alpha(&source, 600, 400);

        let dest = dir.join("thumb.jpg");
        generate_thumbnail(&source, &dest).expect("an RGBA source must still produce a JPEG thumbnail");

        let img = decoded(&dest);
        assert!(img.width().max(img.height()) <= THUMBNAIL_MAX_EDGE);
    }

    #[test]
    fn sixteen_bit_image_encodes_as_jpeg() {
        let dir = temp_dir("deep");
        let source = dir.join("deep.png");
        let img = image::ImageBuffer::<image::Rgb<u16>, Vec<u16>>::from_pixel(600, 400, image::Rgb([60000, 30000, 1000]));
        image::DynamicImage::ImageRgb16(img).save(&source).unwrap();

        let dest = dir.join("thumb.jpg");
        generate_thumbnail(&source, &dest).expect("a 16-bit source must still produce a JPEG thumbnail");

        let img = decoded(&dest);
        assert!(img.width().max(img.height()) <= THUMBNAIL_MAX_EDGE);
    }

    #[test]
    fn small_image_is_not_upscaled() {
        let dir = temp_dir("small");
        let source = dir.join("small.png");
        make_test_image(&source, 64, 48);

        let dest = dir.join("thumb.jpg");
        generate_thumbnail(&source, &dest).expect("thumbnail generation should succeed");

        let img = decoded(&dest);
        assert_eq!((img.width(), img.height()), (64, 48));
    }

    #[test]
    fn generate_thumbnail_returns_err_on_corrupt_image_input() {
        let dir = temp_dir("corrupt-image");
        let source = dir.join("corrupt.jpg");
        std::fs::write(&source, b"not a real jpeg").unwrap();

        let dest = dir.join("thumb.jpg");
        let result = generate_thumbnail(&source, &dest);

        assert!(result.is_err(), "corrupt image input must return Err, not panic");
    }

    #[test]
    fn generate_thumbnail_returns_err_on_unsupported_extension() {
        let dir = temp_dir("unsupported");
        let source = dir.join("source.txt");
        std::fs::write(&source, b"plain text").unwrap();

        let dest = dir.join("thumb.jpg");
        let result = generate_thumbnail(&source, &dest);

        assert!(result.is_err(), "unsupported extension must return Err, not panic");
    }

    #[test]
    fn video_thumbnail_seeks_to_the_one_second_mark() {
        let dir = temp_dir("seek-args");
        let source = dir.join("source.mp4");
        let dest = dir.join("thumb.jpg");

        let command = build_ffmpeg_command(&source, &dest, Some(VIDEO_SEEK_TIMESTAMP));
        let args: Vec<String> = command
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();

        let ss_pos = args.iter().position(|a| a == "-ss").expect("-ss flag must be present");
        assert_eq!(args[ss_pos + 1], VIDEO_SEEK_TIMESTAMP);
        let i_pos = args.iter().position(|a| a == "-i").expect("-i flag must be present");
        assert!(ss_pos < i_pos, "-ss must precede -i so ffmpeg seeks the input, not the output");
    }

    #[test]
    fn is_thumbnailable_matches_only_image_and_video_extensions() {
        for ext in IMAGE_EXTENSIONS.iter().chain(VIDEO_EXTENSIONS.iter()) {
            assert!(is_thumbnailable(ext), "{ext} should be thumbnailable");
        }
        for ext in ["md", "gpg", "pdf", "mp3", "txt", ""] {
            assert!(!is_thumbnailable(ext), "{ext:?} should not be thumbnailable");
        }
    }

    #[test]
    fn thumbnail_path_is_keyed_by_id_inside_the_thumbnails_directory() {
        let vault = Path::new("/vault");
        let path = thumbnail_path(vault, "11111111-1111-1111-1111-111111111111").unwrap();
        assert_eq!(
            path,
            Path::new("/vault/.thumbnails/11111111-1111-1111-1111-111111111111.jpg")
        );
    }

    #[test]
    fn thumbnail_path_refuses_ids_that_escape_the_thumbnails_directory() {
        let vault = Path::new("/vault");
        for id in ["", ".", "..", "../escape", "a/b", "a\\b", "..\\..\\x", "nul\0byte"] {
            assert!(thumbnail_path(vault, id).is_none(), "{id:?} must be refused");
        }
    }

    #[test]
    fn remove_thumbnail_file_removes_existing_and_tolerates_missing() {
        let vault = temp_dir("remove");
        let id = "some-file-id";
        let thumb_dir = vault.join(".thumbnails");
        std::fs::create_dir_all(&thumb_dir).unwrap();
        let thumb_path = thumb_dir.join(format!("{}.jpg", id));
        std::fs::write(&thumb_path, b"fake jpeg bytes").unwrap();

        remove_thumbnail_file(&vault, id);
        assert!(!thumb_path.exists(), "existing thumbnail should be removed");

        remove_thumbnail_file(&vault, id);
        assert!(!thumb_path.exists(), "removing an already-absent thumbnail must not panic or error");

        remove_thumbnail_file(&vault, "never-existed-id");
    }

    #[test]
    fn remove_thumbnail_file_never_reaches_outside_the_thumbnails_directory() {
        let dir = temp_dir("remove-escape");
        let vault = dir.join("vault");
        std::fs::create_dir_all(vault.join(".thumbnails")).unwrap();
        let victim = dir.join("victim.jpg");
        std::fs::write(&victim, b"not yours").unwrap();

        remove_thumbnail_file(&vault, "../../victim");

        assert!(victim.exists(), "an id with path segments must never delete outside .thumbnails");
    }
}
