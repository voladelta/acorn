//! Agents
//! ======
//! An agent (a subagent, or a computer task) has a name and a log; each
//! call gets its context and its whole log, so a told agent goes on even
//! after a quit; its replies reach the chat as one "[Name]" message.
//! Every caller's tools (`Kit`) run here, Acorn's and the agents'.

use super::{Chat, Engine};
use crate::ai::{self, Handle, Out, Spec, Toolbox};
use crate::pick::Effort;
use crate::tools::{self, HANDS, HANDS_NOTE, MAIN, SUB_AT, SUB_NOTE};
use crate::wire::{Event, Live};
use crate::{Block, Kind, Served, clip, computer, config, machine};
use futures_util::FutureExt as _;
use futures_util::future::BoxFuture;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use tokio_util::sync::CancellationToken;

/// What an agent is: a subagent (at an effort of its own, or the run's),
/// or the hands of a computer task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Sub(Option<Effort>),
    Hands,
}

/// An agent: its call at work, what streams, its tool calls awaiting
/// their results, whether its call was stopped (its report stays
/// unanswered), the view its calls get, its replies so far.
pub struct Agent {
    pub name: String,
    role: Role,
    call: Option<Handle>,
    pub(super) live: Vec<Live>,
    pub(super) calls: Vec<(String, String)>,
    stop: bool,
    view: Vec<Block>,
    replies: Vec<String>,
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
    hands: bool,
    /// What stops the turn the call serves (an agent's view waits on it).
    halt: CancellationToken,
}

impl Toolbox for Kit {
    fn specs(&self) -> Vec<Spec> {
        tools::list(&self.work, &self.who, self.hands)
    }

    fn call(&self, name: String, input: Value, cancel: CancellationToken) -> BoxFuture<'static, Out> {
        let (engine, cwd, hands, halt) = (self.engine.clone(), self.work.join(&self.who), self.hands, self.halt.clone());
        async move {
            let Some(engine) = engine.upgrade() else { return Out::fail("Acorn quit.") };
            engine.tool(cwd, hands, &name, input, cancel, halt).await
        }
        .boxed()
    }
}

impl Engine {
    async fn tool(self: Arc<Self>, cwd: PathBuf, hands: bool, name: &str, input: Value, cancel: CancellationToken, halt: CancellationToken) -> Out {
        let num = |k: &str, def: u64| input[k].as_u64().unwrap_or(def);
        match name {
            "zoom" => {
                let mut chat = self.lock();
                let (n, at) = (num("n", 1), num("at", 0) as usize);
                match &input["id"] {
                    Value::String(id) if id.starts_with(|c: char| c.is_ascii_alphabetic()) => Out::ok(clip::page(&chat.mem.agent_show(id), at)),
                    id => {
                        let id = id.as_u64().or_else(|| id.as_str().and_then(|s| s.parse().ok())).unwrap_or(u64::MAX);
                        let imgs = if n == 1 && at == 0 { chat.mem.images(id) } else { vec![] };
                        Out { imgs, ..Out::ok(chat.mem.zoom(id, n, at)) }
                    }
                }
            }
            "date" => Out::ok(self.lock().mem.date(num("id", u64::MAX))),
            "spawn" => {
                let effort = match &input["effort"] {
                    Value::Null => None,
                    e => match e.as_str().and_then(Effort::parse) {
                        Some(effort) => Some(effort),
                        None => {
                            let all: Vec<&str> = crate::pick::EFFORTS.iter().map(|e| e.as_str()).collect();
                            return Out::fail(format!("No effort {}; one of {}.", e.as_str().unwrap_or(&e.to_string()), all.join(", ")));
                        }
                    },
                };
                let tasks: Vec<String> = input["tasks"].as_array().into_iter().flatten().map(|t| t.as_str().map_or_else(|| t.to_string(), str::to_string)).collect();
                Out::ok(self.spawn(tasks, effort, &halt).await)
            }
            "tell" => {
                let str = |k: &str| input[k].as_str().unwrap_or_default().to_string();
                Out::ok(self.tell_agent(&str("name"), &str("message")).await)
            }
            "remember" => {
                let str = |k: &str| input[k].as_str().unwrap_or_default().to_string();
                let (me, chat, msg) = {
                    let chat = self.lock();
                    (chat.me.id.clone(), chat.id.clone(), chat.mem.count.saturating_sub(1))
                };
                let fact = crate::facts::Fact { mine: str("for") == "me", subject: str("subject"), predicate: str("predicate"), value: str("value"), many: input["many"].as_bool().unwrap_or(false), chat, msg };
                crate::facts::remember(&me, fact).map_or_else(|err| Out::fail(format!("Not kept: {err}")), Out::ok)
            }
            "forget" => {
                let me = self.lock().me.id.clone();
                let Some(id) = input["id"].as_u64() else { return Out::fail("Give id: the fact's number.") };
                crate::facts::forget(&me, id).map_or_else(|err| Out::fail(format!("Not forgotten: {err}")), Out::ok)
            }
            "purge" => {
                let mut chat = self.lock();
                match &input["id"] {
                    Value::String(name) if name.starts_with(|c: char| c.is_ascii_alphabetic()) => match input["n"].as_u64() {
                        Some(n) => Out::ok(chat.purge(Some(name), n)),
                        None => Out::fail("Give n: the index of the message in the agent's chat."),
                    },
                    id => {
                        let id = id.as_u64().or_else(|| id.as_str().and_then(|s| s.parse().ok())).unwrap_or(u64::MAX);
                        Out::ok(chat.purge(None, id))
                    }
                }
            }
            "create_assistant" => {
                let str = |k: &str| input[k].as_str().unwrap_or_default().to_string();
                let Some(effort) = Effort::parse(&str("effort")) else {
                    let all: Vec<&str> = crate::pick::EFFORTS.iter().map(|e| e.as_str()).collect();
                    return Out::fail(format!("No effort {}; one of {}.", input["effort"], all.join(", ")));
                };
                match crate::assistant::add(&str("name"), &str("instructions"), effort) {
                    Ok(made) => {
                        let said = format!("Created {}, at {} effort; it is in the sidebar.", made.name, effort.as_str());
                        self.lock().all(Event::Assistant(made));
                        Out::ok(said)
                    }
                    Err(why) => Out::fail(why),
                }
            }
            "computer" if hands => computer::act(&input, cancel).await,
            "computer" => self.computer(input["task"].as_str().unwrap_or_default()),
            _ => machine::run(name, &input, cwd, cancel).await,
        }
    }

    async fn spawn(self: &Arc<Self>, tasks: Vec<String>, effort: Option<Effort>, halt: &CancellationToken) -> String {
        if tasks.is_empty() {
            return "No tasks.".into();
        }
        let view = match self.view(halt).await {
            Ok(view) => view,
            Err(why) => return why,
        };
        let names: Vec<String> = tasks.iter().map(|task| self.lock().start(Role::Sub(effort), task, view.clone())).collect();
        format!("Started {}. Each one's report will reach you as a message when it finishes.", names.join(", "))
    }

    fn computer(self: &Arc<Self>, task: &str) -> Out {
        if !config::get().computer {
            return Out::fail("This Mac does no computer use: the user hasn't set computer to 1 in Acorn's config.");
        }
        let mut chat = self.lock();
        if let Some(why) = chat.screen() {
            return Out::fail(why);
        }
        let name = chat.start(Role::Hands, task, Vec::new());
        Out::ok(format!("Started {name}. Its report will reach you as a message."))
    }

    /// A message to an agent: one at work reads it between its tool calls;
    /// an idle one goes on from its chat.
    pub(super) async fn tell_agent(self: &Arc<Self>, name: &str, message: &str) -> String {
        let role = {
            let mut chat = self.lock();
            let Some(agent) = chat.agent(name) else { return format!("No agent {name} to tell.") };
            let (call, role) = (agent.call.clone(), agent.role);
            if let Some(call) = call {
                chat.push(Some(name), Kind::User, message);
                call.send(crate::Say { text: message.into(), imgs: vec![] });
                return format!("Sent. {name} reads it between its tool calls, and its report answers it.");
            }
            if role == Role::Hands
                && let Some(why) = chat.screen()
            {
                return why;
            }
            role
        };
        let halt = self.lock().halt.clone();
        let view = match role {
            Role::Sub(_) => match self.view(&halt).await {
                Ok(view) => view,
                Err(why) => return why,
            },
            Role::Hands => Vec::new(),
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

    /// The chat's view, once its summaries are built; else why there is
    /// none.
    async fn view(&self, halt: &CancellationToken) -> Result<Vec<Block>, String> {
        let count = self.lock().mem.count;
        match self.settle(count, halt).await {
            Some(why) => Err(why),
            None => {
                let chat = self.lock();
                Ok(chat.mem.view_blocks(chat.mem.count))
            }
        }
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
            let Some(agent) = chat.agents.get_mut(&name) else { return };
            agent.call = None;
            agent.replies.push(if end.ok { end.text } else { format!("failed: {}", end.text) });
            if !(end.ok && !end.left.is_empty()) {
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
            hands: agent.is_some_and(|a| a.role == Role::Hands),
            halt: self.halt.clone(),
        })
    }

    /// An agent started on its task, its log opened with what it is.
    fn start(&mut self, role: Role, task: &str, view: Vec<Block>) -> String {
        let name = match self.mem.agent_new() {
            Ok(name) => name,
            Err(err) => return format!("(no agent: {err})"),
        };
        let _ = std::fs::create_dir_all(self.me.work().join(&name));
        let agent = Agent { name: name.clone(), role, call: None, live: vec![], calls: vec![], stop: false, view, replies: vec![] };
        self.agents.insert(name.clone(), agent);
        let note = match role {
            Role::Sub(None) => SUB_NOTE.to_string(),
            Role::Sub(Some(effort)) => format!("{SUB_NOTE}{SUB_AT}{}", effort.as_str()),
            Role::Hands => format!("{HANDS_NOTE}{}", host()),
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
            let note = self.mem.agent(name)?.first().filter(|r| r.kind == Kind::Note)?.text.clone();
            let role = if note == SUB_NOTE {
                Role::Sub(None)
            } else if let Some(effort) = note.strip_prefix(&format!("{SUB_NOTE}{SUB_AT}")) {
                Role::Sub(Effort::parse(effort))
            } else if note.starts_with(HANDS_NOTE) {
                Role::Hands
            } else {
                return None;
            };
            let agent = Agent { name: name.into(), role, call: None, live: vec![], calls: vec![], stop: false, view: vec![], replies: vec![] };
            self.agents.insert(name.into(), agent);
        }
        self.agents.get_mut(name)
    }

    /// A call of an agent, on the config's `agents` or else the chat's pick,
    /// at its effort if it has one (a computer task's sees no view, and only
    /// its computer tool).
    fn agent_ask(&self, name: &str) -> Option<ai::Ask> {
        let agent = self.agents.get(name)?;
        let log = Block::new(self.mem.agent_show(name), true);
        let pick = config::get().agents.unwrap_or_else(|| self.pick());
        let (kind, system, effort) = match agent.role {
            Role::Sub(effort) => (Served::Agent, tools::system(&(tools::sub(&self.me.work()) + &crate::facts::prompt(&self.me.id))), effort.unwrap_or(pick.effort)),
            Role::Hands => (Served::Computer, tools::system(HANDS), pick.effort),
        };
        let mut blocks = agent.view.clone();
        blocks.push(log);
        Some(ai::Ask { effort, system, blocks, tools: Some(self.kit(Some(agent))), meter: Some(self.meter(kind)), ..self.share() })
    }

    /// Why a computer task can't start now: one works at a time.
    fn screen(&self) -> Option<String> {
        let busy = self.agents.values().find(|a| a.role == Role::Hands && a.call.is_some())?;
        Some(format!("Computer task {} is still working on this Mac; one works there at a time.", busy.name))
    }

    /// The agents at work.
    pub(super) fn working(&self) -> Vec<String> {
        self.agents.values().filter(|a| a.call.is_some()).map(|a| a.name.clone()).collect()
    }

    /// The agents at work now, at the start of each of Acorn's calls'
    /// messages: read from their calls, so the list can't drift from the
    /// truth.
    pub(super) fn at_work(&self) -> Block {
        let names = self.working();
        let list = if names.is_empty() { "none.".into() } else { format!("{}.", names.join(", ")) };
        Block::new(format!("Agents at work now: {list}\n\n"), false)
    }

    /// Every agent at work (None), or the one named.
    pub(super) fn stop_agents(&mut self, name: Option<&str>) {
        for agent in self.agents.values_mut().filter(|a| a.call.is_some() && name.is_none_or(|n| n == a.name)) {
            agent.stop = true;
            if let Some(call) = &agent.call {
                call.stop();
            }
        }
    }

    /// An agent's page: its log, and what streams.
    pub(super) fn agent_page(&mut self, name: &str) {
        let log = self.mem.agent(name).map(|log| {
            log.into_iter().map(|r| crate::wire::Entry { kind: r.kind, text: r.text, date: r.date, images: r.images, spend: None }).collect()
        });
        self.all(Event::Agent { name: name.into(), log });
        let live = self.agents.get(name).map(|a| a.live.clone()).unwrap_or_default();
        self.all(Event::Live { agent: Some(name.into()), live });
    }
}

/// This Mac's name: its local host name (Bonjour's), else the host's.
pub fn host() -> String {
    std::process::Command::new("scutil")
        .args(["--get", "LocalHostName"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "this Mac".into())
}
