//! Fitting a photo into iCloud's card size limit (CG-15 Decision 1, R5).

use std::io::Cursor;

use image::{DynamicImage, codecs::jpeg::JpegEncoder, imageops::FilterType};

use super::{PhotoData, VCard};

/// Largest card body pushed to iCloud. iCloud rejects bodies over about
/// 280,576 bytes with a 403 (research, S7); this leaves a margin.
pub const ICLOUD_MAX_CARD_BYTES: usize = 280_000;

/// Longest-edge steps tried, largest first (never upscaled).
const EDGES: [u32; 5] = [1024, 768, 512, 384, 256];
/// JPEG qualities tried at each edge.
const QUALITIES: [u8; 2] = [85, 70];

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
    let Ok(image) = image::load_from_memory(&photo.bytes) else {
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

    #[test]
    fn undecodable_or_never_fitting_photos_are_unfittable() {
        assert_eq!(fit_photo(&card(), &PhotoData::new(vec![1; 5000]), 1000), Fit::Unfittable);
        assert_eq!(fit_photo(&card(), &noisy_jpeg(600), 1000), Fit::Unfittable);
    }
}
