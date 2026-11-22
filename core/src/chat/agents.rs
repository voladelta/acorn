//! Agents
//! ======
//! An agent (given one task by `delegate`) has a name and a log; each
//! call gets its context and its whole log, so a told agent goes on even
//! after a quit; its replies reach the chat as one "[Name]" message.
//! Every caller's tools (`Kit`) run here, the assistant's and the agents'.

use super::{Chat, Engine};
use crate::ai::{self, Handle, Out, Spec, Toolbox};
use crate::effort::Effort;
use crate::tools::{self, MAIN, SUB_NOTE};
use crate::wire::{Event, Live};
use crate::{Block, Kind, Served, clip, config, memory};
use futures_util::FutureExt as _;
use futures_util::future::BoxFuture;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use tokio_util::sync::CancellationToken;

/// How many of a thread's latest lines the user's chat sees each turn.
const THREAD_LINES: usize = 8;

/// A thread's lines named, and who speaks in them: its user is the
/// assistant who asked, never the user.
fn thread_head(asks: &str) -> String {
  format!("## With {asks} (there, user is {asks}, who asked, and unii is you)")
}

/// An agent: its effort (none: the run's), what it does, what streams
/// (and its end kept back, `Chat::tail`), its tool calls awaiting their
/// results, the view its calls get, its replies so far.
pub struct Agent {
  pub name: String,
  effort: Option<Effort>,
  doing: Doing,
  pub(super) live: Vec<Live>,
  pub(super) tail: String,
  pub(super) calls: Vec<(String, String)>,
  view: View,
  replies: Vec<String>,
}

/// The chat's view as an agent's calls get it, and how many secrets were
/// purged as it was read: a call veils those purged since (`Kit::purged`).
#[derive(Clone, Default)]
struct View {
  blocks: Vec<Block>,
  since: usize,
}

/// What an agent does. Told while idle, it starts (its message in its
/// log, its call waiting for the chat's view), then calls, each call
/// after the one before under one lock, till it reports and is idle
/// again. Starting and calling, it is at work: listed, kept for a
/// restart, and fired by `fire` or a quit.
enum Doing {
  Idle,
  Starting,
  /// `again`: a tell reached the log after this call closed its inbox.
  Calling {
    call: Handle,
    again: bool,
  },
}

impl Agent {
  fn new(name: &str, effort: Option<Effort>, view: View) -> Agent {
    Agent {
      name: name.into(),
      effort,
      doing: Doing::Idle,
      live: vec![],
      tail: String::new(),
      calls: vec![],
      view,
      replies: vec![],
    }
  }
}

/// A number, or one said as a string ("8"), as models sometimes give it.
fn number(v: &Value) -> Option<u64> {
  v.as_u64().or_else(|| v.as_str()?.trim().parse().ok())
}

/// An effort asked for that there is none of.
fn no_effort(given: &Value) -> Out {
  let given = given.as_str().map_or_else(|| given.to_string(), str::to_string);
  Out::fail(format!(
    "No effort {given}; one of {}.",
    crate::effort::names().join(", ")
  ))
}

/// An agent's report as logged: `[Name]` on its own line, its replies,
/// and a line naming its whole chat (the model reads who sent it).
pub fn report_text(name: &str, text: &str) -> String {
  format!("[{name}]\n{text}\n\nFull chat: zoom(\"{name}\")")
}

/// A caller's tools: Acorn's (`who` MAIN) or an agent's.
pub struct Kit {
  engine: Weak<Engine>,
  /// Its assistant's work dir.
  work: PathBuf,
  who: String,
  /// Its chat is a thread (`hub`): no tools of the user's.
  thread: bool,
  /// What stops the turn the call serves (an agent's view waits on it).
  halt: CancellationToken,
  /// The bridge whose assistant's chat with the user this is: its tools.
  bridge: Option<Arc<dyn crate::bridge::Bridge>>,
  /// How many secrets were purged as its call's prompt was read.
  since: usize,
}

impl Toolbox for Kit {
  fn specs(&self) -> Vec<Spec> {
    let mut specs = tools::list(&self.who, self.thread);
    specs.extend(self.bridge.iter().flat_map(|b| b.specs()));
    specs
  }

  /// Each as given, and as a fact's subject or predicate would hold it.
  fn purged(&self) -> Vec<String> {
    let Some(engine) = self.engine.upgrade() else {
      return Vec::new();
    };
    let chat = engine.lock();
    let since = chat.secrets.iter().skip(self.since);
    since.flat_map(|s| [s.clone(), crate::facts::key(s)]).collect()
  }

  fn call(&self, name: String, input: Value, cancel: CancellationToken) -> BoxFuture<'static, Out> {
    let (engine, cwd, halt) = (self.engine.clone(), self.work.join(&self.who), self.halt.clone());
    if let Some(bridge) = self
      .bridge
      .as_ref()
      .filter(|b| b.specs().iter().any(|s| s.name == name))
    {
      return bridge.call(name, input, cancel);
    }
    let specs = (name == "abilities").then(|| self.specs());
    let (who, thread) = (self.who.clone(), self.thread);
    async move {
      let Some(engine) = engine.upgrade() else {
        return Out::fail("Acorn quit.");
      };
      if let Some(specs) = specs {
        let me = engine.lock().me.clone();
        let about = me.brief().map_or_else(String::new, |a| format!(" · {a}"));
        let part = if who != MAIN {
          format!("{who}, an agent {} delegated one task to", me.name)
        } else if thread {
          format!(
            "{}{about}, answering another assistant in your thread with them",
            me.name
          )
        } else {
          format!(
            "{}{about}, one of the user's assistants, in your chat with the user",
            me.name
          )
        };
        let project = input["dir"].as_str().unwrap_or_default().to_string();
        return Out::ok(crate::extend::abilities(
          &config::get().dir,
          &part,
          &specs,
          &cwd,
          &project,
        ));
      }
      engine.tool(cwd, &name, input, cancel, halt).await
    }
    .boxed()
  }
}

/// The biggest file `send_file` sends (Telegram's most for a bot).
const SEND_BYTES: u64 = 50 << 20;

impl Engine {
  async fn tool(
    self: Arc<Self>,
    cwd: PathBuf,
    name: &str,
    input: Value,
    cancel: CancellationToken,
    halt: CancellationToken,
  ) -> Out {
    let num = |k: &str, def: u64| number(&input[k]).unwrap_or(def);
    let str = |k: &str| input[k].as_str().unwrap_or_default().trim().to_string();
    match name {
      "zoom" => {
        if let Some(lines) = input["id"].as_str().and_then(|name| self.thread_lines(name)) {
          self.lock().mem.look("zoom", 1);
          return Out::ok(clip::page(&lines, num("at", 0) as usize));
        }
        let mut chat = self.lock();
        let (n, at) = (num("n", 1), num("at", 0) as usize);
        match &input["id"] {
          Value::String(id) if id.starts_with(|c: char| c.is_ascii_alphabetic()) => {
            let log = chat.mem.agent_show(id);
            chat.mem.look("zoom", usize::from(!log.starts_with("No agent ")));
            Out::ok(clip::page(&log, at))
          }
          id => {
            let id = number(id).unwrap_or(u64::MAX);
            let imgs = if n == 1 && at == 0 { chat.mem.images(id) } else { vec![] };
            Out {
              imgs,
              ..Out::ok(chat.mem.zoom(id, n, at))
            }
          }
        }
      }
      "date" => Out::ok(self.lock().mem.date(num("id", u64::MAX))),
      "search" => Out::ok(
        self
          .lock()
          .mem
          .search(memory::CHAT, &str("query"), num("n", 10) as usize),
      ),
      "list_skills" => {
        Out::ok(crate::skills::list_for(&crate::skills::roots(&cwd, &str("dir")), &str("task"), &cancel).await)
      }
      "load_skill" => {
        crate::skills::load(&crate::skills::roots(&cwd, &str("dir")), &str("name")).map_or_else(Out::fail, Out::ok)
      }
      "delegate" => {
        let effort = match &input["effort"] {
          Value::Null => None,
          e => match e.as_str().and_then(Effort::parse) {
            Some(effort) => Some(effort),
            None => return no_effort(e),
          },
        };
        let tasks: Vec<String> = input["tasks"]
          .as_array()
          .into_iter()
          .flatten()
          .map(|t| t.as_str().map_or_else(|| t.to_string(), str::to_string))
          .collect();
        Out::ok(self.delegate(tasks, effort, &halt).await)
      }
      "tell" => Out::ok(self.tell_agent(&str("name"), &str("message")).await),
      "fire" => {
        let mut chat = self.lock();
        let name = Some(str("name")).filter(|n| !n.is_empty());
        let at = chat
          .working()
          .into_iter()
          .filter(|w| name.as_ref().is_none_or(|n| n == w))
          .collect::<Vec<_>>();
        chat.stop_agents(name.as_deref());
        chat.change();
        match (at.is_empty(), name) {
          (true, Some(name)) => Out::fail(format!("No agent {name} at work.")),
          (true, None) => Out::ok("No agent was at work."),
          _ => Out::ok(format!("Fired {}.", at.join(", "))),
        }
      }
      "goal" => {
        let goal = Some(str("goal")).filter(|g| !g.is_empty());
        let said = match &goal {
          Some(goal) => {
            format!("Goal set: {goal}. A check reads the chat as each turn would end, and you go on till it's met.")
          }
          None => "Goal dropped.".into(),
        };
        self.lock().set_goal(goal);
        Out::ok(said)
      }
      "send_file" => {
        let path = cwd.join(str("path"));
        match tokio::fs::metadata(&path).await {
          Ok(m) if !m.is_file() => return Out::fail(format!("{} is not a file.", path.display())),
          Ok(m) if m.len() > SEND_BYTES => {
            return Out::fail(format!("{} is over 50 MB, the most sent.", path.display()));
          }
          Ok(_) => {}
          Err(err) => return Out::fail(format!("{}: {err}", path.display())),
        }
        let bytes = match tokio::fs::read(&path).await {
          Ok(bytes) => bytes,
          Err(err) => return Out::fail(format!("{}: {err}", path.display())),
        };
        let dir = self.lock().mem.dir.clone();
        let name = match tokio::task::spawn_blocking(move || memory::media_add(&dir, &bytes)).await {
          Ok(Ok(name)) => name,
          Ok(Err(err)) => return Out::fail(format!("Not sent: {err}")),
          Err(err) => return Out::fail(format!("Not sent: {err}")),
        };
        let msg = memory::Msg {
          images: vec![name],
          ..memory::Msg::new(Kind::Acorn, str("caption"))
        };
        self.lock().hold(move |chat| {
          chat.push_msg(msg);
        });
        Out::ok(format!("Sent {}: the user sees it in the chat.", path.display()))
      }
      "usage" => Out::ok(super::usage::told(num("days", 7).clamp(1, 366) as i64)),
      "set_timer" | "list_timers" | "cancel_timer" => {
        let chat = self.lock().id.clone();
        crate::timers::tool(&chat, name, &input).map_or_else(Out::fail, Out::ok)
      }
      "import" => self.import_tool(&input).await,
      "export" => self
        .export()
        .await
        .map_or_else(Out::fail, |file| Out::ok(format!("Saved as {}.", file.display()))),
      "tree" => self.tree().await.map_or_else(Out::fail, |file| {
        Out::ok(format!("Opened in the browser: {}.", file.display()))
      }),
      "list_assistants" => Out::ok(self.lock().peers(&str("query"))),
      "ask_assistant" => self.ask(&str("name"), &str("message")),
      "remember" => {
        let (me, chat, msg) = {
          let chat = self.lock();
          (chat.me.id.clone(), chat.id.clone(), chat.mem.count.saturating_sub(1))
        };
        let fact = crate::facts::Fact {
          mine: str("for") == "me",
          subject: str("subject"),
          predicate: str("predicate"),
          value: str("value"),
          many: input["many"].as_bool().unwrap_or(false),
          chat,
          msg,
        };
        // SQLite, and a vacuum of its file, off the async workers
        match tokio::task::spawn_blocking(move || crate::facts::remember(&me, fact)).await {
          Ok(Ok(said)) => Out::ok(said),
          Ok(Err(err)) => Out::fail(format!("Not kept: {err}")),
          Err(err) => Out::fail(format!("Not kept: {err}")),
        }
      }
      "forget" => {
        let me = self.lock().me.id.clone();
        let Some(id) = number(&input["id"]) else {
          return Out::fail("Give id: the fact's number.");
        };
        match tokio::task::spawn_blocking(move || crate::facts::forget(&me, id)).await {
          Ok(Ok(said)) => Out::ok(said),
          Ok(Err(err)) => Out::fail(format!("Not forgotten: {err}")),
          Err(err) => Out::fail(format!("Not forgotten: {err}")),
        }
      }
      "purge" | "unpurge" => {
        let (mut chat, back) = (self.lock(), name == "unpurge");
        if let (false, Some(text)) = (back, input["text"].as_str()) {
          return Out::ok(chat.purge_text(text));
        }
        match &input["id"] {
          Value::String(name) if name.starts_with(|c: char| c.is_ascii_alphabetic()) => match input["n"].as_u64() {
            Some(n) => Out::ok(chat.purge(Some(name), n, back)),
            None => Out::fail("Give n: the index of the message in the agent's chat."),
          },
          id => Out::ok(chat.purge(None, number(id).unwrap_or(u64::MAX), back)),
        }
      }
      "extend" => crate::extend::extend(
        &config::get().dir,
        &str("kind"),
        &str("name"),
        input["text"].as_str().unwrap_or_default(),
      )
      .map_or_else(Out::fail, Out::ok),
      "create_assistant" => {
        let Some(effort) = Effort::parse(&str("effort")) else {
          return no_effort(&input["effort"]);
        };
        match crate::assistant::add(
          &str("name"),
          &str("about"),
          &str("instructions"),
          &str("welcome"),
          effort,
        ) {
          Ok(made) => self.listed(
            format!("Created {}, {}; they are in the sidebar.", made.name, made.about),
            made,
          ),
          Err(why) => Out::fail(why),
        }
      }
      "archive_assistant" => {
        let me = self.lock().me.id.clone();
        match crate::assistant::archive(&str("name"), &me) {
          Ok(gone) => {
            super::hub::close(&gone.id);
            self.lock().all(Event::Gone(gone.id.clone()));
            Out::ok(format!(
              "Archived {}: off the sidebar, their chat and files kept; restore_assistant brings them back.",
              gone.name
            ))
          }
          Err(why) => Out::fail(why),
        }
      }
      "restore_assistant" => match crate::assistant::restore(&str("name")) {
        Ok(back) => self.listed(
          format!(
            "Restored {}, {}; they are in the sidebar, their chat as it was.",
            back.name, back.about
          ),
          back,
        ),
        Err(why) => Out::fail(why),
      },
      "luau" => crate::luau::run(&input, cwd, cancel).await,
      _ => Out::fail(format!("No tool {name}.")),
    }
  }

  /// An assistant made or restored: its chat opened, and the app told
  /// to list it.
  fn listed(&self, said: String, it: crate::assistant::Assistant) -> Out {
    let _ = super::hub::chat(&it);
    self.lock().all(Event::Assistant(it));
    Out::ok(said)
  }

  async fn delegate(self: &Arc<Self>, tasks: Vec<String>, effort: Option<Effort>, halt: &CancellationToken) -> String {
    if tasks.is_empty() {
      return "No tasks.".into();
    }
    let view = match self.view(halt).await {
      Ok(view) => view,
      Err(why) => return why,
    };
    let names: Vec<String> = tasks
      .iter()
      .map(|task| self.lock().start(effort, task, view.clone()))
      .collect();
    format!(
      "Delegated to {}. Each one's report will reach you as a message when it finishes.",
      names.join(", ")
    )
  }

  /// A message to an agent, logged: one calling stops what it does and
  /// reads it; one starting reads it as it starts; an idle one starts,
  /// going on from its chat.
  pub(super) async fn tell_agent(self: &Arc<Self>, name: &str, message: &str) -> String {
    let halt = {
      let mut chat = self.lock();
      let Some(agent) = chat.agent(name) else {
        return format!("No agent {name} to tell.");
      };
      let reply = match &mut agent.doing {
        Doing::Calling { call, .. } if call.stopped() => {
          Some(format!("{name} is stopping; your message stays in its log."))
        }
        Doing::Calling { call, again } => {
          *again = !call.send(crate::Say {
            text: message.into(),
            cut: true,
            ..Default::default()
          });
          Some(format!("Sent. {name}'s report will answer your message."))
        }
        Doing::Starting => Some(format!(
          "Sent. {name} reads it as it starts; its reply will reach you as a message."
        )),
        Doing::Idle => {
          agent.doing = Doing::Starting;
          agent.replies.clear();
          None
        }
      };
      chat.push(Some(name), Kind::User, message);
      if let Some(reply) = reply {
        return reply;
      }
      chat.change();
      chat.halt.clone()
    };
    let view = match self.view(&halt).await {
      Ok(view) => view,
      Err(why) => return why,
    };
    let mut chat = self.lock();
    let Some(agent) = chat.agents.get_mut(name) else {
      return format!("No agent {name} to tell.");
    };
    match agent.doing {
      Doing::Idle => return format!("{name} was fired before it began; your message stays in its log."),
      // another tell started it meanwhile, its call reading this message
      Doing::Calling { .. } => {}
      Doing::Starting => {
        agent.view = view;
        if let Some(call) = chat.call_agent(name) {
          tokio::spawn(self.clone().run(name.to_string(), call));
        }
      }
    }
    format!("Sent. {name}'s reply will reach you as a message.")
  }

  /// Another assistant asked, in their thread (`hub`): its reply comes
  /// back as a message.
  fn ask(self: &Arc<Self>, name: &str, message: &str) -> Out {
    let me = self.lock().me.clone();
    let Some(them) = crate::assistant::find(name) else {
      return Out::fail(format!("No assistant {name}; list_assistants names them."));
    };
    if them.id == me.id {
      return Out::fail("That's you.");
    }
    if message.is_empty() {
      return Out::fail("Ask something: message is empty.");
    }
    match super::hub::thread(&them, &me) {
      Ok(thread) => {
        thread.command(crate::wire::Command::Say {
          text: message.into(),
          images: vec![],
          via: None,
        });
        Out::ok(format!(
          "Asked {}. Their reply will reach you as a message \"[{}]\".",
          them.name, them.name
        ))
      }
      Err(why) => Out::fail(why),
    }
  }

  /// Your threads, where other assistants asked you, as the user's chat
  /// sees them: each one's latest lines, `zoom` giving one whole. None in
  /// a thread, or with none. Read before the chat is locked: a thread
  /// locks its asker's chat to answer, so the two never wait on each
  /// other.
  pub(super) fn threads(&self) -> Option<Block> {
    if self.id.contains('/') {
      return None;
    }
    let mine = format!("{}/", self.id);
    let said: Vec<String> = super::hub::all()
      .into_iter()
      .filter(|e| e.id.starts_with(&mine))
      .filter_map(|e| {
        let chat = e.lock();
        let latest = chat.mem.view_lines(THREAD_LINES);
        let head = thread_head(&chat.with.as_ref()?.name);
        (!latest.is_empty()).then(|| format!("{head}\n{}\n", latest.join("\n")))
      })
      .collect();
    (!said.is_empty()).then(|| {
            Block::new(format!("Your threads, where other assistants asked you for the user (each one's latest lines; its ids are the thread's, not this chat's; zoom(\"Name\") gives one whole):\n{}\n", said.join("\n")), false)
        })
  }

  /// Your thread with the assistant named, whole (any case); None: no
  /// such assistant, or no thread with them.
  fn thread_lines(&self, name: &str) -> Option<String> {
    let asks = crate::assistant::find(name)?;
    let thread = super::hub::get(&format!("{}/{}", self.id, asks.id))?;
    let lines = thread.lock().mem.view_lines(usize::MAX);
    Some(format!("{}\n{}", thread_head(&asks.name), lines.join("\n")))
  }

  /// The chat's view and whole tail; else why there is
  /// none (work stopped).
  async fn view(&self, halt: &CancellationToken) -> Result<View, String> {
    let from = {
      let chat = self.lock();
      chat.mem.whole(chat.mem.first(0, 0))
    };
    if !self.settle(from, halt).await {
      return Err("Stopped.".into());
    }
    let chat = self.lock();
    let latest = super::turns::latest(&chat.mem, from, &[]);
    let mut blocks = chat.mem.view_blocks(from);
    if !latest.text.is_empty() {
      blocks.push(Block::new(latest.text, true));
    }
    Ok(View {
      blocks,
      since: chat.secrets.len(),
    })
  }

  /// An agent's calls, `call` first, while a message waits in its log
  /// (told too late for the call before); then its report.
  async fn run(self: Arc<Self>, name: String, mut call: ai::Call) {
    loop {
      let end = call.end().await;
      let mut chat = self.lock();
      let Some(agent) = chat.agents.get_mut(&name) else {
        return;
      };
      let Doing::Calling { call: handle, again } = &agent.doing else {
        return;
      };
      let fired = handle.stopped();
      agent.replies.push(if end.ok {
        end.text
      } else {
        format!("failed: {}", end.text)
      });
      if fired || (!again && end.left.is_empty()) {
        agent.doing = Doing::Idle;
        let text = report_text(&name, &agent.replies.join("\n\n"));
        chat.report(&name, text, fired);
        chat.change();
        return;
      }
      // the next call starts under the lock this one ended in, so the
      // agent is never seen idle between them
      match chat.call_agent(&name) {
        Some(next) => call = next,
        None => return,
      }
    }
  }
}

impl Chat {
  /// A caller's tools: Acorn's (None), or an agent's.
  pub(super) fn kit(&self, agent: Option<&Agent>) -> Arc<dyn Toolbox> {
    Arc::new(Kit {
      engine: self.this.clone(),
      work: self.me.work(),
      who: agent.map_or(MAIN, |a| a.name.as_str()).to_string(),
      thread: self.with.is_some(),
      halt: self.halt.clone(),
      bridge: (agent.is_none() && self.with.is_none())
        .then(|| crate::bridge::of(&self.me.id))
        .flatten(),
      since: agent.map_or(self.secrets.len(), |a| a.view.since),
    })
  }

  /// An agent started on its task, its log opened with what it is.
  fn start(&mut self, effort: Option<Effort>, task: &str, view: View) -> String {
    let name = match self.mem.agent_new(effort) {
      Ok(name) => name,
      Err(err) => return format!("(no agent: {err})"),
    };
    let _ = std::fs::create_dir_all(self.me.work().join(&name));
    self.agents.insert(name.clone(), Agent::new(&name, effort, view));
    self.push(Some(&name), Kind::Note, SUB_NOTE);
    self.push(Some(&name), Kind::User, task);
    if let Some(call) = self.call_agent(&name) {
      tokio::spawn(self.engine().run(name.clone(), call));
    }
    name
  }

  /// An agent's next call, started: it is at work, calling.
  fn call_agent(&mut self, name: &str) -> Option<ai::Call> {
    let ask = self.agent_ask(name)?;
    let call = ai::ask(ask, self.on_events(Some(name.to_string())));
    self.agents.get_mut(name)?.doing = Doing::Calling {
      call: call.handle.clone(),
      again: false,
    };
    self.change();
    Some(call)
  }

  /// An agent; one not met since Acorn started is taken back from its
  /// row, idle (one from before efforts were kept: at the run's).
  fn agent(&mut self, name: &str) -> Option<&mut Agent> {
    if !self.agents.contains_key(name) {
      self.mem.agent(name)?;
      let effort = self.mem.agent_effort(name);
      self
        .agents
        .insert(name.into(), Agent::new(name, effort, View::default()));
    }
    self.agents.get_mut(name)
  }

  /// A call of an agent, on the config's `agents` or else its assistant's,
  /// at its effort if it has one.
  fn agent_ask(&self, name: &str) -> Option<ai::Ask> {
    let agent = self.agents.get(name)?;
    let log = Block::new(self.mem.agent_reads(name), true);
    let pick = config::get().agents.unwrap_or(self.me.effort);
    let system = tools::system(&(tools::sub(&self.me.work()) + &crate::facts::prompt(&self.me.id)));
    let effort = agent.effort.unwrap_or(pick);
    let mut blocks = agent.view.blocks.clone();
    blocks.push(log);
    Some(ai::Ask {
      effort,
      system,
      blocks,
      tools: Some(self.kit(Some(agent))),
      meter: Some(self.meter(Served::Agent)),
      ..self.share()
    })
  }

  /// The agents at work.
  pub(super) fn working(&self) -> Vec<String> {
    self
      .agents
      .values()
      .filter(|a| !matches!(a.doing, Doing::Idle))
      .map(|a| a.name.clone())
      .collect()
  }

  /// The agents at work now, at the start of each of Acorn's calls'
  /// messages: read from their calls, so the list can't drift from the
  /// truth.
  pub(super) fn at_work(&self) -> Block {
    let names = self.working();
    let list = if names.is_empty() {
      "none.".into()
    } else {
      format!("{}.", names.join(", "))
    };
    Block::new(format!("Agents at work now: {list}\n\n"), false)
  }

  /// Every agent at work (None), or the one named: one starting goes
  /// back to idle, one calling is stopped (its report, as it ends, left
  /// unanswered).
  pub(super) fn stop_agents(&mut self, name: Option<&str>) {
    for agent in self.agents.values_mut().filter(|a| name.is_none_or(|n| n == a.name)) {
      match &mut agent.doing {
        Doing::Idle => {}
        Doing::Starting => agent.doing = Doing::Idle,
        Doing::Calling { call, .. } => call.stop(),
      }
    }
  }

  /// The user's other assistants, each its name and about, then the
  /// archived; with a query, those it matches (each word in its name,
  /// about or instructions), best first.
  fn peers(&self, query: &str) -> String {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut them: Vec<(usize, crate::assistant::Assistant)> = crate::assistant::all()
      .into_iter()
      .filter(|a| a.id != self.me.id)
      .map(|a| {
        let hay = format!("{} {} {}", a.name, a.about, a.instructions).to_lowercase();
        (words.iter().filter(|w| hay.contains(w.as_str())).count(), a)
      })
      .filter(|(hits, _)| words.is_empty() || *hits > 0)
      .collect();
    them.sort_by_key(|(hits, _)| std::cmp::Reverse(*hits));
    let shown = |a: &crate::assistant::Assistant| {
      if a.about.is_empty() {
        a.name.clone()
      } else {
        format!("{}: {}", a.name, a.about)
      }
    };
    let mut said = if !them.is_empty() {
      them.iter().map(|(_, a)| shown(a)).collect::<Vec<_>>().join("\n")
    } else if words.is_empty() {
      "No other assistants yet.".to_string()
    } else {
      return "None matches.".into();
    };
    let archived: Vec<String> = crate::assistant::archived().iter().map(shown).collect();
    if words.is_empty() && !archived.is_empty() {
      said += "\n\nArchived (restore_assistant brings one back):\n";
      said += &archived.join("\n");
    }
    said
  }

  /// An agent's page: its log, and what streams.
  pub(super) fn agent_page(&self, name: &str) {
    let log = self.mem.agent(name).map(|log| {
      log
        .into_iter()
        .map(|r| crate::wire::Entry {
          kind: r.kind,
          text: r.text,
          date: r.date,
          images: r.images,
          spend: None,
          via: None,
        })
        .collect()
    });
    self.all(Event::Agent { name: name.into(), log });
    let live = self.agents.get(name).map(|a| a.live.clone()).unwrap_or_default();
    self.all(Event::Live {
      agent: Some(name.into()),
      live,
    });
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn handoff_restarts_an_agent_told_after_its_call_finished() {
    let dir = tempfile::tempdir().unwrap();
    let engine = crate::chat::tests::open(dir.path());
    let call = ai::Call::ended("old report");
    while !call.done.is_finished() {
      tokio::task::yield_now().await;
    }
    let name = {
      let mut chat = engine.lock();
      let name = chat.mem.agent_new(None).unwrap();
      let mut agent = Agent::new(&name, None, View::default());
      agent.doing = Doing::Calling {
        call: call.handle.clone(),
        again: false,
      };
      chat.agents.insert(name.clone(), agent);
      name
    };
    engine.tell_agent(&name, "check the revised input").await;
    let run = engine.clone().run(name.clone(), call);
    // Drive the completion through handoff on this single-thread runtime.
    // The next request cannot run until we yield.
    assert!(futures_util::poll!(Box::pin(run)).is_pending());
    let chat = engine.lock();
    assert_eq!(chat.working(), std::slice::from_ref(&name));
    let ask = chat.agent_ask(&name).unwrap();
    assert!(ask.blocks.last().unwrap().text.contains("check the revised input"));
  }

  #[tokio::test]
  async fn a_starting_view_takes_unbuilt_messages_whole() {
    let dir = tempfile::tempdir().unwrap();
    let engine = crate::chat::tests::open(dir.path());
    let halt = CancellationToken::new();
    assert!(engine.view(&halt).await.is_ok());
    let mut png = Vec::new();
    image::RgbImage::new(1, 1)
      .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
      .unwrap();
    let image = memory::media_add(dir.path(), &png).unwrap();
    engine
      .lock()
      .mem
      .push(vec![memory::Msg {
        images: vec![image.clone()],
        ..memory::Msg::new(Kind::User, "current turn")
      }])
      .unwrap();
    let view = tokio::time::timeout(std::time::Duration::from_secs(1), engine.view(&halt))
      .await
      .unwrap()
      .unwrap();
    assert!(view.blocks.last().unwrap().text.contains("0+0|"));
    assert!(view.blocks.last().unwrap().text.contains("current turn"));
    assert!(
      view
        .blocks
        .last()
        .unwrap()
        .text
        .contains(&format!("[image docs/images/{image}]"))
    );
    {
      let mut chat = engine.lock();
      chat.agents.insert("Test".into(), Agent::new("Test", None, view));
      assert!(chat.agent_ask("Test").unwrap().imgs.is_empty());
    }
    halt.cancel();
    assert!(engine.view(&halt).await.is_err());
  }

  /// A call veils in what it sends the texts purged since its prompt was
  /// read (`Kit::purged`), each as given and as a fact's key holds it: one
  /// purged again too, but none purged before, so a message taken back
  /// (unpurge) is read by the calls after. An agent's veils those purged
  /// since its view was read.
  #[tokio::test]
  async fn a_call_veils_what_was_purged_since_its_prompt_was_read() {
    let test = "chat::agents::tests::a_call_veils_what_was_purged_since_its_prompt_was_read";
    if std::env::var_os("ACORN_TEST_VEILED").is_none() {
      let dir = tempfile::tempdir().unwrap();
      let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env("ACORN_TEST_VEILED", "1")
        .env("ACORN_DIR", dir.path())
        .env("ACORN_CHATS", dir.path().join("chats"))
        .output()
        .unwrap();
      assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
      );
      return;
    }
    let dir = tempfile::tempdir().unwrap();
    let engine = super::super::tests::open(dir.path());
    let before = engine.lock().kit(None);
    assert!(before.purged().is_empty());
    let said = engine.lock().purge_text("Hunter Two 4242");
    assert!(said.contains("this call too"), "{said}");
    assert_eq!(before.purged(), ["Hunter Two 4242", "hunter two 4242"]);
    // a call built now reads what the purge left: it has nothing to veil
    let after = engine.lock().kit(None);
    assert!(after.purged().is_empty());
    let view = engine.view(&CancellationToken::new()).await.unwrap();
    engine.lock().purge_text("hunter-2-4242");
    engine.lock().purge_text("Hunter Two 4242");
    // purged again, a secret is not told again: each call veils it already
    let since = ["hunter-2-4242", "hunter-2-4242"];
    assert_eq!(after.purged(), since);
    assert_eq!(before.purged().len(), 4);
    let mut chat = engine.lock();
    let name = chat.mem.agent_new(None).unwrap();
    chat.agents.insert(name.clone(), Agent::new(&name, None, view));
    let kit = chat.kit(chat.agents.get(&name));
    drop(chat);
    assert_eq!(kit.purged(), since, "though its call is built after them");
  }
}
