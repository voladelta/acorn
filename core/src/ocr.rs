//! OCR
//! ===
//! A document's text (PDF, Word, PowerPoint, or an image), recognized page
//! by page by DeepSeek's OCR API on the same key as its model, for luau's
//! `ocr`. Nothing is kept on DeepSeek's side. A PDF is read for its layout
//! (rich_layout_recognition), anything else for its text
//! (text_recognition), which reads Word and PowerPoint directly.

use crate::web;
use serde_json::Value;
use std::path::Path;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub const URL: &str = "https://api.deepseek.com/v1/tools/ocr";

/// The largest document the API takes.
pub const MOST_BYTES: u64 = 100 << 20;
/// OCR has 180 s on DeepSeek's side, upload and all.
const TIMEOUT: Duration = Duration::from_secs(300);

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

/// A file read to send: its name and bytes, if it is not too big.
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

/// A file's text, recognized page by page (`pages`).
pub async fn read(path: &Path, cancel: &CancellationToken) -> Result<String, String> {
  let (name, bytes) = load(path)?;
  let (kind, body) = form(&[("purpose", purpose(path, &bytes).to_string())], &name, &bytes);
  let got = web::api(URL, &kind, body, TIMEOUT, cancel).await;
  let got = got
    .and_then(failed)
    .map_err(|why| format!("{}: {why}", path.display()))?;
  Ok(pages(&got))
}

/// A reply, unless it is an error: once recognition has begun the status is
/// already 200, so one that fails or runs out of time says so in the
/// body alone.
fn failed(got: Value) -> Result<Value, String> {
  match &got["error"] {
    Value::Null => Ok(got),
    err => Err(err["message"].as_str().unwrap_or("failed").to_string()),
  }
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
      if text.is_empty() {
        // Recognition misses a page now and then; asked again, it may not.
        text = "[no text recognized on this page: ocr again to retry]".into();
      }
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
  }

  #[test]
  fn the_form_sends_purpose_first_and_a_safe_name() {
    let fields = [("purpose", "text_recognition".to_string()), ("then", "x".to_string())];
    let (kind, body) = form(&fields, "a\"b\r\n.pdf", b"%PDF-");
    let line = kind.strip_prefix("multipart/form-data; boundary=").unwrap();
    let body = String::from_utf8(body).unwrap();
    let purpose = body.find("name=\"purpose\"\r\n\r\ntext_recognition\r\n").unwrap();
    let file = body.find("name=\"file\"; filename=\"a_b__.pdf\"").unwrap();
    assert!(purpose < body.find("name=\"then\"").unwrap() && purpose < file);
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
    let blank = json!({"pages": [{"index": 0, "text": " \n", "finish_reason": "stop"}]});
    assert!(pages(&blank).starts_with("[no text recognized"));
  }

  #[test]
  fn an_error_in_a_200_reply_is_a_failure() {
    let err = json!({"error": {"message": "OCR did not finish in time", "code": "gateway_timeout_error"}});
    assert_eq!(failed(err).unwrap_err(), "OCR did not finish in time");
    assert!(failed(json!({"pages": []})).is_ok());
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

  /// Reads a PDF for real; run with `cargo test -- --ignored` and
  /// DEEPSEEK_API_KEY.
  #[tokio::test]
  #[ignore]
  async fn a_pdf_reads() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = dir.path().join("hello.pdf");
    std::fs::write(&pdf, HELLO_PDF).unwrap();
    let text = read(&pdf, &CancellationToken::new()).await.unwrap();
    assert!(text.contains("Hello"), "{text}");
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
