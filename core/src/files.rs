//! Files
//! =====
//! Documents (PDF, Word, PowerPoint) read through DeepSeek's parsing, on
//! the same key as its model: `upload` parses one into its Files API, for
//! the model to read by id (luau's `view`) till the upload expires; `ocr`
//! gives its text, page by page, for code (luau's `ocr`), and keeps
//! nothing. A PDF is read for its layout, figures and all
//! (rich_layout_recognition); anything else for its text
//! (text_recognition), which reads Word and PowerPoint directly.

use crate::ai::Doc;
use crate::web;
use serde_json::Value;
use std::path::Path;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub const FILES: &str = "https://api.deepseek.com/files";
pub const OCR: &str = "https://api.deepseek.com/v1/tools/ocr";

/// An upload's life by default, and the shortest and longest the API takes.
pub const EXPIRES: u64 = 3600;
const EXPIRES_MAX: u64 = 30 * 24 * 3600;
/// The largest document the API takes.
pub const MOST_BYTES: u64 = 100 << 20;
/// Upload and parsing each have 120 s on DeepSeek's side; a reply comes
/// only when both are done.
const TIMEOUT: Duration = Duration::from_secs(300);

/// The documents the Files API parses, by extension.
const DOCS: [&str; 5] = ["pdf", "doc", "docx", "ppt", "pptx"];

pub const DOCS_ARE: &str = "Documents are PDF, Word (doc, docx) or PowerPoint (ppt, pptx).";

/// Whether a path names a document, by its extension.
pub fn is_doc(path: &Path) -> bool {
  ext(path).is_some_and(|x| DOCS.contains(&x.as_str()))
}

fn ext(path: &Path) -> Option<String> {
  Some(path.extension()?.to_str()?.to_ascii_lowercase())
}

/// How a file is parsed: a PDF (by its first bytes or its name) for its
/// layout, anything else for its text.
fn purpose(path: &Path, bytes: &[u8]) -> &'static str {
  if bytes.starts_with(b"%PDF-") || ext(path).as_deref() == Some("pdf") {
    "rich_layout_recognition"
  } else {
    "text_recognition"
  }
}

/// A file read for upload: its name and bytes, if it is not too big.
fn load(path: &Path) -> Result<(String, Vec<u8>), String> {
  let shown = path.display();
  let meta = std::fs::metadata(path).map_err(|e| format!("{shown}: {e}"))?;
  if !meta.is_file() {
    return Err(format!("{shown}: not a regular file"));
  }
  if meta.len() > MOST_BYTES {
    return Err(format!("{shown}: {} bytes, over {} MB", meta.len(), MOST_BYTES >> 20));
  }
  let bytes = std::fs::read(path).map_err(|e| format!("{shown}: {e}"))?;
  let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
  Ok((name, bytes))
}

/// A multipart/form-data body: its fields in order, then the file. The
/// order matters: DeepSeek needs `purpose` before `file`.
fn form(fields: &[(&str, String)], name: &str, bytes: &[u8]) -> (String, Vec<u8>) {
  let line = format!("acorn-{:032x}", rand::random::<u128>());
  let mut body = Vec::with_capacity(bytes.len() + 512);
  for (k, v) in fields {
    body.extend(format!("--{line}\r\ncontent-disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n").bytes());
  }
  // A quote or line break would end the header; the name is only a label.
  let name: String = name
    .chars()
    .map(|c| if c == '"' || c.is_control() { '_' } else { c })
    .collect();
  body.extend(
    format!(
      "--{line}\r\ncontent-disposition: form-data; name=\"file\"; filename=\"{name}\"\r\ncontent-type: application/octet-stream\r\n\r\n"
    )
    .bytes(),
  );
  body.extend_from_slice(bytes);
  body.extend(format!("\r\n--{line}--\r\n").bytes());
  (format!("multipart/form-data; boundary={line}"), body)
}

/// A file sent to one of the APIs with its purpose (and `fields` after it);
/// the reply, or why it failed.
async fn send(url: &str, path: &Path, fields: &[(&str, String)], cancel: &CancellationToken) -> Result<Value, String> {
  let (name, bytes) = load(path)?;
  let mut all = vec![("purpose", purpose(path, &bytes).to_string())];
  all.extend_from_slice(fields);
  let (kind, body) = form(&all, &name, &bytes);
  web::api(url, &kind, body, TIMEOUT, cancel)
    .await
    .map_err(|why| format!("{}: {why}", path.display()))
}

/// A document parsed into the Files API, to last `expires` seconds
/// (clamped to what the API takes).
pub async fn upload(path: &Path, expires: u64, cancel: &CancellationToken) -> Result<Doc, String> {
  let expires = expires.clamp(EXPIRES, EXPIRES_MAX);
  let fields = [
    ("expires_after[anchor]", "created_at".to_string()),
    ("expires_after[seconds]", expires.to_string()),
  ];
  let got = send(FILES, path, &fields, cancel).await?;
  let id = got["id"].as_str().filter(|id| !id.is_empty());
  let id = id.ok_or_else(|| format!("{}: the upload gave no id", path.display()))?;
  let until = got["expires_at"].as_i64().unwrap_or_else(|| {
    got["created_at"]
      .as_i64()
      .unwrap_or_else(|| chrono::Utc::now().timestamp())
      + expires as i64
  });
  Ok(Doc {
    id: id.into(),
    name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(),
    until,
  })
}

/// A file's text, recognized page by page (`pages`).
pub async fn ocr(path: &Path, cancel: &CancellationToken) -> Result<String, String> {
  Ok(pages(&send(OCR, path, &[], cancel).await?))
}

/// An OCR reply as text: its pages in order, each under a line naming it
/// when there are several, one cut short said so.
fn pages(got: &Value) -> String {
  let all = got["pages"].as_array().cloned().unwrap_or_default();
  let many = all.len() > 1;
  all
    .iter()
    .enumerate()
    .map(|(i, p)| {
      let n = p["index"].as_u64().map_or(i + 1, |k| k as usize + 1);
      let mut text = p["text"].as_str().unwrap_or_default().trim().to_string();
      if p["finish_reason"] == "length" {
        text.push_str("\n[page cut short: its text ran past the limit]");
      }
      if many {
        format!("--- page {n} ---\n{text}")
      } else {
        text
      }
    })
    .collect::<Vec<_>>()
    .join("\n\n")
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  #[test]
  fn a_pdf_is_read_for_its_layout_and_the_rest_for_text() {
    assert_eq!(purpose(Path::new("a.PDF"), b""), "rich_layout_recognition");
    assert_eq!(purpose(Path::new("scan"), b"%PDF-1.7\n"), "rich_layout_recognition");
    assert_eq!(purpose(Path::new("a.docx"), b"PK\x03\x04"), "text_recognition");
    assert_eq!(purpose(Path::new("a.png"), b"\x89PNG"), "text_recognition");
    assert!(is_doc(Path::new("deck.PPTX")) && is_doc(Path::new("x/report.pdf")));
    assert!(!is_doc(Path::new("notes.txt")) && !is_doc(Path::new("pdf")));
  }

  #[test]
  fn the_form_sends_purpose_first_and_a_safe_name() {
    let fields = [
      ("purpose", "text_recognition".to_string()),
      ("expires_after[seconds]", "3600".to_string()),
    ];
    let (kind, body) = form(&fields, "a\"b\r\n.pdf", b"%PDF-");
    let line = kind.strip_prefix("multipart/form-data; boundary=").unwrap();
    let body = String::from_utf8(body).unwrap();
    let purpose = body.find("name=\"purpose\"\r\n\r\ntext_recognition\r\n").unwrap();
    let file = body.find("name=\"file\"; filename=\"a_b__.pdf\"").unwrap();
    assert!(purpose < body.find("expires_after[seconds]").unwrap() && purpose < file);
    assert!(body.starts_with(&format!("--{line}\r\n")), "{body}");
    assert!(body.ends_with(&format!("%PDF-\r\n--{line}--\r\n")), "{body}");
  }

  #[test]
  fn pages_read_in_order_and_a_cut_one_says_so() {
    let one = json!({"pages": [{"index": 0, "text": " Hello \n", "finish_reason": "stop"}]});
    assert_eq!(pages(&one), "Hello");
    let two = json!({"pages": [
      {"index": 0, "text": "A", "finish_reason": "stop"},
      {"index": 1, "text": "B", "finish_reason": "length"},
    ]});
    assert_eq!(
      pages(&two),
      "--- page 1 ---\nA\n\n--- page 2 ---\nB\n[page cut short: its text ran past the limit]"
    );
    assert_eq!(pages(&json!({"pages": []})), "");
  }

  #[test]
  fn a_missing_or_huge_file_fails_before_any_request() {
    let dir = tempfile::tempdir().unwrap();
    assert!(load(&dir.path().join("none.pdf")).unwrap_err().contains("none.pdf"));
    assert!(load(dir.path()).unwrap_err().contains("not a regular file"));
    let big = dir.path().join("big.pdf");
    std::fs::File::create(&big).unwrap().set_len(MOST_BYTES + 1).unwrap();
    assert!(load(&big).unwrap_err().contains("over 100 MB"));
  }

  /// Parses a PDF for real; run with `cargo test -- --ignored` and
  /// DEEPSEEK_API_KEY.
  #[tokio::test]
  #[ignore]
  async fn a_pdf_uploads_and_reads() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = dir.path().join("hello.pdf");
    std::fs::write(&pdf, HELLO_PDF).unwrap();
    let cancel = CancellationToken::new();
    let text = ocr(&pdf, &cancel).await.unwrap();
    assert!(text.contains("Hello"), "{text}");
    let doc = upload(&pdf, EXPIRES, &cancel).await.unwrap();
    assert!(doc.id.starts_with("file-"), "{doc:?}");
    assert!(doc.until > chrono::Utc::now().timestamp() + 3000, "{doc:?}");
  }

  /// A one-page PDF saying "Hello Acorn".
  const HELLO_PDF: &str = "%PDF-1.4
1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj
2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj
3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 300 100]/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>>>endobj
4 0 obj<</Length 44>>stream
BT /F1 24 Tf 20 40 Td (Hello Acorn) Tj ET
endstream endobj
5 0 obj<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>endobj
trailer<</Root 1 0 R>>
%%EOF
";
}
