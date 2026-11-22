//! DeepSeek
//! ========
//! DeepSeek in Acorn's own loop (`agent`), through its stateless Responses
//! API. Each request sends the full conversation, streams its output, and
//! tells its usage in its final event. DeepSeek caches prompts itself. Its
//! thoughts come whole (reasoning_text), and every output goes back as it
//! came, thoughts too. A server_error is busy; a failed, cut (incomplete)
//! or refused answer fails. Silent while it reasons, it is dead after IDLE.

use super::{Ask, Out, Spec};
use crate::price::Usage;
use crate::{Img, Kind};
use serde_json::{Value, json};
use std::time::Duration;

pub use crate::wire::MODEL;
pub const URL: &str = "https://api.deepseek.com/responses";
/// A request's most output tokens: DeepSeek's documented cap.
pub const MAX_OUTPUT_TOKENS: u64 = 384_000;
pub const IDLE: Duration = Duration::from_mins(20);

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

/// A request: `room`, the most tokens its output may take (they count
/// against the model's context, as its input does).
pub fn body(ask: &Ask, items: &[Item], tools: &[Spec], room: u64) -> Value {
  let tools: Vec<Value> = tools
    .iter()
    .map(|t| json!({"type": "function", "name": t.name, "description": t.info, "parameters": t.schema}))
    .collect();
  let mut body = json!({
      "model": MODEL,
      "stream": true,
      "tools": tools,
      "reasoning": {"effort": ask.effort.as_str()},
      "max_output_tokens": room,
      "instructions": ask.system,
      "input": items.iter().flat_map(input).collect::<Vec<_>>(),
  });
  if let Some(user) = &ask.user {
    body["user"] = json!(user);
  }
  body
}

/// What makes an item what it is, and its images: veiled, a request would
/// be refused.
const SHAPE: [&str; 8] = [
  "type",
  "role",
  "id",
  "call_id",
  "name",
  "status",
  "encrypted_content",
  "image_url",
];

/// A request with each secret written `[purged]` in its system prompt and
/// in its input: every text of every item, but its SHAPE.
pub fn veil(body: &mut Value, secrets: &[String]) {
  if !secrets.is_empty() {
    veil_in(&mut body["instructions"], secrets, false);
    veil_in(&mut body["input"], secrets, false);
  }
}

/// Every string of a JSON value veiled, but an item's SHAPE (`free`: none
/// is kept). One that is itself JSON (a tool call's arguments) may hold a
/// secret escaped, so it is veiled as parsed, all of it free.
fn veil_in(v: &mut Value, secrets: &[String], free: bool) {
  match v {
    Value::String(s) => {
      if let Some(mut inner) = s
        .starts_with(['{', '['])
        .then(|| serde_json::from_str::<Value>(s).ok())
        .flatten()
      {
        let was = inner.clone();
        veil_in(&mut inner, secrets, true);
        if inner != was {
          *s = inner.to_string();
        }
      }
      super::veil(s, secrets);
    }
    Value::Array(a) => a.iter_mut().for_each(|v| veil_in(v, secrets, free)),
    Value::Object(o) => o
      .iter_mut()
      .filter(|(k, _)| free || !SHAPE.contains(&k.as_str()))
      .for_each(|(_, v)| veil_in(v, secrets, free)),
    _ => {}
  }
}

/// Of a refusal for the model's context ("This model's maximum context
/// length is W tokens. However, you requested N tokens (M in the messages,
/// K in the completion)"), the tokens its messages leave for an output:
/// W - M; None: another failure.
pub fn left(why: &str) -> Option<u64> {
  let num = |before: &str, after: &str| -> Option<u64> {
    let (head, _) = why.split_once(after)?;
    head.rsplit_once(before)?.1.trim().parse().ok()
  };
  let most = num("maximum context length is ", " tokens")?;
  Some(most.saturating_sub(num("(", " in the messages")?))
}

fn input(item: &Item) -> Vec<Value> {
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
        json!({"type": "function_call_output", "call_id": id, "output": output})
      })
      .collect(),
  }
}

fn image(img: &Img) -> Value {
  json!({"type": "input_image", "image_url": format!("data:{};base64,{}", img.mime, img.data)})
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
