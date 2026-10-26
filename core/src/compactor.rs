//! Compactor
//! =========
//! Builds the memory tree's nodes (`memory`) as messages come. A node whose
//! source fits is its source, built at once; else a compaction writes it:
//! a call like a turn, on the config's compactor, with the same system
//! prompt, no tools, and the compactions' own view up to the node, then
//! its task. A line too long is asked anew, TRIES lines at most, the
//! shortest kept. The call asks till it is answered (a patient one: an
//! outage only delays a node, never harms it); answered with no line (a
//! refusal: its input's doing), its node is its source clipped. Of each
//! side, JOBS build at once, in the order the messages came.

use crate::ai::{self, Stop};
use crate::chat::Chat;
use crate::memory::{self, NODE, Part};
use crate::{Block, Served, config};
use std::sync::Arc;

const TRIES: usize = 5;

/// The nodes to build: those whose source fits built at once, together;
/// the rest started.
pub fn pump(chat: &mut Chat) {
    let mut built = false;
    loop {
        if chat.mem.done {
            break;
        }
        let (free, jobs) = chat.mem.starts();
        for (part, text) in jobs {
            job(chat, part, text);
        }
        if free.is_empty() {
            break;
        }
        if let Err(err) = chat.mem.store(&free) {
            eprintln!("compactor: {err}");
            break;
        }
        built = true;
    }
    if built {
        chat.mem.fit(false);
    }
}

/// A node built by its calls; cut (its side held, or Acorn quitting), it
/// builds as it goes on.
fn job(chat: &mut Chat, part: Part, text: String) {
    let this = chat.this();
    tokio::spawn(async move {
        let built = build(&this, part, text).await;
        let Some(engine) = this.upgrade() else { return };
        let mut chat = engine.lock();
        if let Err(why) = built {
            eprintln!("compactor: node {}/{}: {why}", part.l, part.i);
        }
        chat.mem.busy.remove(&part.slot());
        chat.pump();
        chat.mem.fit(false);
    });
}

/// Node (l,i)'s summary: its view to the node and its task, asked till a
/// line fits. Each try sends those blocks, never a try's words or
/// thoughts; one after a line too long adds only its size and where the
/// limit cuts it.
async fn build(this: &std::sync::Weak<crate::chat::Engine>, part: Part, text: String) -> Result<(), String> {
    let (ask, served) = {
        let Some(engine) = this.upgrade() else { return Err("Acorn quit".into()) };
        let mut chat = engine.lock();
        let side = chat.mem.side(part);
        let upto = part.ctx().min(chat.mem.first(0, side));
        let mut blocks = chat.mem.blocks(upto, &chat.mem.seen.parts.clone());
        blocks.push(Block::new(format!("{}{text}\n</input>", memory::Memory::task(part)), false));
        let served = chat.mem.served(part);
        let yields = (served == Served::Import).then(|| chat.mem.dir.display().to_string());
        let pick = config::get().compactor;
        let ask = ai::Ask { effort: pick.effort, blocks, meter: Some(chat.meter(served)), patient: true, yields, ..chat.share() };
        (ask, served)
    };
    let mut lines: Vec<String> = Vec::new();
    let mut end = compact(this, part, served, ask.clone()).await?;
    while end.ok && !end.text.trim().is_empty() {
        let line = end.text.trim().to_string();
        lines.push(line.clone());
        let done = this.upgrade().is_none_or(|e| e.lock().mem.done);
        if line.len() <= NODE || lines.len() == TRIES || done {
            break;
        }
        let mut again = ask.clone();
        again.blocks.push(Block::new(memory::over(&line), false));
        end = compact(this, part, served, again).await?;
    }
    if lines.is_empty() && !end.ok && end.stop != Some(Stop::Fail) {
        return Err(end.text);
    }
    if lines.is_empty() {
        let why = if end.ok { "an empty line" } else { end.text.as_str() };
        eprintln!("compactor: node {}/{}: {why}; clipped", part.l, part.i);
    }
    let line = match lines.into_iter().min_by_key(String::len) {
        Some(line) => line,
        None => memory::cut(&memory::flat(&text)).to_string(),
    };
    let engine = this.upgrade().ok_or("Acorn quit")?;
    engine.lock().mem.store(&[(part, line)]).map_err(|e| e.to_string())
}

/// A summary's try: its call's end, the call kept (to cut it) till then.
async fn compact(this: &std::sync::Weak<crate::chat::Engine>, part: Part, served: Served, ask: ai::Ask) -> Result<ai::End, String> {
    let call = {
        let engine = this.upgrade().ok_or("Acorn quit")?;
        let mut chat = engine.lock();
        let call = ai::ask(ask, Arc::new(|_| {}));
        chat.compactions().push((call.handle.clone(), part, served));
        call
    };
    let handle = call.handle.clone();
    let end = call.end().await;
    if let Some(engine) = this.upgrade() {
        engine.lock().compactions().retain(|(h, _, _)| !h.same(&handle));
    }
    Ok(end)
}
