//! Images
//! ======
//! A message's images, in docs/images/ by their hash, each with a copy
//! scaled for models beside it.

use super::*;
use anyhow::Context;
use base64::Engine as _;

impl Memory {
    pub fn images_dir(&self) -> PathBuf {
        self.dir.join(IMAGES)
    }

    /// An image file of the chat by its name; None: no such.
    pub fn image_file(&self, name: &str) -> Option<PathBuf> {
        let (hash, ext) = name.split_once('.')?;
        let ok = hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) && !ext.is_empty() && ext.bytes().all(|b| b.is_ascii_lowercase());
        let file = self.images_dir().join(name);
        (ok && file.exists()).then_some(file)
    }

    /// What models get of message id's images: their copies, if any.
    pub fn images(&self, id: u64) -> Vec<Img> {
        let names = self.message(id).map(|m| m.images).unwrap_or_default();
        let dir = self.images_dir();
        names
            .iter()
            .filter_map(|name| {
                let fit = dir.join(format!("{name}.jpg"));
                let (file, mime) = if fit.exists() {
                    (fit, "image/jpeg".to_string())
                } else {
                    (dir.join(name), format!("image/{}", name.rsplit('.').next().unwrap_or("png")))
                };
                let data = base64::engine::general_purpose::STANDARD.encode(fs::read(file).ok()?);
                Some(Img { mime, data })
            })
            .collect()
    }
}

/// An image's kind by its first bytes; None: none of png, jpeg, gif, webp.
pub fn image_kind(head: &[u8]) -> Option<&'static str> {
    let at = |k: usize, mark: &[u8]| head.get(k..k + mark.len()) == Some(mark);
    if at(0, b"\x89PNG") {
        Some("image/png")
    } else if at(0, b"\xFF\xD8\xFF") {
        Some("image/jpeg")
    } else if at(0, b"GIF8") {
        Some("image/gif")
    } else if at(8, b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}

pub const IMAGES_ARE: &str = "Images are png, jpeg, gif or webp.";

/// An image's bytes put in a chat's dir by their hash, its copy for models
/// beside it (scaled to FIT, as a JPEG); its name. Blocking: run off the
/// chat's lock.
pub fn image_add(dir: &Path, bytes: &[u8]) -> Result<String> {
    use sha2::Digest;
    let kind = image_kind(bytes).context(IMAGES_ARE)?;
    let img = image::load_from_memory(bytes).context(IMAGES_ARE)?;
    let (w, h) = (img.width(), img.height());
    let hash = sha2::Sha256::digest(bytes);
    let hex: String = hash.iter().take(8).map(|b| format!("{b:02x}")).collect();
    let name = format!("{hex}.{}", &kind["image/".len()..]);
    let k = (FIT / w.max(h) as f64).min(1.0);
    let fit = (k < 1.0 || bytes.len() > IMAGE) && (w as u64) * (h as u64) <= PIXELS;
    let images = dir.join(IMAGES);
    fs::create_dir_all(&images)?;
    if fit {
        let small = img.resize_exact((w as f64 * k).round() as u32, (h as f64 * k).round() as u32, image::imageops::FilterType::Lanczos3);
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85).encode_image(&small.to_rgb8())?;
        let file = images.join(format!("{name}.jpg"));
        fs::write(&file, out)?;
        disk::wrote(&file);
    }
    let file = images.join(&name);
    fs::write(&file, bytes)?;
    disk::wrote(&file);
    Ok(name)
}
