//! Call
//! ====
//! A tool call as people read it. Logged as its tool's name, a space and
//! its input's JSON (`zoom {"id":6243,"n":1}`), it shows as its emoji
//! (`look::tool_glyph`), its name in bold and a head, as its tool reads: a
//! command as itself; a file tool its path, read's lines after it; edit's
//! files (the diff comes in its result); zoom and date the message
//! (`#6243`, a line `#6243+8`, an agent by name); hire its tasks, tell
//! and computer their name and text in blocks. Any other call shows its
//! name and short values as arguments, over a block for each other value,
//! keyed by its name unless alone. A call still streaming reads as its
//! input so far: its open string and brackets closed, else up to its last
//! whole field.
//!
//! An agent's report is logged as `[Name]` on its own line, its replies,
//! and a line naming its whole chat; it shows as its replies alone, framed
//! and named by its agent.

use serde_json::{Map, Value};

const SHORT: usize = 60;

/// A block of a call: its key ("" if none), its text.
#[derive(Clone, Debug, PartialEq)]
pub struct Code {
    pub k: String,
    pub x: String,
}

/// name: the bare tool name ("": no call), shown bold; head: the rest of
/// its line.
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub name: String,
    pub head: String,
    pub blocks: Vec<Code>,
}

/// An agent's report, read: its agent, its replies.
pub fn report_read(text: &str) -> Option<(String, String)> {
    let rest = text.strip_prefix('[')?;
    let close = rest.find(']')?;
    let name = &rest[..close];
    if name.is_empty() || name.contains('\n') {
        return None;
    }
    let mut body = rest[close + 1..].trim();
    if let Some(k) = body.rfind("Full chat: zoom(\"")
        && body[k..].trim_end().ends_with("\")")
        && !body[k..].contains('\n')
    {
        body = body[..k].trim_end();
    }
    Some((name.to_string(), body.to_string()))
}

pub fn parse(text: &str) -> Call {
    let plain = || Call { name: String::new(), head: text.to_string(), blocks: vec![] };
    let Some((name, json)) = text.split_once(' ').filter(|(n, j)| !n.is_empty() && !n.contains(char::is_whitespace) && j.starts_with('{')) else {
        return plain();
    };
    let Some(input) = input_parse(json) else { return plain() };
    let name = name.strip_prefix("mcp__").and_then(|n| n.split_once("__").map(|(_, n)| n)).unwrap_or(name);
    let (head, blocks) = shown(name, input);
    Call { name: name.to_string(), head, blocks }
}

/// The call on one line, after its name: its head, then each block,
/// spaces folded.
pub fn line(c: &Call) -> String {
    let blocks = c.blocks.iter().map(|b| if b.k.is_empty() { b.x.clone() } else { format!("{}: {}", b.k, b.x) });
    std::iter::once(c.head.clone()).chain(blocks).collect::<Vec<_>>().join(" ").split_whitespace().collect::<Vec<_>>().join(" ")
}

fn shown(name: &str, x: Map<String, Value>) -> (String, Vec<Code>) {
    let str = |k: &str| x.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    match name {
        "shell" => (str("command"), vec![]),
        "read" => (format!("{}{}", str("path"), lines_show(x.get("offset"), x.get("limit"))), vec![]),
        "write" => (str("path"), vec![]),
        "edit" if x.get("edits").is_some_and(Value::is_array) => {
            let mut files: Vec<String> = Vec::new();
            for e in x["edits"].as_array().unwrap() {
                if let Some(p) = e["path"].as_str()
                    && !files.iter().any(|f| f == p)
                {
                    files.push(p.into());
                }
            }
            (files.join(", "), vec![])
        }
        "zoom" => {
            let at = x.get("at").map_or(String::new(), |a| format!(" from char {a}"));
            (format!("{}{at}", msg_show(x.get("id"), x.get("n"))), vec![])
        }
        "date" => (msg_show(x.get("id"), None), vec![]),
        "web_search" => (str("query"), vec![]),
        "web_fetch" => (str("url"), vec![]),
        "hire" | "spawn" => {
            let tasks: Vec<&Value> = x.get("tasks").and_then(Value::as_array).map(|t| t.iter().collect()).unwrap_or_default();
            let n = tasks.len();
            let effort = x.get("effort").and_then(Value::as_str).map_or(String::new(), |e| format!(" · {e}"));
            let head = format!("{n} {}{effort}", if n == 1 { "agent" } else { "agents" });
            (head, tasks.into_iter().flat_map(text_block).collect())
        }
        "tell" => (str("name"), x.get("message").map(text_block).unwrap_or_default()),
        "computer" if x.contains_key("task") => (String::new(), x.get("task").map(text_block).unwrap_or_default()),
        _ => input_shown(x),
    }
}

/// Any tool's input: its short values as arguments, the rest in blocks.
fn input_shown(input: Map<String, Value>) -> (String, Vec<Code>) {
    let alone = input.len() <= 1;
    let (mut args, mut blocks) = (Vec::new(), Vec::new());
    for (k, v) in input {
        let json = v.to_string();
        let word = v.as_str().is_none_or(|s| !s.contains(|c: char| c.is_whitespace() || c == '"' || c == '\\'));
        if json.chars().count() <= SHORT && word {
            args.push(json);
        } else {
            let x = v.as_str().map_or_else(|| serde_json::to_string_pretty(&v).unwrap_or_default(), str::to_string);
            blocks.push(Code { k: if alone { String::new() } else { k }, x });
        }
    }
    (args.join(" "), blocks)
}

fn text_block(x: &Value) -> Vec<Code> {
    match x.as_str() {
        Some(s) if !s.is_empty() => vec![Code { k: String::new(), x: s.strip_suffix('\n').unwrap_or(s).to_string() }],
        _ => vec![],
    }
}

/// The message or line zoom and date name: `#6243`, `#6243+8` (n > 1), an
/// agent's name as is.
fn msg_show(id: Option<&Value>, n: Option<&Value>) -> String {
    let many = n.and_then(Value::as_u64).filter(|&n| n > 1).map_or(String::new(), |n| format!("+{n}"));
    match id {
        Some(Value::Number(id)) => format!("#{id}{many}"),
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

/// A read's lines, as path:4-7 shows them ("": the whole file).
fn lines_show(offset: Option<&Value>, limit: Option<&Value>) -> String {
    if offset.is_none() && limit.is_none() {
        return String::new();
    }
    let from = offset.and_then(Value::as_i64).unwrap_or(1);
    let to = limit.and_then(Value::as_i64).map_or(String::new(), |l| (from + l - 1).to_string());
    format!(":{from}-{to}")
}

/// None: no JSON object, nor a prefix of one.
fn input_parse(text: &str) -> Option<Map<String, Value>> {
    let (mut shut, mut quote, mut esc, mut field) = (Vec::new(), false, false, 0);
    for (i, c) in text.char_indices() {
        if quote {
            quote = esc || c != '"';
            esc = !esc && c == '\\';
        } else if c == '"' {
            quote = true;
        } else if c == '{' || c == '[' {
            shut.push(if c == '{' { '}' } else { ']' });
            if shut.len() == 1 {
                field = i + 1;
            }
        } else if c == '}' || c == ']' {
            shut.pop();
        } else if c == ',' && shut.len() == 1 {
            field = i;
        }
    }
    let open = if quote { format!("{}\"", if esc { &text[..text.len() - 1] } else { text }) } else { text.to_string() };
    let full = open.trim_end_matches(|c: char| c.is_whitespace() || c == ',').to_string() + &shut.iter().rev().collect::<String>();
    object(&full).or_else(|| object(&format!("{}}}", &text[..field])))
}

fn object(text: &str) -> Option<Map<String, Value>> {
    match serde_json::from_str(text) {
        Ok(Value::Object(map)) => Some(map),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_read_as_their_tools() {
        let c = parse(r#"zoom {"id":6243,"n":8}"#);
        assert_eq!((c.name.as_str(), c.head.as_str()), ("zoom", "#6243+8"));
        let c = parse(r#"shell {"command":"ls -la"}"#);
        assert_eq!(c.head, "ls -la");
        let c = parse(r#"shell {"command":"cargo te"#);
        assert_eq!((c.name.as_str(), c.head.as_str()), ("shell", "cargo te"));
        let c = parse(r#"lookup {"q":"bend","deep":{"a":1}}"#);
        assert_eq!(c.head, "\"bend\" {\"a\":1}");
        let c = parse(r#"web_search {"query":"acorn site:github.com","count":3}"#);
        assert_eq!(c.head, "acorn site:github.com");
        assert_eq!(parse("no call").name, "");
        assert_eq!(report_read("[RedAnt]\ndone\n\nFull chat: zoom(\"RedAnt\")"), Some(("RedAnt".into(), "done".into())));
    }
}
