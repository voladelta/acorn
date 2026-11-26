//! Stateless Gemini Interactions. Acorn keeps the conversation and replays
//! the model's steps, including thought signatures, across tool calls.

use super::{Ask, Out, Spec};
use crate::{Img, Kind, price::Usage};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};

pub const URL: &str = "https://generativelanguage.googleapis.com/v1beta/interactions";
pub const CONTEXT_TOKENS: u64 = 1_048_576;
pub const MAX_OUTPUT_TOKENS: u64 = 65_536;
pub const LEAST_OUTPUT_TOKENS: u64 = 32_768;
pub const IDLE: Duration = Duration::from_mins(20);

pub enum Item {
  User { parts: Vec<String>, imgs: Vec<Img> },
  Turn(Vec<Value>),
  Said(String),
  Back(Vec<(String, String, Out)>),
}

pub struct Use {
  pub id: String,
  pub name: String,
  pub input: Value,
}

pub struct Entry {
  pub kind: Kind,
  pub text: String,
  pub of: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Halt {
  Done,
  Tools,
  Busy,
  Down,
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

pub fn body(ask: &Ask, items: &[Item], tools: &[Spec], room: u64) -> Value {
  let mut body = json!({
    "model": ask.model.name(),
    "stream": true,
    "store": false,
    "system_instruction": ask.system,
    "generation_config": {
      "thinking_level": ask.model.thinking(ask.effort),
      "thinking_summaries": "auto",
      "max_output_tokens": room
    },
    "input": items.iter().flat_map(input).collect::<Vec<_>>()
  });
  if !tools.is_empty() {
    body["tools"] = Value::Array(
      tools
        .iter()
        .map(|t| {
          json!({
            "type": "function", "name": t.name, "description": t.info, "parameters": t.schema
          })
        })
        .collect(),
    );
  }
  body
}

pub fn text(s: &str) -> Value {
  json!({"type": "text", "text": s})
}

fn image(img: &Img) -> Value {
  json!({"type": "image", "mime_type": img.mime, "data": img.data})
}

fn input(item: &Item) -> Vec<Value> {
  match item {
    Item::User { parts, imgs } => {
      let mut content: Vec<Value> = parts.iter().map(|s| text(s)).collect();
      content.extend(imgs.iter().map(image));
      if content.is_empty() {
        content.push(text("Continue."));
      }
      vec![json!({"type": "user_input", "content": content})]
    }
    Item::Turn(out) => out.clone(),
    Item::Said(s) => vec![json!({"type": "model_output", "content": [text(s)]})],
    Item::Back(backs) => backs
      .iter()
      .map(|(id, name, out)| {
        let mut result = vec![text(&out.text)];
        result.extend(out.imgs.iter().map(image));
        json!({"type": "function_result", "call_id": id, "name": name, "is_error": out.fail, "result": result})
      })
      .collect(),
  }
}

pub fn veil(body: &mut Value, secrets: &[String]) {
  if secrets.is_empty() {
    return;
  }
  veil_in(&mut body["system_instruction"], secrets, true);
  veil_in(&mut body["input"], secrets, false);
}

fn veil_in(v: &mut Value, secrets: &[String], free: bool) {
  const SHAPE: &[&str] = &["type", "id", "call_id", "name", "signature", "mime_type", "data"];
  match v {
    Value::String(s) => {
      if s.starts_with(['{', '['])
        && let Ok(mut inner) = serde_json::from_str::<Value>(s)
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
    Value::Object(o) => {
      for (key, value) in o {
        if free || !SHAPE.contains(&key.as_str()) {
          veil_in(value, secrets, free || key == "arguments");
        }
      }
    }
    _ => {}
  }
}

pub fn usage(value: &Value) -> Usage {
  let num = |key| value[key].as_u64().unwrap_or(0);
  let read = num("total_cached_tokens");
  Usage {
    input: num("total_input_tokens").saturating_sub(read) + num("total_tool_use_tokens"),
    write: 0,
    read,
    output: num("total_output_tokens") + num("total_thought_tokens"),
  }
}

pub fn output_text(step: &Value) -> impl Iterator<Item = &str> {
  step["content"]
    .as_array()
    .into_iter()
    .flatten()
    .filter(|part| part["type"] == "text")
    .filter_map(|part| part["text"].as_str())
}

fn thought_text(step: &Value) -> impl Iterator<Item = &str> {
  step["summary"]
    .as_array()
    .into_iter()
    .flatten()
    .filter(|part| part["type"].is_null() || part["type"] == "text")
    .filter_map(|part| part["text"].as_str())
}

#[derive(Default)]
struct Building {
  step: Value,
  args: String,
  text: String,
  summary: String,
  closed: bool,
}

impl Building {
  fn start(&mut self, on: &mut impl FnMut(Kind, &str, bool)) {
    let step = &self.step;
    match step["type"].as_str() {
      Some("model_output") => {
        for part in output_text(step) {
          if !part.is_empty() {
            on(Kind::Acorn, part, self.text.is_empty());
            self.text.push_str(part);
          }
        }
      }
      Some("thought") => {
        for part in thought_text(step) {
          if !part.is_empty() {
            on(Kind::Mind, part, self.summary.is_empty());
            self.summary.push_str(part);
          }
        }
      }
      Some("function_call") => {
        if let Some(name) = step["name"].as_str() {
          on(Kind::Tool, &format!("{name} "), true);
        }
        match &step["arguments"] {
          Value::String(args) if !args.is_empty() => {
            self.args.push_str(args);
            on(Kind::Tool, args, false);
          }
          Value::Object(args) if !args.is_empty() => {
            on(Kind::Tool, &step["arguments"].to_string(), false);
          }
          _ => {}
        }
      }
      _ => {}
    }
  }

  fn delta(&mut self, delta: &Value, on: &mut impl FnMut(Kind, &str, bool)) {
    let value = |key| delta[key].as_str().unwrap_or_default();
    match value("type") {
      "text" => {
        let text = value("text");
        if !text.is_empty() {
          on(Kind::Acorn, text, self.text.is_empty());
          self.text.push_str(text);
        }
      }
      "thought_summary" => {
        if let Some(text) = delta["content"]["text"].as_str()
          && !text.is_empty()
        {
          on(Kind::Mind, text, self.summary.is_empty());
          self.summary.push_str(text);
        }
      }
      "thought_signature" => {
        if delta["signature"].is_string() {
          self.step["signature"].clone_from(&delta["signature"]);
        }
      }
      "arguments_delta" => {
        let args = value("arguments");
        if !args.is_empty() {
          self.args.push_str(args);
          on(Kind::Tool, args, false);
        }
      }
      _ => {}
    }
  }

  fn finish(mut self) -> Result<Value, String> {
    match self.step["type"].as_str() {
      Some("function_call") if !self.args.is_empty() => {
        self.step["arguments"] =
          serde_json::from_str(&self.args).map_err(|_| "Gemini sent malformed function arguments".to_string())?;
      }
      Some("model_output") if !self.text.is_empty() => self.step["content"] = json!([text(&self.text)]),
      Some("thought") if !self.summary.is_empty() => self.step["summary"] = json!([text(&self.summary)]),
      _ => {}
    }
    Ok(self.step)
  }
}

#[derive(Default)]
pub struct Got {
  steps: BTreeMap<u64, Building>,
  terminal: Value,
  pub usage: Option<Usage>,
}

impl Got {
  pub fn begun(ev: &Value) -> bool {
    ev["event_type"] == "step.start"
  }

  pub fn read(&mut self, ev: &Value, on: &mut impl FnMut(Kind, &str, bool)) -> bool {
    match ev["event_type"].as_str() {
      Some("interaction.completed" | "error") => {
        self.terminal = ev.clone();
        self.usage = ev["interaction"]["usage"]
          .as_object()
          .map(|_| usage(&ev["interaction"]["usage"]));
        true
      }
      Some(kind) => {
        if let Some(index) = ev["index"].as_u64() {
          self.read_step(index, kind, ev, on);
        }
        false
      }
      None => false,
    }
  }

  fn read_step(&mut self, index: u64, kind: &str, ev: &Value, on: &mut impl FnMut(Kind, &str, bool)) {
    match kind {
      "step.start" => {
        let mut building = Building {
          step: ev["step"].clone(),
          ..Building::default()
        };
        building.start(on);
        self.steps.insert(index, building);
      }
      "step.delta" => {
        if let Some(step) = self.steps.get_mut(&index) {
          step.delta(&ev["delta"], on);
        }
      }
      "step.stop" => {
        if let Some(step) = self.steps.get_mut(&index) {
          step.closed = true;
        }
      }
      _ => {}
    }
  }

  pub fn step(self) -> Step {
    let (halt, why) = self.outcome();
    if !matches!(halt, Halt::Done | Halt::Tools) {
      return stopped(halt, why);
    }
    if self.steps.values().any(|s| !s.closed) {
      return stopped(Halt::Busy, "the Gemini stream ended mid-step".into());
    }
    let out = match self
      .steps
      .into_values()
      .map(Building::finish)
      .collect::<Result<Vec<_>, _>>()
    {
      Ok(out) => out,
      Err(why) => return stopped(Halt::Fail, why),
    };
    let mut uses = Vec::new();
    let mut entries = Vec::new();
    for item in &out {
      match item["type"].as_str() {
        Some("model_output") => entries.extend(output_text(item).map(|s| entry(Kind::Acorn, s))),
        Some("thought") => entries.extend(thought_text(item).map(|s| entry(Kind::Mind, s))),
        Some("function_call") => {
          let Some(call) = function(item) else {
            return stopped(Halt::Fail, "Gemini sent an invalid function call".into());
          };
          entries.push(Entry {
            kind: Kind::Tool,
            text: format!("{} {}", call.name, call.input),
            of: Some(call.id.clone()),
          });
          uses.push(call);
        }
        _ => {}
      }
    }
    if (halt == Halt::Tools) != !uses.is_empty() {
      return stopped(Halt::Fail, "Gemini returned a mismatched function-call status".into());
    }
    if halt == Halt::Done
      && !entries
        .iter()
        .any(|e| e.kind == Kind::Acorn && !e.text.trim().is_empty())
    {
      return stopped(Halt::Fail, "Gemini returned no answer".into());
    }
    Step {
      out,
      uses,
      entries,
      halt,
      why,
    }
  }

  fn outcome(&self) -> (Halt, String) {
    let interaction = &self.terminal["interaction"];
    let error = self.terminal.get("error").unwrap_or(&interaction["errors"][0]);
    let code = error["code"].as_str().unwrap_or_default();
    let status = interaction["status"].as_str().unwrap_or("failed");
    if status == "incomplete" {
      let usage = &interaction["usage"];
      let used_input = usage["total_input_tokens"]
        .as_u64()
        .unwrap_or(0)
        .saturating_add(usage["total_tool_use_tokens"].as_u64().unwrap_or(0));
      if used_input > 0 && CONTEXT_TOKENS.saturating_sub(used_input) < LEAST_OUTPUT_TOKENS {
        return (
          Halt::Fail,
          "Gemini context window left too little room for output".into(),
        );
      }
    }
    let halt = match status {
      "completed" => Halt::Done,
      "requires_action" => Halt::Tools,
      _ if matches!(
        code,
        "api_error"
          | "internal"
          | "internal_server_error"
          | "service_unavailable"
          | "unavailable"
          | "overloaded"
          | "resource_exhausted"
          | "rate_limit_exceeded"
          | "too_many_requests"
          | "gateway_timeout"
          | "deadline_exceeded"
          | "aborted"
      ) =>
      {
        Halt::Busy
      }
      _ if matches!(
        code,
        "unauthenticated"
          | "authentication"
          | "invalid_api_key"
          | "permission_denied"
          | "forbidden"
          | "payment_required"
          | "quota_exceeded"
          | "billing_not_active"
      ) =>
      {
        Halt::Down
      }
      _ => Halt::Fail,
    };
    let why = if let Some(msg) = error["message"].as_str().filter(|s| !s.is_empty()) {
      msg.to_string()
    } else if !code.is_empty() {
      format!("Gemini interaction {status} ({code})")
    } else {
      format!("Gemini interaction {status}")
    };
    (halt, why)
  }
}

fn function(item: &Value) -> Option<Use> {
  Some(Use {
    id: item["id"].as_str()?.to_string(),
    name: item["name"].as_str()?.to_string(),
    input: Value::Object(item["arguments"].as_object()?.clone()),
  })
}

fn entry(kind: Kind, text: &str) -> Entry {
  Entry {
    kind,
    text: text.to_string(),
    of: None,
  }
}

fn stopped(halt: Halt, why: String) -> Step {
  Step {
    out: vec![],
    uses: vec![],
    entries: vec![],
    halt,
    why,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn step_start_payloads_stream_and_survive_follow_up_deltas() {
    let mut got = Got::default();
    let mut deltas = Vec::new();
    let mut record = |kind: Kind, text: &str, fresh: bool| {
      deltas.push((kind, text.to_string(), fresh));
    };
    for ev in [
      json!({"event_type": "step.start", "index": 0, "step": {
        "type": "thought",
        "summary": [{"type": "text", "text": "Plan: "}],
      }}),
      json!({"event_type": "step.delta", "index": 0, "delta": {
        "type": "thought_summary",
        "content": {"type": "text", "text": "check files."},
      }}),
      json!({"event_type": "step.delta", "index": 0, "delta": {
        "type": "thought_signature",
        "signature": "sig-1",
      }}),
      json!({"event_type": "step.stop", "index": 0}),
      json!({"event_type": "step.start", "index": 1, "step": {
        "type": "model_output",
        "content": [{"type": "text", "text": "Hello "}],
      }}),
      json!({"event_type": "step.delta", "index": 1, "delta": {
        "type": "text",
        "text": "world!",
      }}),
      json!({"event_type": "step.stop", "index": 1}),
      json!({"event_type": "interaction.completed", "interaction": {"status": "completed"}}),
    ] {
      got.read(&ev, &mut record);
    }
    assert_eq!(
      deltas,
      vec![
        (Kind::Mind, "Plan: ".into(), true),
        (Kind::Mind, "check files.".into(), false),
        (Kind::Acorn, "Hello ".into(), true),
        (Kind::Acorn, "world!".into(), false),
      ]
    );
    let step = got.step();
    assert_eq!(step.halt, Halt::Done);
    assert_eq!(step.entries.len(), 2);
    assert_eq!(step.entries[0].text, "Plan: check files.");
    assert_eq!(step.entries[1].text, "Hello world!");
    assert_eq!(step.out[0]["summary"][0]["text"], "Plan: check files.");
    assert_eq!(step.out[0]["signature"], "sig-1");
    assert_eq!(step.out[1]["content"][0]["text"], "Hello world!");
  }

  #[test]
  fn step_start_complete_function_call_streams_arguments() {
    let mut got = Got::default();
    let mut deltas = Vec::new();
    let mut record = |kind: Kind, text: &str, fresh: bool| {
      deltas.push((kind, text.to_string(), fresh));
    };
    for ev in [
      json!({"event_type": "step.start", "index": 0, "step": {
        "type": "function_call",
        "id": "c1",
        "name": "search",
        "arguments": {"query": "acorn"},
      }}),
      json!({"event_type": "step.stop", "index": 0}),
      json!({"event_type": "interaction.completed", "interaction": {"status": "requires_action"}}),
    ] {
      got.read(&ev, &mut record);
    }
    assert_eq!(
      deltas,
      vec![
        (Kind::Tool, "search ".into(), true),
        (Kind::Tool, "{\"query\":\"acorn\"}".into(), false),
      ]
    );
    let step = got.step();
    assert_eq!(step.halt, Halt::Tools);
    assert_eq!(step.uses.len(), 1);
    assert_eq!(step.uses[0].input, json!({"query": "acorn"}));
  }

  #[test]
  fn sse_errors_and_clamped_incomplete_windows_classify_accurately() {
    let classify = |ev: Value| {
      let mut got = Got::default();
      got.read(&ev, &mut |_, _, _| {});
      got.step()
    };
    for code in ["api_error", "too_many_requests", "aborted"] {
      let step = classify(json!({"event_type": "error", "error": {"code": code, "message": ""}}));
      assert_eq!(step.halt, Halt::Busy, "{code}");
      assert!(step.why.contains(code), "{}", step.why);
    }
    for code in ["unauthenticated", "permission_denied", "quota_exceeded"] {
      let step = classify(json!({"event_type": "error", "error": {"code": code, "message": "denied"}}));
      assert_eq!(step.halt, Halt::Down, "{code}");
    }
    let clamped = classify(json!({
      "event_type": "interaction.completed",
      "interaction": {
        "status": "incomplete",
        "usage": {"total_input_tokens": 1_030_000, "total_output_tokens": 18_576},
      }
    }));
    assert_eq!(clamped.halt, Halt::Fail);
    assert!(clamped.why.contains("context window"), "{}", clamped.why);
  }
}
