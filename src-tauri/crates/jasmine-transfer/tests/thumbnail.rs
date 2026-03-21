use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::{DynamicImage, ImageFormat, ImageReader, Rgba, RgbaImage};
use jasmine_transfer::{generate_thumbnail, THUMBNAIL_MAX_EDGE};
use tempfile::tempdir;

fn encode_image_bytes(format: ImageFormat, width: u32, height: u32) -> Vec<u8> {
    let image = RgbaImage::from_fn(width, height, |x, y| {
        Rgba([(x % 255) as u8, (y % 255) as u8, ((x + y) % 255) as u8, 255])
    });
    let image = DynamicImage::ImageRgba8(image);
    let mut cursor = Cursor::new(Vec::new());
    image
        .write_to(&mut cursor, format)
        .expect("encode test image");
    cursor.into_inner()
}

fn write_input(path: &Path, bytes: &[u8]) -> PathBuf {
    std::fs::write(path, bytes).expect("write thumbnail input");
    path.to_path_buf()
}

fn assert_generated_webp(path: &Path, expected_width: Option<u32>, expected_height: Option<u32>) {
    let reader = ImageReader::open(path)
        .expect("open generated thumbnail")
        .with_guessed_format()
        .expect("guess generated thumbnail format");
    assert_eq!(reader.format(), Some(ImageFormat::WebP));

    let decoded = reader.decode().expect("decode generated thumbnail");
    assert!(decoded.width() <= THUMBNAIL_MAX_EDGE);
    assert!(decoded.height() <= THUMBNAIL_MAX_EDGE);

    if let Some(expected_width) = expected_width {
        assert_eq!(decoded.width(), expected_width);
    }
    if let Some(expected_height) = expected_height {
        assert_eq!(decoded.height(), expected_height);
    }
}

#[test]
fn thumbnail_generates_webp_from_jpeg_with_bounded_dimensions() {
    let temp = tempdir().expect("tempdir");
    let input_path = write_input(
        &temp.path().join("source.jpg"),
        &encode_image_bytes(ImageFormat::Jpeg, 900, 600),
    );
    let output_dir = temp.path().join("thumbs");

    let output_path =
        generate_thumbnail(&input_path, &output_dir, "jpeg-thumb").expect("generate jpeg thumb");

    assert_eq!(output_path, output_dir.join("jpeg-thumb.webp"));
    assert!(output_path.exists());
    assert_generated_webp(&output_path, Some(300), Some(200));
}

#[test]
fn thumbnail_generates_webp_from_png_gif_and_webp_inputs() {
    let temp = tempdir().expect("tempdir");
    let cases = [
        (ImageFormat::Png, "image.png", "png-thumb"),
        (ImageFormat::Gif, "image.gif", "gif-thumb"),
        (ImageFormat::WebP, "image.webp", "webp-thumb"),
    ];

    for (format, name, transfer_id) in cases {
        let input_path = write_input(
            &temp.path().join(name),
            &encode_image_bytes(format, 640, 480),
        );
        let output_path = generate_thumbnail(&input_path, temp.path(), transfer_id)
            .expect("generate thumbnail for supported format");

        assert_eq!(output_path, temp.path().join(format!("{transfer_id}.webp")));
        assert_generated_webp(&output_path, None, None);
    }
}

#[test]
fn thumbnail_rejects_images_exceeding_dimension_limits() {
    let temp = tempdir().expect("tempdir");
    let input_path = write_input(
        &temp.path().join("too-wide.png"),
        &encode_image_bytes(ImageFormat::Png, 10_001, 1),
    );
    let output_dir = temp.path().join("thumbs");
    let output_path = output_dir.join("too-wide.webp");

    let error =
        generate_thumbnail(&input_path, &output_dir, "too-wide").expect_err("limit error expected");

    assert!(!output_path.exists());
    assert!(
        error.to_string().contains("limit") || error.to_string().contains("dimension"),
        "expected limit-related error, got {error}"
    );
}

#[test]
fn thumbnail_rejects_unsupported_non_image_input() {
    let temp = tempdir().expect("tempdir");
    let input_path = write_input(&temp.path().join("notes.txt"), b"not an image");
    let output_dir = temp.path().join("thumbs");

    generate_thumbnail(&input_path, &output_dir, "text-input")
        .expect_err("non-image input should fail");
    assert!(!output_dir.join("text-input.webp").exists());
}

#[test]
fn thumbnail_rejects_corrupt_image_bytes() {
    let temp = tempdir().expect("tempdir");
    let mut corrupt = encode_image_bytes(ImageFormat::Jpeg, 32, 32);
    corrupt.truncate(12);
    let input_path = write_input(&temp.path().join("broken.jpg"), &corrupt);
    let output_dir = temp.path().join("thumbs");

    generate_thumbnail(&input_path, &output_dir, "broken").expect_err("corrupt image should fail");
    assert!(!output_dir.join("broken.webp").exists());
}
