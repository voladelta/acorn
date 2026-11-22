//! Markdown to Telegram
//! ====================
//! What the assistant writes (Markdown) as Telegram's HTML: bold, italic,
//! strikethrough, code, code blocks, links and quotes; headings bold,
//! bullets as •; anything else as it is, escaped. Markup that doesn't
//! close stays as written. A text over Telegram's 4096 characters goes
//! in pieces, split between paragraphs where it can, a code block cut
//! closed and opened again.

/// The most characters of a piece's Markdown: its HTML adds tags.
const PIECE: usize = 3500;

/// Markdown in pieces, each short enough for one message.
pub fn pieces(md: &str) -> Vec<String> {
  let mut out: Vec<String> = Vec::new();
  let mut cur = String::new();
  let flush = |cur: &mut String, out: &mut Vec<String>| {
    let piece = cur.trim_matches('\n').to_string();
    if !piece.trim().is_empty() {
      out.push(piece);
    }
    cur.clear();
  };
  for block in blocks(md) {
    let fits = |cur: &str, more: &str| cur.chars().count() + more.chars().count() + 2 <= PIECE;
    if !fits(&cur, &block) {
      flush(&mut cur, &mut out);
    }
    if fits("", &block) {
      if !cur.is_empty() {
        cur.push_str("\n\n");
      }
      cur.push_str(&block);
      continue;
    }
    for part in cut(&block) {
      out.push(part);
    }
  }
  flush(&mut cur, &mut out);
  out
}

/// Paragraphs, a code block each one whole.
fn blocks(md: &str) -> Vec<String> {
  let (mut all, mut cur, mut fence) = (Vec::new(), Vec::<&str>::new(), false);
  for line in md.lines() {
    if line.trim_start().starts_with("```") {
      fence = !fence;
    }
    if !fence && line.trim().is_empty() && !line.trim_start().starts_with("```") {
      if !cur.is_empty() {
        all.push(cur.join("\n"));
        cur.clear();
      }
      continue;
    }
    cur.push(line);
  }
  if !cur.is_empty() {
    all.push(cur.join("\n"));
  }
  all
}

/// A block too long for one piece, by lines (a line too long, by
/// characters); a code block's pieces each fenced.
fn cut(block: &str) -> Vec<String> {
  let code = block.trim_start().starts_with("```");
  let (open, body) = if code {
    let (first, rest) = block.split_once('\n').unwrap_or((block, ""));
    (
      first.trim().to_string(),
      rest.trim_end().trim_end_matches("```").to_string(),
    )
  } else {
    (String::new(), block.to_string())
  };
  let room = PIECE
    .saturating_sub(if code { open.chars().count() + 8 } else { 0 })
    .max(1);
  let mut parts: Vec<String> = Vec::new();
  let mut cur = String::new();
  for line in body.lines() {
    let mut line = line.to_string();
    while line.chars().count() > room {
      let at = line.char_indices().nth(room).map_or(line.len(), |(i, _)| i);
      if !cur.is_empty() {
        parts.push(std::mem::take(&mut cur));
      }
      parts.push(line[..at].to_string());
      line = line[at..].to_string();
    }
    if cur.chars().count() + line.chars().count() + 1 > room && !cur.is_empty() {
      parts.push(std::mem::take(&mut cur));
    }
    if !cur.is_empty() {
      cur.push('\n');
    }
    cur.push_str(&line);
  }
  if !cur.is_empty() {
    parts.push(cur);
  }
  if code {
    parts.into_iter().map(|p| format!("{open}\n{p}\n```")).collect()
  } else {
    parts
  }
}

/// A piece of Markdown as Telegram's HTML.
pub fn html(md: &str) -> String {
  let mut out = String::new();
  let mut lines = md.lines().peekable();
  let mut quote: Vec<String> = Vec::new();
  let end_quote = |quote: &mut Vec<String>, out: &mut String| {
    if !quote.is_empty() {
      out.push_str(&format!("<blockquote>{}</blockquote>\n", quote.join("\n")));
      quote.clear();
    }
  };
  while let Some(line) = lines.next() {
    let t = line.trim_start();
    if let Some(lang) = t.strip_prefix("```") {
      end_quote(&mut quote, &mut out);
      let mut code = Vec::new();
      for l in lines.by_ref() {
        if l.trim_start().starts_with("```") {
          break;
        }
        code.push(l);
      }
      let class = match lang.trim() {
        "" => String::new(),
        lang => format!(" class=\"language-{}\"", escape(lang).replace('"', "&quot;")),
      };
      out.push_str(&format!(
        "<pre><code{class}>{}</code></pre>\n",
        escape(&code.join("\n"))
      ));
      continue;
    }
    if let Some(q) = t.strip_prefix('>') {
      quote.push(inline(q.strip_prefix(' ').unwrap_or(q)));
      continue;
    }
    end_quote(&mut quote, &mut out);
    let hashes = t.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
      out.push_str(&format!("<b>{}</b>\n", inline(t[hashes..].trim())));
      continue;
    }
    let indent = &line[..line.len() - t.len()];
    let bullet = ["- ", "* ", "+ "].iter().find_map(|b| t.strip_prefix(b));
    match bullet {
      Some(rest) => out.push_str(&format!("{indent}• {}\n", inline(rest))),
      None => out.push_str(&format!("{}\n", inline(line))),
    }
  }
  end_quote(&mut quote, &mut out);
  out.trim_end().to_string()
}

fn escape(s: &str) -> String {
  s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// A line's inline markup: `code`, **bold**, __bold__, *italic*, _italic_,
/// ~~strike~~, [text](url).
fn inline(s: &str) -> String {
  let c: Vec<char> = s.chars().collect();
  let mut out = String::new();
  let mut i = 0;
  let text = |a: usize, b: usize| c[a..b].iter().collect::<String>();
  let find = |from: usize, pat: &[char]| (from..c.len()).find(|&k| c[k..].starts_with(pat));
  while i < c.len() {
    let rest = &c[i..];
    if rest[0] == '`'
      && let Some(end) = find(i + 1, &['`'])
    {
      out.push_str(&format!("<code>{}</code>", escape(&text(i + 1, end))));
      i = end + 1;
      continue;
    }
    let mut paired = false;
    for (mark, tag) in [("**", "b"), ("__", "b"), ("~~", "s")] {
      let m: Vec<char> = mark.chars().collect();
      if rest.starts_with(&m)
        && c.get(i + 2).is_some_and(|ch| !ch.is_whitespace())
        && let Some(end) = find(i + 2, &m).filter(|&e| e > i + 2)
      {
        out.push_str(&format!("<{tag}>{}</{tag}>", inline(&text(i + 2, end))));
        i = end + 2;
        paired = true;
        break;
      }
    }
    if paired {
      continue;
    }
    let ch = rest[0];
    let word_before = i > 0 && c[i - 1].is_alphanumeric();
    if (ch == '*' || ch == '_')
      && !word_before
      && c.get(i + 1).is_some_and(|n| !n.is_whitespace() && *n != ch)
      && let Some(end) = (i + 1..c.len())
        .find(|&k| c[k] == ch && !c[k - 1].is_whitespace() && !c.get(k + 1).is_some_and(|n| n.is_alphanumeric()))
    {
      out.push_str(&format!("<i>{}</i>", inline(&text(i + 1, end))));
      i = end + 1;
      continue;
    }
    if ch == '['
      && let Some(mid) = find(i + 1, &[']', '('])
      && let Some(end) = find(mid + 2, &[')'])
    {
      let url = text(mid + 2, end);
      if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("tg://") {
        out.push_str(&format!(
          "<a href=\"{}\">{}</a>",
          escape(&url).replace('"', "&quot;"),
          inline(&text(i + 1, mid))
        ));
        i = end + 1;
        continue;
      }
    }
    out.push_str(&escape(&ch.to_string()));
    i += 1;
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn markdown_reads_as_telegrams_html() {
    assert_eq!(
      html("**Done.** It's `x < y` & *fine*"),
      "<b>Done.</b> It's <code>x &lt; y</code> &amp; <i>fine</i>"
    );
    assert_eq!(html("# Plan\n- one\n  - two"), "<b>Plan</b>\n• one\n  • two");
    assert_eq!(
      html("see [the docs](https://x.io/a?b=1&c=2)"),
      "see <a href=\"https://x.io/a?b=1&amp;c=2\">the docs</a>"
    );
    assert_eq!(
      html("```rust\nfn a() -> u8 { 1 }\n```"),
      "<pre><code class=\"language-rust\">fn a() -&gt; u8 { 1 }</code></pre>"
    );
    assert_eq!(
      html("> said\n> twice\nback"),
      "<blockquote>said\ntwice</blockquote>\nback"
    );
    assert_eq!(html("~~old~~ new"), "<s>old</s> new");
  }

  #[test]
  fn markup_that_doesnt_close_stays_as_written() {
    assert_eq!(
      html("```a\"b&c\nx\n```"),
      "<pre><code class=\"language-a&quot;b&amp;c\">x</code></pre>"
    );
    assert_eq!(html("snake_case_name and 2*3*4"), "snake_case_name and 2*3*4");
    assert_eq!(html("a **b"), "a **b");
    assert_eq!(html("[x](javascript:alert)"), "[x](javascript:alert)");
    assert_eq!(html("<script>"), "&lt;script&gt;");
  }

  #[test]
  fn a_long_text_goes_in_pieces_between_paragraphs() {
    let para = "word ".repeat(300); // 1500 chars
    let md = [para.as_str(); 5].join("\n\n");
    let parts = pieces(&md);
    assert_eq!(parts.len(), 3);
    assert!(parts.iter().all(|p| p.chars().count() <= PIECE));
    assert_eq!(parts.concat().matches("word").count(), 1500);
  }

  #[test]
  fn a_long_code_block_is_closed_and_opened_again() {
    let code = (0..400)
      .map(|i| format!("let x{i} = {i};"))
      .collect::<Vec<_>>()
      .join("\n");
    let parts = pieces(&format!("Here:\n\n```rust\n{code}\n```"));
    assert!(parts.len() >= 3);
    assert_eq!(parts[0], "Here:");
    for p in &parts {
      assert!(p.chars().count() <= PIECE, "piece has {} characters", p.chars().count());
      assert!(html(p).chars().count() <= 4096);
    }
    for p in &parts[1..] {
      assert!(p.starts_with("```rust\n") && p.ends_with("\n```"), "{}", &p[..20]);
      assert!(html(p).starts_with("<pre><code class=\"language-rust\">"));
    }
    let rebuilt = parts[1..]
      .iter()
      .map(|p| p.strip_prefix("```rust\n").unwrap().strip_suffix("\n```").unwrap())
      .collect::<Vec<_>>()
      .join("\n");
    assert_eq!(rebuilt, code);
  }

  #[test]
  fn a_long_fence_leaves_room_for_progress() {
    let open = format!("```{}", "é".repeat(4000));
    let parts = pieces(&format!("{open}\nxy\n```"));
    assert_eq!(parts, vec![format!("{open}\nx\n```"), format!("{open}\ny\n```")]);
  }

  #[test]
  fn a_huge_line_is_cut_by_characters() {
    let parts = pieces(&"é".repeat(8000));
    assert_eq!(parts.len(), 3);
    assert_eq!(parts.iter().map(|p| p.chars().count()).sum::<usize>(), 8000);
  }
}
