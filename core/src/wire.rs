//! Wire
//! ====
//! What the engine and the app say to each other. The app sends a
//! `Command` (a person's act, or a read of the chat); the engine answers
//! with `Event`s, the chat as the agent sees it: the view's parts as
//! blocks, from the turn on each message whole, then what streams. Each
//! message and summary is told once (`Event::Know`), before any part that
//! shows it.

use crate::{Kind, Spend};

/// The n messages from id on (n a power of two).
pub type Seg = (u64, u64);

/// A message logged: its kind, text, date (ISO), images, and, the last of
/// a turn, what the turn's requests used.
#[derive(Clone, Debug)]
pub struct Entry {
    pub kind: Kind,
    pub text: String,
    pub date: String,
    pub images: Vec<String>,
    pub spend: Option<Spend>,
}

/// A summary: its text, its last message's date.
#[derive(Clone, Debug)]
pub struct Node {
    pub text: String,
    pub to: String,
}

/// A piece of a message streaming.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Live {
    pub kind: Kind,
    pub text: String,
}

/// A piece joins the message streaming, if of its kind, unless it starts
/// one (fresh: a tool call, as two calls never join).
pub fn live_push(live: &mut Vec<Live>, kind: Kind, text: &str, fresh: bool) {
    match live.last_mut() {
        Some(top) if top.kind == kind && !fresh => top.text.push_str(text),
        _ => live.push(Live { kind, text: text.to_string() }),
    }
}

/// An import's progress: its time done and in all (micro-USD of calls),
/// USD spent and left, why paused, its messages, its nodes not built by
/// level.
#[derive(Clone, Debug, PartialEq)]
pub struct Progress {
    pub done: f64,
    pub total: f64,
    pub spent: f64,
    pub left: f64,
    pub why: Option<String>,
    pub msgs: u64,
    pub open: Vec<u64>,
}

/// What a turn waits on while nothing streams.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wait {
    None,
    /// The summaries of the messages before its own.
    Summaries,
    /// The model's first piece (a request out, no tool running).
    Model,
}

/// The chat's state: a turn runs; what it waits on; the agents at work;
/// the goal it works to; an import's progress; what every call used today.
#[derive(Clone, Debug, PartialEq)]
pub struct State {
    pub busy: bool,
    pub wait: Wait,
    pub agents: Vec<String>,
    pub goal: Option<String>,
    pub importing: Option<Progress>,
    pub today: Option<Spend>,
}

#[derive(Clone, Debug)]
pub enum Event {
    /// Messages and summaries a view will show.
    Know { whole: Vec<(u64, Entry)>, nodes: Vec<(Seg, Node)> },
    /// The view's parts, and where the turn starts (each message from it
    /// on whole).
    View { parts: Vec<Seg>, turn: u64 },
    /// The halves of a part opened.
    Zoom { seg: Seg, parts: Vec<Seg> },
    /// The tree's roots: the chat, folded.
    Root { parts: Vec<Seg> },
    /// A message logged: Acorn's (`id`), an agent's, or a thought (none).
    Entry { agent: Option<String>, id: Option<u64>, entry: Entry },
    Delta { agent: Option<String>, kind: Kind, text: String, fresh: bool },
    /// What streams, told anew.
    Live { agent: Option<String>, live: Vec<Live> },
    State(State),
    /// An agent's log, asked for (None: no such agent).
    Agent { name: String, log: Option<Vec<Entry>> },
    Note(String),
    /// What a turn used, on its last message.
    Use { id: u64, spend: Spend },
    /// An assistant made (`create_assistant`), to list and start.
    Assistant(crate::assistant::Assistant),
}

/// What the app sends: the person's words, and looks. Every other act is
/// said, and an assistant does it by a tool (`tools`).
#[derive(Clone, Debug)]
pub enum Command {
    /// A message; one sent while the assistant works cuts in.
    Say { text: String, images: Vec<Vec<u8>> },
    /// The app (again) shows this chat: tell it all anew.
    Attach,
    /// Open a part into its halves.
    Zoom(Seg),
    /// The tree's roots.
    Root,
    /// An agent's log.
    Agent(String),
}

/// The turn, as the work a quit cut names it among the agents.
pub const TURN: &str = "your turn";
