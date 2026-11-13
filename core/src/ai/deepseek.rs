//! DeepSeek
//! ========
//! DeepSeek in Acorn's own loop (`agent`), through its stateless Responses
//! API. Each request sends the full conversation, streams its output, and
//! tells its usage in its final event. DeepSeek caches prompts itself. Its
//! thoughts come whole (reasoning_text), and every output goes back as it
//! came, thoughts too. A server_error is busy; a failed, cut (incomplete)
//! or refused answer fails. Silent while it reasons, it is dead after IDLE.

use super::{Ask, Doc, Out, Spec};
use crate::price::Usage;
use crate::{Img, Kind};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::time::Duration;

pub use crate::wire::MODEL;
pub const URL: &str = "https://api.deepseek.com/responses";
/// A request's most output tokens: DeepSeek's documented cap.
pub const MAX_OUTPUT_TOKENS: u64 = 384_000;
pub const IDLE: Duration = Duration::from_secs(1200);
/// The most documents a request may reference: DeepSeek's documented cap.
pub const DOCS: usize = 20;
/// Seconds before its upload expires that a document is no longer sent,
/// so a request never names one gone by the time it is read.
const DOC_MARGIN: i64 = 60;

/// What a request sends after the system prompt: the user's text and
/// images, the model's output as its API gave it, its reply so far as
/// a message cut it off, tool results.
pub enum Item {
  User { parts: Vec<String>, imgs: Vec<Img> },
  Turn(Vec<Value>),
  Said(String),
  Back(Vec<(String, Out)>),
}

/// A tool call: its input (a string: not JSON).
pub struct Use {
  pub id: String,
  pub name: String,
  pub input: Value,
}

/// A message of an output: its kind, text, and tool call.
pub struct Entry {
  pub kind: Kind,
  pub text: String,
  pub of: Option<String>,
}

/// How a step ended: answered (`Done`), with tools to run, or not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Halt {
  Done,
  Tools,
  Busy,
  Down,
  /// Not sent: the daily budget is spent (`chat::over_budget`).
  Capped,
  Lost,
  Fail,
}

pub struct Step {
  pub out: Vec<Value>,
  pub uses: Vec<Use>,
  pub entries: Vec<Entry>,
  pub halt: Halt,
  pub why: String,
}

pub fn body(ask: &Ask, items: &[Item], tools: &[Spec]) -> Value {
  let tools: Vec<Value> = tools
    .iter()
    .map(|t| json!({"type": "function", "name": t.name, "description": t.info, "parameters": t.schema}))
    .collect();
  let live = attached(items, chrono::Utc::now().timestamp());
  let mut body = json!({
      "model": MODEL,
      "stream": true,
      "tools": tools,
      "reasoning": {"effort": ask.effort.as_str()},
      "max_output_tokens": MAX_OUTPUT_TOKENS,
      "instructions": ask.system,
      "input": items.iter().flat_map(|i| input(i, &live)).collect::<Vec<_>>(),
  });
  if let Some(user) = &ask.user {
    body["user"] = json!(user);
  }
  body
}

/// The documents a request at `now` references: the latest DOCS whose
/// uploads still last. Any other is named in words instead (`input`), and
/// that only once it expires or is crowded out, so the prompt's prefix,
/// and DeepSeek's cache of it, stays put till then.
fn attached(items: &[Item], now: i64) -> HashSet<&str> {
  items
    .iter()
    .rev()
    .filter_map(|i| match i {
      Item::Back(backs) => Some(backs),
      _ => None,
    })
    .flat_map(|backs| backs.iter().rev().flat_map(|(_, out)| out.docs.iter().rev()))
    .filter(|d| d.until - DOC_MARGIN > now)
    .take(DOCS)
    .map(|d| d.id.as_str())
    .collect()
}

fn input(item: &Item, live: &HashSet<&str>) -> Vec<Value> {
  match item {
    Item::User { parts, imgs } => {
      let mut content: Vec<Value> = parts.iter().map(|t| json!({"type": "input_text", "text": t})).collect();
      content.extend(imgs.iter().map(image));
      vec![json!({"role": "user", "content": content})]
    }
    Item::Turn(out) => out.clone(),
    Item::Said(text) => vec![json!({"role": "assistant", "content": text})],
    Item::Back(backs) => backs
      .iter()
      .map(|(id, out)| {
        let mut output = vec![json!({"type": "input_text", "text": out.text})];
        output.extend(out.imgs.iter().map(image));
        output.extend(out.docs.iter().map(|d| doc(d, live)));
        json!({"type": "function_call_output", "call_id": id, "output": output})
      })
      .collect(),
  }
}

fn image(img: &Img) -> Value {
  json!({"type": "input_image", "image_url": format!("data:{};base64,{}", img.mime, img.data)})
}

fn doc(d: &Doc, live: &HashSet<&str>) -> Value {
  if live.contains(d.id.as_str()) {
    json!({"type": "input_file", "file_id": d.id})
  } else {
    json!({"type": "input_text", "text": format!("[{} is no longer attached: view it again to read it]", d.name)})
  }
}

/// A Responses stream read so far: the final response, or an error event;
/// its usage.
#[derive(Default)]
pub struct Got {
  res: Option<Value>,
  error: Option<Value>,
  pub usage: Usage,
}

impl Got {
  /// One stream event, its pieces told to `on` (a kind, its text, whether
  /// it starts a message); whether it is the last.
  pub fn read(&mut self, ev: &Value, on: &mut impl FnMut(Kind, &str, bool)) -> bool {
    let str = |k: &str| ev[k].as_str().unwrap_or_default();
    match str("type") {
      "response.output_text.delta" => on(Kind::Acorn, str("delta"), false),
      "response.output_item.added" if ev["item"]["type"] == "function_call" => on(
        Kind::Tool,
        &format!("{} ", ev["item"]["name"].as_str().unwrap_or_default()),
        true,
      ),
      "response.function_call_arguments.delta" => on(Kind::Tool, str("delta"), false),
      "response.reasoning_text.delta" => on(Kind::Mind, str("delta"), false),
      "response.completed" | "response.incomplete" | "response.failed" => {
        let usage = &ev["response"]["usage"];
        let num = |v: &Value| v.as_u64().unwrap_or(0);
        self.usage.read = num(&usage["input_tokens_details"]["cached_tokens"]);
        self.usage.input = num(&usage["input_tokens"]).saturating_sub(self.usage.read);
        self.usage.output = num(&usage["output_tokens"]);
        self.res = Some(ev["response"].clone());
        return true;
      }
      "error" => {
        self.error = Some(ev.clone());
        return true;
      }
      _ => {}
    }
    false
  }

  /// The event its response begins with.
  pub fn begun(ev: &Value) -> bool {
    ev["type"] == "response.output_item.added"
  }

  /// The step a stream gave (its usage stays).
  pub fn step(&mut self) -> Step {
    let res = self
      .res
      .take()
      .unwrap_or_else(|| json!({"status": "failed", "error": self.error.take(), "output": []}));
    let out: Vec<Value> = res["output"].as_array().cloned().unwrap_or_default();
    let uses: Vec<Use> = out
      .iter()
      .filter(|o| o["type"] == "function_call")
      .map(|o| {
        let args = o["arguments"].as_str().unwrap_or_default();
        let json = if args.is_empty() { "{}" } else { args };
        Use {
          id: o["call_id"].as_str().unwrap_or_default().into(),
          name: o["name"].as_str().unwrap_or_default().into(),
          input: serde_json::from_str(json).unwrap_or_else(|_| Value::String(args.into())),
        }
      })
      .collect();
    let refusals: Vec<String> = out
      .iter()
      .filter(|o| o["type"] == "message")
      .flat_map(|o| o["content"].as_array().cloned().unwrap_or_default())
      .filter(|c| c["type"] == "refusal")
      .map(|c| c["refusal"].as_str().unwrap_or_default().to_string())
      .collect();
    let entries = out.iter().flat_map(entries).collect();
    let mut step = Step {
      out,
      uses,
      entries,
      halt: Halt::Done,
      why: String::new(),
    };
    match res["status"].as_str() {
      Some("failed") => {
        let (code, kind) = (&res["error"]["code"], &res["error"]["type"]);
        let busy = ["server_error", "rate_limit_error"].iter().any(|c| code == c)
          || ["server_error", "overloaded_error", "rate_limit_error"]
            .iter()
            .any(|t| kind == t);
        step.halt = if busy { Halt::Busy } else { Halt::Fail };
        step.why = res["error"]["message"].as_str().unwrap_or("failed").into();
      }
      Some("incomplete") => {
        step.halt = Halt::Fail;
        step.why = format!(
          "incomplete: {}",
          res["incomplete_details"]["reason"].as_str().unwrap_or("undefined")
        );
      }
      _ if !refusals.is_empty() => {
        step.halt = Halt::Fail;
        step.why = format!("refused: {}", refusals.join(" "));
      }
      _ => step.halt = if step.uses.is_empty() { Halt::Done } else { Halt::Tools },
    }
    step
  }
}

/// The messages of an output item: a thought, a reply, a tool call.
fn entries(o: &Value) -> Vec<Entry> {
  fn texts(v: &Value) -> impl Iterator<Item = String> + '_ {
    v.as_array()
      .into_iter()
      .flatten()
      .map(|s| s["text"].as_str().unwrap_or_default().to_string())
  }
  match o["type"].as_str() {
    Some("reasoning") => {
      let text = texts(&o["summary"])
        .chain(texts(&o["content"]))
        .collect::<Vec<_>>()
        .join("\n\n");
      let text = text.trim();
      if text.is_empty() {
        vec![]
      } else {
        vec![Entry {
          kind: Kind::Mind,
          text: text.into(),
          of: None,
        }]
      }
    }
    Some("message") => {
      let text = texts(&o["content"]).collect::<String>();
      let text = text.trim();
      if text.is_empty() {
        vec![]
      } else {
        vec![Entry {
          kind: Kind::Acorn,
          text: text.into(),
          of: None,
        }]
      }
    }
    Some("function_call") => {
      let text = format!(
        "{} {}",
        o["name"].as_str().unwrap_or_default(),
        o["arguments"].as_str().unwrap_or_default()
      );
      vec![Entry {
        kind: Kind::Tool,
        text,
        of: o["call_id"].as_str().map(str::to_string),
      }]
    }
    _ => vec![],
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn back(docs: &[(&str, i64)]) -> Item {
    let docs = docs
      .iter()
      .map(|(id, until)| Doc {
        id: id.to_string(),
        name: format!("{id}.pdf"),
        until: *until,
      })
      .collect();
    Item::Back(vec![(
      "c".into(),
      Out {
        docs,
        ..Out::ok("read")
      },
    )])
  }

  #[test]
  fn a_document_is_sent_while_its_upload_lasts_and_then_named() {
    let now = 1_000_000;
    let items = [back(&[("old", now + 30), ("new", now + 3600)])];
    let live = attached(&items, now);
    let sent = input(&items[0], &live);
    assert_eq!(
      sent[0]["output"],
      json!([
        {"type": "input_text", "text": "read"},
        {"type": "input_text", "text": "[old.pdf is no longer attached: view it again to read it]"},
        {"type": "input_file", "file_id": "new"},
      ])
    );
  }

  #[test]
  fn only_the_latest_documents_go_past_the_cap() {
    let now = 0;
    let items: Vec<Item> = (0..DOCS + 2).map(|k| back(&[(&format!("d{k}"), 3600)])).collect();
    let live = attached(&items, now);
    assert_eq!(live.len(), DOCS);
    assert!(!live.contains("d0") && !live.contains("d1") && live.contains("d2"));
    assert!(live.contains(format!("d{}", DOCS + 1).as_str()));
  }
}
