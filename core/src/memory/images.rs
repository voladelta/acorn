//! Images
//! ======
//! A message's images and other media (video, audio, documents), in
//! docs/images/ by their hash and kind; each image with a copy scaled for
//! models beside it. Models see the images; the rest, by its path.

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
    let ok = hash.len() == 16
      && hash.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
      && !ext.is_empty()
      && ext.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    let file = self.images_dir().join(name);
    (ok && file.exists()).then_some(file)
  }

  /// What models get of message id's images: their copies, if any (its
  /// other media, none).
  pub fn images(&self, id: u64) -> Vec<Img> {
    let names = self.message(id).map_or_default(|m| m.images);
    let dir = self.images_dir();
    names
      .iter()
      .filter(|name| is_image(name))
      .filter_map(|name| {
        let fit = dir.join(format!("{name}.jpg"));
        let (file, mime) = if fit.exists() {
          (fit, "image/jpeg".to_string())
        } else {
          (
            dir.join(name),
            format!("image/{}", name.rsplit('.').next().unwrap_or("png")),
          )
        };
        let bytes = fs::read(file).ok()?;
        if mime == "image/gif" {
          return image_for_model(&bytes).ok().map(|(img, _)| img);
        }
        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
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

/// Whether a chat's file (by its name) is an image models see.
pub fn is_image(name: &str) -> bool {
  matches!(name.rsplit('.').next(), Some("png" | "jpeg" | "gif" | "webp"))
}

/// A file's kind by its first bytes, as an extension: an image's
/// (`image_kind`), a video's, a sound's, a document's; else txt for UTF-8
/// text, bin for anything else.
pub fn media_kind(bytes: &[u8]) -> &'static str {
  if let Some(kind) = image_kind(bytes) {
    return &kind["image/".len()..];
  }
  let at = |k: usize, mark: &[u8]| bytes.get(k..k + mark.len()) == Some(mark);
  let has = |mark: &[u8], within: usize| bytes[..bytes.len().min(within)].windows(mark.len()).any(|w| w == mark);
  if at(0, b"%PDF") {
    "pdf"
  } else if at(4, b"ftyp") {
    match bytes.get(8..12) {
      Some(b"M4A " | b"M4B ") => "m4a",
      Some(b"qt  ") => "mov",
      _ => "mp4",
    }
  } else if at(0, b"\x1A\x45\xDF\xA3") {
    if has(b"webm", 64) { "webm" } else { "mkv" }
  } else if at(0, b"OggS") {
    "ogg"
  } else if at(0, b"fLaC") {
    "flac"
  } else if at(0, b"RIFF") && at(8, b"WAVE") {
    "wav"
  } else if at(0, b"RIFF") && at(8, b"AVI ") {
    "avi"
  } else if at(0, b"ID3") || (bytes.len() > 1 && bytes[0] == 0xFF && bytes[1] & 0xE6 == 0xE2) {
    "mp3"
  } else if bytes.len() > 1 && bytes[0] == 0xFF && bytes[1] & 0xF6 == 0xF0 {
    "aac"
  } else if at(0, b"PK\x03\x04") {
    zip_office_kind(bytes).unwrap_or("zip")
  } else if std::str::from_utf8(bytes).is_ok() {
    "txt"
  } else {
    "bin"
  }
}

fn zip_office_kind(bytes: &[u8]) -> Option<&'static str> {
  if bytes.get(..4) != Some(b"PK\x03\x04") {
    return None;
  }
  if let Ok(archive) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) {
    let mut kind = None;
    for name in archive.file_names().flatten() {
      if name.starts_with("word/") {
        return Some("docx");
      }
      if name.starts_with("ppt/") {
        kind = Some("pptx");
      } else if name.starts_with("xl/") && kind.is_none() {
        kind = Some("xlsx");
      }
    }
    return kind;
  }
  let head = &bytes[..bytes.len().min(4096)];
  let has = |mark: &[u8]| head.windows(mark.len()).any(|w| w == mark);
  if has(b"word/") {
    Some("docx")
  } else if has(b"ppt/") {
    Some("pptx")
  } else if has(b"xl/") {
    Some("xlsx")
  } else {
    None
  }
}

/// A file's bytes put in a chat's dir by their hash and kind: an image
/// as `image_add` puts it (one that won't decode, as it is), anything else
/// as it is; its name. Blocking: run off the chat's lock.
pub fn media_add(dir: &Path, bytes: &[u8]) -> Result<String> {
  if image_kind(bytes).is_some()
    && let Ok(name) = image_add(dir, bytes)
  {
    return Ok(name);
  }
  use sha2::Digest;
  let hash = sha2::Sha256::digest(bytes);
  let hex: String = hash.iter().take(8).map(|b| format!("{b:02x}")).collect();
  let kind = match media_kind(bytes) {
    // an image that won't decode is no image to models
    "png" | "jpeg" | "gif" | "webp" => "bin",
    kind => kind,
  };
  let name = format!("{hex}.{kind}");
  let images = dir.join(IMAGES);
  fs::create_dir_all(&images)?;
  let file = images.join(&name);
  fs::write(&file, bytes)?;
  disk::wrote(&file);
  Ok(name)
}

/// An image read: its kind, its width and height, and its copy for models
/// if it needs one.
struct Fitted {
  kind: &'static str,
  small: Option<Vec<u8>>,
  size: (u32, u32),
}

/// An image read, its copy for models scaled to FIT, as a JPEG, for GIFs
/// or when larger than that or IMAGE bytes (and no more than PIXELS decoded; a
/// larger one goes as is).
fn fitted(bytes: &[u8]) -> Result<Fitted> {
  let kind = image_kind(bytes).context(IMAGES_ARE)?;
  // its size is read off its header: one over PIXELS is never decoded
  let (w, h) = image::ImageReader::new(std::io::Cursor::new(bytes))
    .with_guessed_format()
    .ok()
    .and_then(|r| r.into_dimensions().ok())
    .context(IMAGES_ARE)?;
  let as_is = Fitted {
    kind,
    small: None,
    size: (w, h),
  };
  if u64::from(w) * u64::from(h) > PIXELS {
    return Ok(as_is);
  }
  let img = image::load_from_memory(bytes).context(IMAGES_ARE)?;
  let k = (FIT / f64::from(w.max(h))).min(1.0);
  if !(k < 1.0 || bytes.len() > IMAGE || kind == "image/gif") {
    return Ok(as_is);
  }
  let small = img.resize_exact(
    ((f64::from(w) * k).round() as u32).max(1),
    ((f64::from(h) * k).round() as u32).max(1),
    image::imageops::FilterType::Lanczos3,
  );
  let mut out = Vec::new();
  image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85).encode_image(&small.to_rgb8())?;
  Ok(Fitted {
    kind,
    small: Some(out),
    size: (w, h),
  })
}

/// An image as models get it (its copy, if it needs one), and its width
/// and height. Blocking.
pub fn image_for_model(bytes: &[u8]) -> Result<(Img, (u32, u32))> {
  let Fitted { kind, small, size } = fitted(bytes)?;
  anyhow::ensure!(
    kind != "image/gif" || small.is_some(),
    "GIF dimensions exceed the model's image limit"
  );
  let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
  let img = match small {
    Some(jpeg) => Img {
      mime: "image/jpeg".into(),
      data: b64(&jpeg),
    },
    None => Img {
      mime: kind.into(),
      data: b64(bytes),
    },
  };
  Ok((img, size))
}

/// An image's bytes put in a chat's dir by their hash, its copy for models
/// beside it (`fitted`); its name. Blocking: run off the chat's lock.
pub fn image_add(dir: &Path, bytes: &[u8]) -> Result<String> {
  use sha2::Digest;
  let Fitted { kind, small, .. } = fitted(bytes)?;
  let hash = sha2::Sha256::digest(bytes);
  let hex: String = hash.iter().take(8).map(|b| format!("{b:02x}")).collect();
  let name = format!("{hex}.{}", &kind["image/".len()..]);
  let images = dir.join(IMAGES);
  fs::create_dir_all(&images)?;
  if let Some(jpeg) = small {
    let file = images.join(format!("{name}.jpg"));
    fs::write(&file, jpeg)?;
    disk::wrote(&file);
  }
  let file = images.join(&name);
  fs::write(&file, bytes)?;
  disk::wrote(&file);
  Ok(name)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_thin_image_keeps_both_dimensions() {
    let mut png = Vec::new();
    image::RgbImage::new(10000, 1)
      .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
      .unwrap();
    let fitted = fitted(&png).unwrap();
    let small = image::load_from_memory(&fitted.small.unwrap()).unwrap();
    assert!(small.width() > 0);
    assert_eq!(small.height(), 1);
  }

  #[test]
  fn an_image_over_the_pixels_goes_as_is_undecoded() {
    let mut png = Vec::new();
    image::RgbImage::new(2, 2)
      .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
      .unwrap();
    // its header says 40000 x 40000, over PIXELS; its data is for 2 x 2,
    // so a decode would fail
    png[16..20].copy_from_slice(&40000u32.to_be_bytes());
    png[20..24].copy_from_slice(&40000u32.to_be_bytes());
    let crc = png[12..29].iter().fold(!0u32, |crc, &b| {
      (0..8).fold(crc ^ u32::from(b), |c, _| {
        (c >> 1) ^ (0xEDB8_8320 & (c & 1).wrapping_neg())
      })
    });
    png[29..33].copy_from_slice(&(!crc).to_be_bytes());
    assert!(image::load_from_memory(&png).is_err());
    let fitted = fitted(&png).unwrap();
    assert_eq!(fitted.size, (40000, 40000));
    assert!(fitted.small.is_none());
  }

  #[test]
  fn gifs_are_sent_as_jpegs() {
    let mut gif = Vec::new();
    image::RgbaImage::new(4, 3)
      .write_to(&mut std::io::Cursor::new(&mut gif), image::ImageFormat::Gif)
      .unwrap();
    let (img, size) = image_for_model(&gif).unwrap();
    assert_eq!(img.mime, "image/jpeg");
    assert_eq!(size, (4, 3));
  }

  #[test]
  fn a_file_is_known_by_its_first_bytes() {
    let cases: [(&[u8], &str); 12] = [
      (b"%PDF-1.7", "pdf"),
      (b"\0\0\0\x20ftypisom\0\0", "mp4"),
      (b"\0\0\0\x20ftypM4A \0\0", "m4a"),
      (b"\0\0\0\x14ftypqt  \0\0", "mov"),
      (b"\x1A\x45\xDF\xA3\x9f\x42\x82\x84webm", "webm"),
      (b"OggS\0\x02", "ogg"),
      (b"ID3\x04\0", "mp3"),
      (b"\xFF\xFB\x90\x00", "mp3"),
      (b"RIFF\0\0\0\0WAVEfmt ", "wav"),
      (b"PK\x03\x04\x14\0\0\0[Content_Types].xmlword/document.xml", "docx"),
      (b"plain words", "txt"),
      (b"\x00\x01\xFE\xFF", "bin"),
    ];
    for (bytes, kind) in cases {
      assert_eq!(media_kind(bytes), kind, "{bytes:?}");
    }
  }

  #[test]
  fn media_is_kept_by_its_kind_and_models_see_images_alone() {
    let dir = tempfile::tempdir().unwrap();
    let video = media_add(dir.path(), b"\0\0\0\x20ftypisom\0\0more").unwrap();
    assert!(video.ends_with(".mp4") && video.len() == 20, "{video}");
    assert!(dir.path().join(IMAGES).join(&video).is_file());
    let mut png = Vec::new();
    image::RgbImage::new(4, 3)
      .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
      .unwrap();
    let pic = media_add(dir.path(), &png).unwrap();
    assert!(pic.ends_with(".png"));
    assert!(is_image(&pic) && !is_image(&video));
    let broken = media_add(dir.path(), b"\x89PNG not really").unwrap();
    assert!(broken.ends_with(".bin"), "an image that won't decode: {broken}");
    let doc = media_add(dir.path(), b"%PDF-1.7\ndocument").unwrap();
    let mut mem = Memory::load(dir.path()).unwrap();
    let mut msg = Msg::new(Kind::User, "attachments");
    msg.images = vec![video.clone(), pic.clone(), doc.clone(), broken];
    mem.push(vec![msg]).unwrap();
    let payload = mem.images(0);
    assert_eq!(
      payload,
      [Img {
        mime: "image/png".into(),
        data: base64::engine::general_purpose::STANDARD.encode(&png),
      }]
    );
    let mut msg = Msg::new(Kind::User, "documents alone");
    msg.images = vec![video, doc];
    mem.push(vec![msg]).unwrap();
    assert!(mem.images(1).is_empty());
    assert!(mem.images(2).is_empty());
  }
}
