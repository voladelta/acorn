//! Agents
//! ======
//! An agent (hired for one task) has a name and a log; each
//! call gets its context and its whole log, so a told agent goes on even
//! after a quit; its replies reach the chat as one "[Name]" message.
//! Every caller's tools (`Kit`) run here, the assistant's and the agents'.

use super::{Chat, Engine};
use crate::ai::{self, Handle, Out, Spec, Toolbox};
use crate::effort::Effort;
use crate::tools::{self, MAIN, SUB_AT, SUB_NOTE};
use crate::wire::{Event, Live};
use crate::{Block, Kind, Served, clip, config, machine};
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

/// An agent: its effort (none: the run's), its call at work, what
/// streams, its tool calls awaiting their results, whether its call was
/// stopped (its report stays unanswered), the view its calls get, its
/// replies so far.
pub struct Agent {
  pub name: String,
  effort: Option<Effort>,
  call: Option<Handle>,
  pub(super) live: Vec<Live>,
  pub(super) calls: Vec<(String, String)>,
  stop: bool,
  view: Vec<Block>,
  replies: Vec<String>,
}

impl Agent {
  fn new(name: &str, effort: Option<Effort>, view: Vec<Block>) -> Agent {
    Agent {
      name: name.into(),
      effort,
      call: None,
      live: vec![],
      calls: vec![],
      stop: false,
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
}

impl Toolbox for Kit {
  fn specs(&self) -> Vec<Spec> {
    tools::list(&self.work, &self.who, self.thread)
  }

  fn call(&self, name: String, input: Value, cancel: CancellationToken) -> BoxFuture<'static, Out> {
    let (engine, cwd, halt) = (self.engine.clone(), self.work.join(&self.who), self.halt.clone());
    async move {
      let Some(engine) = engine.upgrade() else {
        return Out::fail("Acorn quit.");
      };
      engine.tool(cwd, &name, input, cancel, halt).await
    }
    .boxed()
  }
}

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
          return Out::ok(clip::page(&lines, num("at", 0) as usize));
        }
        let mut chat = self.lock();
        let (n, at) = (num("n", 1), num("at", 0) as usize);
        match &input["id"] {
          Value::String(id) if id.starts_with(|c: char| c.is_ascii_alphabetic()) => {
            Out::ok(clip::page(&chat.mem.agent_show(id), at))
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
      "list_skills" => Out::ok(crate::skills::list(&crate::skills::roots(&cwd, &str("dir")))),
      "load_skill" => {
        crate::skills::load(&crate::skills::roots(&cwd, &str("dir")), &str("name")).map_or_else(Out::fail, Out::ok)
      }
      "hire" => {
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
        Out::ok(self.hire(tasks, effort, &halt).await)
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
      "usage" => Out::ok(super::usage::told(num("days", 7).clamp(1, 366) as i64)),
      "import" => self.import_tool(&input).await,
      "export" => self
        .export()
        .await
        .map_or_else(Out::fail, |file| Out::ok(format!("Saved as {}.", file.display()))),
      "tree" => self.tree().await.map_or_else(Out::fail, |file| {
        Out::ok(format!("Opened in the browser: {}.", file.display()))
      }),
      "list_assistants" => Out::ok(self.lock().peers(None)),
      "search_assistants" => Out::ok(self.lock().peers(Some(&str("query")))),
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
        crate::facts::remember(&me, fact).map_or_else(|err| Out::fail(format!("Not kept: {err}")), Out::ok)
      }
      "forget" => {
        let me = self.lock().me.id.clone();
        let Some(id) = number(&input["id"]) else {
          return Out::fail("Give id: the fact's number.");
        };
        crate::facts::forget(&me, id).map_or_else(|err| Out::fail(format!("Not forgotten: {err}")), Out::ok)
      }
      "purge" => {
        let mut chat = self.lock();
        match &input["id"] {
          Value::String(name) if name.starts_with(|c: char| c.is_ascii_alphabetic()) => match input["n"].as_u64() {
            Some(n) => Out::ok(chat.purge(Some(name), n)),
            None => Out::fail("Give n: the index of the message in the agent's chat."),
          },
          id => Out::ok(chat.purge(None, number(id).unwrap_or(u64::MAX))),
        }
      }
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
      "web_search" | "web_fetch" => crate::web::run(name, &input, cancel).await,
      _ => machine::run(name, &input, cwd, cancel).await,
    }
  }

  /// An assistant made or restored: its chat opened, and the app told
  /// to list it.
  fn listed(&self, said: String, it: crate::assistant::Assistant) -> Out {
    let _ = super::hub::chat(&it);
    self.lock().all(Event::Assistant(it));
    Out::ok(said)
  }

  async fn hire(self: &Arc<Self>, tasks: Vec<String>, effort: Option<Effort>, halt: &CancellationToken) -> String {
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
      "Hired {}. Each one's report will reach you as a message when it finishes.",
      names.join(", ")
    )
  }

  /// A message to an agent: one at work stops what it does and reads it;
  /// an idle one goes on from its chat.
  pub(super) async fn tell_agent(self: &Arc<Self>, name: &str, message: &str) -> String {
    {
      let mut chat = self.lock();
      let Some(agent) = chat.agent(name) else {
        return format!("No agent {name} to tell.");
      };
      if let Some(call) = agent.call.clone() {
        chat.push(Some(name), Kind::User, message);
        call.send(crate::Say {
          text: message.into(),
          imgs: vec![],
          cut: true,
        });
        return format!("Sent. {name} stops what it was doing to read it, and its report answers it.");
      }
    }
    let halt = self.lock().halt.clone();
    let view = match self.view(&halt).await {
      Ok(view) => view,
      Err(why) => return why,
    };
    let mut chat = self.lock();
    chat.push(Some(name), Kind::User, message);
    if let Some(agent) = chat.agents.get_mut(name) {
      agent.view = view;
      agent.replies.clear();
      agent.stop = false;
    }
    drop(chat);
    tokio::spawn(self.clone().run(name.to_string()));
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

  /// The chat's view, once its summaries are built; else why there is
  /// none.
  async fn view(&self, halt: &CancellationToken) -> Result<Vec<Block>, String> {
    let count = self.lock().mem.count;
    if let Some(why) = self.settle(count, halt).await {
      return Err(why);
    }
    let chat = self.lock();
    Ok(chat.mem.view_blocks(chat.mem.count))
  }

  /// An agent's calls while a message waits in its log (told too late for
  /// the call before); then its report.
  async fn run(self: Arc<Self>, name: String) {
    loop {
      let call = {
        let mut chat = self.lock();
        let Some(ask) = chat.agent_ask(&name) else { return };
        let call = ai::ask(ask, chat.on_events(Some(name.clone())));
        if let Some(agent) = chat.agents.get_mut(&name) {
          agent.call = Some(call.handle.clone());
        }
        chat.change();
        call
      };
      let end = call.end().await;
      let mut chat = self.lock();
      let Some(agent) = chat.agents.get_mut(&name) else {
        return;
      };
      agent.call = None;
      agent.replies.push(if end.ok {
        end.text
      } else {
        format!("failed: {}", end.text)
      });
      if !end.ok || end.left.is_empty() {
        let (text, stopped) = (report_text(&name, &agent.replies.join("\n\n")), agent.stop);
        chat.report(&name, text, stopped);
        chat.change();
        return;
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
    })
  }

  /// An agent started on its task, its log opened with what it is.
  fn start(&mut self, effort: Option<Effort>, task: &str, view: Vec<Block>) -> String {
    let name = match self.mem.agent_new() {
      Ok(name) => name,
      Err(err) => return format!("(no agent: {err})"),
    };
    let _ = std::fs::create_dir_all(self.me.work().join(&name));
    self.agents.insert(name.clone(), Agent::new(&name, effort, view));
    let note = match effort {
      None => SUB_NOTE.to_string(),
      Some(effort) => format!("{SUB_NOTE}{SUB_AT}{}", effort.as_str()),
    };
    self.push(Some(&name), Kind::Note, &note);
    self.push(Some(&name), Kind::User, task);
    tokio::spawn(self.engine().run(name.clone()));
    name
  }

  /// An agent; one not met since Acorn started is taken back from its
  /// log, idle.
  fn agent(&mut self, name: &str) -> Option<&mut Agent> {
    if !self.agents.contains_key(name) {
      let note = self
        .mem
        .agent(name)?
        .first()
        .filter(|r| r.kind == Kind::Note)?
        .text
        .clone();
      let effort = if note == SUB_NOTE {
        None
      } else {
        Effort::parse(note.strip_prefix(&format!("{SUB_NOTE}{SUB_AT}"))?)
      };
      self.agents.insert(name.into(), Agent::new(name, effort, Vec::new()));
    }
    self.agents.get_mut(name)
  }

  /// A call of an agent, on the config's `agents` or else its assistant's,
  /// at its effort if it has one.
  fn agent_ask(&self, name: &str) -> Option<ai::Ask> {
    let agent = self.agents.get(name)?;
    let log = Block::new(self.mem.agent_show(name), true);
    let pick = config::get().agents.unwrap_or(self.me.effort);
    let system = tools::system(&(tools::sub(&self.me.work()) + &crate::facts::prompt(&self.me.id)));
    let effort = agent.effort.unwrap_or(pick);
    let mut blocks = agent.view.clone();
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
      .filter(|a| a.call.is_some())
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

  /// Every agent at work (None), or the one named.
  pub(super) fn stop_agents(&mut self, name: Option<&str>) {
    for agent in self
      .agents
      .values_mut()
      .filter(|a| a.call.is_some() && name.is_none_or(|n| n == a.name))
    {
      agent.stop = true;
      if let Some(call) = &agent.call {
        call.stop();
      }
    }
  }

  /// The user's other assistants, each its name and about; those a query
  /// matches (each word in its name, about or instructions), best first.
  fn peers(&self, query: Option<&str>) -> String {
    let words: Vec<String> = query
      .unwrap_or_default()
      .split_whitespace()
      .map(str::to_lowercase)
      .collect();
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
