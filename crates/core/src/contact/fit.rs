//! Fitting a photo into iCloud's card size limit (CG-15 Decision 1, R5).

use std::io::Cursor;

use image::{DynamicImage, ImageDecoder, ImageReader, Limits, codecs::jpeg::JpegEncoder, imageops::FilterType};

use super::{PhotoData, VCard};

/// Largest card body pushed to iCloud. iCloud rejects bodies over about
/// 280,576 bytes with a 403 (research, S7); this leaves a margin.
pub const ICLOUD_MAX_CARD_BYTES: usize = 280_000;

/// Longest-edge steps tried, largest first (never upscaled).
const EDGES: [u32; 5] = [1024, 768, 512, 384, 256];
/// JPEG qualities tried at each edge.
const QUALITIES: [u8; 2] = [85, 70];
/// Largest width or height decoded: a photo beyond it is unfittable.
const MAX_DECODE_EDGE: u32 = 10_000;
/// Largest buffer the decoder may allocate. Downloads are capped at 10 MB;
/// a camera JPEG that size decodes to well under this (a 48 MP RGB frame is
/// about 144 MB), while a small file claiming huge dimensions is refused.
const MAX_DECODE_ALLOC: u64 = 192 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fit {
    /// The card with this photo inline is within the limit.
    AsIs,
    /// A downscaled JPEG that fits.
    Resized(PhotoData),
    /// Undecodable, or too large even at the smallest step.
    Unfittable,
}

/// Whether `card` with `photo` inline fits in `max_card_bytes`, and if not,
/// the first downscaled JPEG that does.
#[must_use]
pub fn fit_photo(card: &VCard, photo: &PhotoData, max_card_bytes: usize) -> Fit {
    let fits = |bytes: &[u8]| card.with_inline_photo(bytes).as_bytes().len() <= max_card_bytes;
    if fits(&photo.bytes) {
        return Fit::AsIs;
    }
    let Some(image) = decode(&photo.bytes) else {
        return Fit::Unfittable;
    };
    for edge in EDGES {
        let scaled = if image.width().max(image.height()) > edge {
            image.resize(edge, edge, FilterType::Lanczos3)
        } else {
            image.clone()
        };
        for quality in QUALITIES {
            if let Some(bytes) = encode_jpeg(&scaled, quality).filter(|bytes| fits(bytes)) {
                return Fit::Resized(PhotoData::new(bytes));
            }
        }
    }
    Fit::Unfittable
}

/// Decodes `bytes` within the decode limits, turned upright as its EXIF
/// orientation says: the re-encoded JPEG carries no EXIF.
fn decode(bytes: &[u8]) -> Option<DynamicImage> {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DECODE_EDGE);
    limits.max_image_height = Some(MAX_DECODE_EDGE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
    reader.limits(limits);
    let mut decoder = reader.into_decoder().ok()?;
    let orientation = decoder.orientation().ok();
    let mut image = DynamicImage::from_decoder(decoder).ok()?;
    if let Some(orientation) = orientation {
        image.apply_orientation(orientation);
    }
    Some(image)
}

fn encode_jpeg(image: &DynamicImage, quality: u8) -> Option<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    JpegEncoder::new_with_quality(&mut out, quality).encode_image(&image.to_rgb8()).ok()?;
    Some(out.into_inner())
}

#[cfg(test)]
mod tests {
    use image::{ImageFormat, RgbImage};

    use super::*;

    fn card() -> VCard {
        VCard::parse("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:u1\r\nFN:Jane\r\nEND:VCARD\r\n").unwrap()
    }

    /// A noisy JPEG that compresses badly: several hundred KB at 2000 px.
    fn noisy_jpeg(size: u32) -> PhotoData {
        let mut seed: u32 = 1;
        let image = RgbImage::from_fn(size, size, |_, _| {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            image::Rgb([(seed >> 16) as u8, (seed >> 8) as u8, seed as u8])
        });
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, ImageFormat::Jpeg).unwrap();
        PhotoData::new(bytes.into_inner())
    }

    #[test]
    fn a_photo_that_fits_is_left_as_is() {
        assert_eq!(fit_photo(&card(), &noisy_jpeg(16), ICLOUD_MAX_CARD_BYTES), Fit::AsIs);
    }

    #[test]
    fn an_oversized_photo_is_downscaled_until_the_card_fits() {
        let photo = noisy_jpeg(2000);
        assert!(card().with_inline_photo(&photo.bytes).as_bytes().len() > ICLOUD_MAX_CARD_BYTES);

        let Fit::Resized(resized) = fit_photo(&card(), &photo, ICLOUD_MAX_CARD_BYTES) else {
            panic!("expected a resize");
        };

        assert!(card().with_inline_photo(&resized.bytes).as_bytes().len() <= ICLOUD_MAX_CARD_BYTES);
        let decoded = image::load_from_memory(&resized.bytes).unwrap();
        assert!(decoded.width() <= 1024 && decoded.height() <= 1024);
    }

    /// A noisy `width`×`height` JPEG at quality 100, with an EXIF segment
    /// setting `orientation` when given.
    fn oriented_jpeg(width: u32, height: u32, orientation: Option<u16>) -> PhotoData {
        let mut seed: u32 = 7;
        let image = RgbImage::from_fn(width, height, |_, _| {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            image::Rgb([(seed >> 16) as u8, (seed >> 8) as u8, seed as u8])
        });
        let mut out = Cursor::new(Vec::new());
        JpegEncoder::new_with_quality(&mut out, 100).encode_image(&image).unwrap();
        let mut bytes = out.into_inner();
        if let Some(orientation) = orientation {
            // APP1 "Exif": a big-endian TIFF header and one IFD entry,
            // Orientation (0x0112), SHORT, count 1.
            let mut exif = b"Exif\0\0MM\0\x2A\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01".to_vec();
            exif.extend_from_slice(&orientation.to_be_bytes());
            exif.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
            let length = u16::try_from(exif.len() + 2).unwrap().to_be_bytes();
            let segment = [&[0xFF, 0xE1], &length[..], &exif].concat();
            bytes.splice(2..2, segment);
        }
        PhotoData::new(bytes)
    }

    /// Just too large to go inline as it is.
    fn just_over(photo: &PhotoData) -> usize {
        card().with_inline_photo(&photo.bytes).as_bytes().len() - 1
    }

    #[test]
    fn exif_orientation_is_applied_before_resizing() {
        let photo = oriented_jpeg(64, 32, Some(6));

        let Fit::Resized(resized) = fit_photo(&card(), &photo, just_over(&photo)) else {
            panic!("expected a re-encode");
        };

        let decoded = image::load_from_memory(&resized.bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (32, 64), "rotated 90° as EXIF says");
    }

    #[test]
    fn a_photo_without_orientation_keeps_its_shape() {
        let photo = oriented_jpeg(64, 32, None);

        let Fit::Resized(resized) = fit_photo(&card(), &photo, just_over(&photo)) else {
            panic!("expected a re-encode");
        };

        let decoded = image::load_from_memory(&resized.bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (64, 32));
    }

    #[test]
    fn a_photo_over_the_decode_limits_is_unfittable() {
        // Would downscale to 1024×2 and fit easily, but 10,001 px wide is
        // over the decode limit.
        let photo = oriented_jpeg(10_001, 16, None);

        assert_eq!(fit_photo(&card(), &photo, just_over(&photo)), Fit::Unfittable);
    }

    #[test]
    fn undecodable_or_never_fitting_photos_are_unfittable() {
        assert_eq!(fit_photo(&card(), &PhotoData::new(vec![1; 5000]), 1000), Fit::Unfittable);
        assert_eq!(fit_photo(&card(), &noisy_jpeg(600), 1000), Fit::Unfittable);
    }
}
