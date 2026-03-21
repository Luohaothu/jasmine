use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};

use image::imageops::FilterType;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};
use thiserror::Error;

pub const THUMBNAIL_MAX_EDGE: u32 = 300;
const MAX_DECODE_DIMENSION: u32 = 10_000;
const MAX_DECODE_ALLOC_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum ThumbnailError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),
    #[error("unsupported image format")]
    UnsupportedFormat,
}

pub fn generate_thumbnail(
    image_path: &Path,
    output_dir: &Path,
    transfer_id: &str,
) -> Result<PathBuf, ThumbnailError> {
    let file = File::open(image_path)?;
    let mut reader = ImageReader::new(BufReader::new(file)).with_guessed_format()?;
    reader.limits(decode_limits());

    let format = reader.format().ok_or(ThumbnailError::UnsupportedFormat)?;
    if !is_supported_input_format(format) {
        return Err(ThumbnailError::UnsupportedFormat);
    }

    let mut decoder = reader.into_decoder()?;
    let orientation = decoder.orientation()?;
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);

    let resized = resize_image(image);
    fs::create_dir_all(output_dir)?;

    let output_path = output_dir.join(format!("{transfer_id}.webp"));
    let temp_output_path = output_dir.join(format!("{transfer_id}.webp.tmp"));
    remove_file_if_exists(&temp_output_path)?;

    let write_result = (|| -> Result<(), ThumbnailError> {
        let mut file = File::create(&temp_output_path)?;
        resized.write_to(&mut file, ImageFormat::WebP)?;
        file.sync_all()?;
        drop(file);

        remove_file_if_exists(&output_path)?;
        fs::rename(&temp_output_path, &output_path)?;
        Ok(())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&temp_output_path);
    }

    write_result?;
    Ok(output_path)
}

fn decode_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODE_DIMENSION);
    limits.max_image_height = Some(MAX_DECODE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOC_BYTES);
    limits
}

fn is_supported_input_format(format: ImageFormat) -> bool {
    matches!(
        format,
        ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::Gif | ImageFormat::WebP
    )
}

fn resize_image(image: DynamicImage) -> DynamicImage {
    let width = image.width();
    let height = image.height();
    let (target_width, target_height) = resized_dimensions(width, height, THUMBNAIL_MAX_EDGE);
    let source = image.to_rgba8();

    if width == target_width && height == target_height {
        DynamicImage::ImageRgba8(source)
    } else {
        DynamicImage::ImageRgba8(image::imageops::resize(
            &source,
            target_width,
            target_height,
            FilterType::Lanczos3,
        ))
    }
}

fn resized_dimensions(width: u32, height: u32, max_edge: u32) -> (u32, u32) {
    if width <= max_edge && height <= max_edge {
        return (width, height);
    }

    if width >= height {
        let scaled_height = ((u64::from(height) * u64::from(max_edge)) / u64::from(width)).max(1);
        (max_edge, scaled_height as u32)
    } else {
        let scaled_width = ((u64::from(width) * u64::from(max_edge)) / u64::from(height)).max(1);
        (scaled_width as u32, max_edge)
    }
}

fn remove_file_if_exists(path: &Path) -> Result<(), std::io::Error> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
