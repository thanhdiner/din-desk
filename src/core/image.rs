//! Validated image input. Binary data is kept outside the model's text prompt.
use base64::{engine::general_purpose::STANDARD, Engine};
use sha2::{Digest, Sha256};
use std::sync::Arc;

pub const MAX_IMAGE_BYTES: usize = 2 * 1024 * 1024;
// Anthropic requests include images from the entire conversation, not only
// the current turn. Bound that history independently of a single upload.
pub const MAX_IMAGES: usize = 128;
pub const IMAGE_TOKEN_ESTIMATE: u64 = 2048;

// Deliberately no Debug: never write image data to logs.
#[derive(Clone)]
pub struct InputImage {
    pub id: String,
    pub media_type: String,
    pub data_base64: Arc<str>,
}

impl InputImage {
    pub fn from_base64(media_type: &str, data: &str) -> Result<Self, String> {
        if data.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 {
            return Err("Each image must be at most 2 MiB decoded".into());
        }
        if !matches!(
            media_type,
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        ) {
            return Err("Images must be PNG, JPEG, GIF or WebP".into());
        }
        let bytes = STANDARD.decode(data).map_err(|_| "Invalid image base64")?;
        if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
            return Err("Each image must contain 1 byte to 2 MiB decoded".into());
        }
        let matches = match media_type {
            "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
            "image/jpeg" => bytes.starts_with(b"\xff\xd8\xff"),
            "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
            "image/webp" => bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
            _ => false,
        };
        if !matches {
            return Err("Image bytes do not match media_type".into());
        }
        let id = format!("image_{:x}", Sha256::digest(&bytes));
        Ok(Self {
            id,
            media_type: media_type.to_owned(),
            data_base64: Arc::from(data),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checks_encoding_size_and_signature_without_counting_base64_as_prompt_text() {
        let data = STANDARD.encode(b"\x89PNG\r\n\x1a\nfixture");
        let first = InputImage::from_base64("image/png", &data).unwrap();
        assert_eq!(
            first.id,
            InputImage::from_base64("image/png", &data).unwrap().id
        );
        assert!(InputImage::from_base64("image/jpeg", &data).is_err());
        assert!(InputImage::from_base64("image/png", "!!!").is_err());
        assert!(InputImage::from_base64("image/png", "").is_err());
        assert!(InputImage::from_base64("image/svg+xml", &data).is_err());
        assert!(InputImage::from_base64(
            "image/png",
            &STANDARD.encode(vec![0; MAX_IMAGE_BYTES + 1])
        )
        .is_err());
    }
}
