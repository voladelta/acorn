//! Acorn
//! =====
//! The window around the assistants' chats (`acorn_core`): each
//! assistant's engine runs on the runtime's threads, the window on its
//! own; the person's acts go to the model (`model::Model::act`), the shown
//! engine's events too (`hear`), and both answer what to send it. The
//! page: the sidebar (`side`: the assistants), a header (the sidebar's
//! button, the assistant's name, an import's progress, the ☰ menu), the
//! chat, and a footer (today's cost, the images waiting, the message box,
//! the tabs while agents work).

mod call;
mod look;
mod model;
mod rows;
mod sheets;
mod side;
mod text;

use acorn_core::assistant::Assistant;
use acorn_core::chat::Engine;
use acorn_core::import::Found;
use acorn_core::wire::{Command, Event, Usage};
use look::*;
use model::{Act, Model, Then};
use nsh_base::input;
use nsh_core::prelude::*;
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

/// Where the window's commands go: each assistant's engine, by id.
static ENGINES: LazyLock<Mutex<HashMap<String, UnboundedSender<Command>>>> = LazyLock::new(Default::default);

/// Where the engines' events go, each by its assistant's id: the window.
static HEARD: OnceLock<UnboundedSender<(String, Event)>> = OnceLock::new();

/// The engine's threads: its calls, tools and files never hold the
/// window's.
static RUNTIME: LazyLock<tokio::runtime::Runtime> =
    LazyLock::new(|| tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("acorn").build().expect("the engine's runtime"));


/// A box over the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sheet {
    None,
    Menu,
    Usage,
    Import,
}

/// What every part of the window shares: the model (of the chat shown),
/// the assistants, the one shown, those at work, whether the sidebar
/// shows, the box over the page, the notes shown, what the usage and
/// import boxes show, the clock their times read.
#[derive(Clone, Copy)]
pub struct Ui {
    pub model: Signal<Model>,
    pub assistants: Signal<Vec<Assistant>>,
    pub shown: Signal<String>,
    pub working: Signal<HashSet<String>>,
    pub side: Signal<bool>,
    pub sheet: Signal<Sheet>,
    pub toast: Signal<Option<String>>,
    pub usage: Signal<Option<Vec<Usage>>>,
    pub found: Signal<Option<Found>>,
    pub now: Signal<i64>,
}

impl Ui {
    /// A person's act, done.
    pub fn act(self, act: Act) {
        let mut then = Then::default();
        self.model.update(|m| then = m.act(act));
        self.then(then);
    }

    /// A command to the shown assistant's engine.
    pub fn send(self, cmd: Command) {
        let shown = self.shown.get_untracked();
        if let Some(tx) = ENGINES.lock().unwrap().get(&shown) {
            let _ = tx.send(cmd);
        }
    }

    /// The assistant shown.
    pub fn me(self) -> Assistant {
        let shown = self.shown.get();
        self.assistants.with(|all| all.iter().find(|a| a.id == shown).cloned()).unwrap_or_else(Assistant::acorn)
    }

    /// An assistant's chat shown, from scratch: its engine tells it all
    /// anew. The one shown again: back to its chat.
    pub fn show(self, id: String) {
        if self.shown.get_untracked() == id {
            self.act(Act::Goto(None));
            return;
        }
        self.shown.set(id);
        self.model.set(Model::default());
        self.sheet.set(Sheet::None);
        self.usage.set(None);
        self.found.set(None);
        self.send(Command::Attach);
    }

    fn then(self, then: Then) {
        for cmd in then.cmds {
            self.send(cmd);
        }
        if let Some(note) = then.notes.into_iter().last() {
            self.note(note);
        }
        if let Some(days) = then.usage {
            self.usage.set(Some(days));
        }
        if let Some(found) = then.found {
            self.found.set(Some(found));
        }
    }

    /// A note, shown a while.
    pub fn note(self, text: String) {
        self.toast.set(Some(text.clone()));
        let toast = self.toast;
        set_timeout(Duration::from_secs(6), move || {
            if toast.get_untracked().as_deref() == Some(text.as_str()) {
                toast.set(None);
            }
        });
    }
}

/// The window's root.
#[component]
pub fn App() -> impl IntoElement {
    let assistants = acorn_core::assistant::all();
    let ui = Ui {
        model: signal_with(Model::default(), SignalEquals::Never),
        assistants: signal(assistants.clone()),
        shown: signal(assistants[0].id.clone()),
        working: signal(HashSet::new()),
        side: signal(true),
        sheet: signal(Sheet::None),
        toast: signal(None),
        usage: signal(None),
        found: signal(None),
        now: signal(acorn_core::now_ms()),
    };
    provide_context(ui);
    hear(ui);
    for me in assistants {
        start(me);
    }
    ui.send(Command::Attach);
    set_interval(Duration::from_secs(60), move || ui.now.set(acorn_core::now_ms()));
    keys(ui);
    view()
        .flex_1()
        .flex_row()
        .bg(paper())
        .text_color(ink())
        .on_file_drop(move |e: &FileDropEvent| sheets::dropped(ui, e))
        .child(side::bar(ui))
        .child(view().flex_1().child(header(ui)).child(rows::chat(ui)).child(footer(ui)))
        .child(sheets::sheet(ui))
}

/// The engines' events, heard as long as the window: whether each works,
/// an assistant one made (listed, started), its notes (named), always;
/// the rest, of the one shown.
fn hear(ui: Ui) {
    let (tx, mut heard) = tokio::sync::mpsc::unbounded_channel::<(String, Event)>();
    if HEARD.set(tx).is_err() {
        return;
    }
    spawn(async move {
        while let Some((id, ev)) = heard.recv().await {
            if let Event::State(s) = &ev {
                let works = s.busy || !s.agents.is_empty();
                if works != ui.working.with_untracked(|w| w.contains(&id)) {
                    ui.working.update(|w| {
                        let _ = if works { w.insert(id.clone()) } else { w.remove(&id) };
                    });
                }
            }
            if let Event::Assistant(made) = ev {
                ui.assistants.update(|all| all.push(made.clone()));
                start(made);
                continue;
            }
            if ui.shown.get_untracked() != id {
                // a hidden one's note (it can't open its chat), still shown
                if let Event::Note(text) = ev {
                    let name = ui.assistants.with_untracked(|all| all.iter().find(|a| a.id == id).map(|a| a.name.clone())).unwrap_or(id);
                    ui.note(format!("{name}: {text}"));
                }
                continue;
            }
            let mut then = Then::default();
            ui.model.update(|m| then = m.hear(ev));
            ui.then(then);
        }
    });
}

/// An assistant's engine, started on the runtime's threads (once), its
/// events to the window, named by its id.
pub fn start(me: Assistant) {
    let (cmds, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (events, mut told) = tokio::sync::mpsc::unbounded_channel();
    let Some(heard) = HEARD.get().cloned() else { return };
    let id = me.id.clone();
    {
        let mut engines = ENGINES.lock().unwrap();
        if engines.contains_key(&id) {
            return;
        }
        engines.insert(id.clone(), cmds);
    }
    RUNTIME.spawn(async move {
        while let Some(ev) = told.recv().await {
            let _ = heard.send((id.clone(), ev));
        }
    });
    let failed = events.clone();
    RUNTIME.spawn(async move {
        let name = me.name.clone();
        match Engine::start(me, events) {
            Ok(engine) => {
                while let Some(cmd) = rx.recv().await {
                    engine.command(cmd);
                }
            }
            Err(err) => {
                let _ = failed.send(Event::Note(format!("{name} can't open its chat: {err:#}")));
            }
        }
    });
}

/// The keys: Esc interrupts the assistant's turn (agents go on); Ctrl+Tab
/// goes to the next tab; Cmd+Shift+F folds the chat, and again unfolds
/// it; Cmd+B hides the sidebar, and again shows it.
fn keys(ui: Ui) {
    on_key("escape", move |_| {
        if ui.sheet.get_untracked() != Sheet::None {
            ui.sheet.set(Sheet::None);
        } else {
            ui.act(Act::Send { text: String::new(), images: vec![] });
        }
    });
    on_key("ctrl+tab", move |_| {
        let tabs = ui.model.with_untracked(|m| m.tabs());
        let at = ui.model.with_untracked(|m| tabs.iter().position(|t| *t == m.page).map_or(0, |k| k + 1));
        ui.act(Act::Goto(tabs[at % tabs.len()].clone()));
    });
    on_key("mod+shift+f", move |_| {
        let folded = ui.model.with_untracked(|m| m.fold.is_some());
        ui.act(Act::Fold(!folded));
    });
    on_key("mod+b", move |_| ui.side.update(|s| *s = !*s));
}

/// The sidebar's button, the assistant's name and effort (back to its
/// chat), an import's progress, the menu.
fn header(ui: Ui) -> impl IntoElement {
    let importing = move || ui.model.with(|m| m.state.as_ref().and_then(|s| s.importing.clone()));
    let effort = move || ui.model.with(|m| m.pick().map(|p| format!("{} effort", p.label())).unwrap_or_default());
    view()
        .flex_row()
        .items_center()
        .gap_3()
        .h(px(52.0))
        .px_5()
        .border_b_1()
        .border_color(hair())
        .child(chip(|| "◧".to_string(), move || ui.side.update(|s| *s = !*s)).tooltip(hint("Hide or show the assistants (⌘B)")))
        .child(
            view()
                .flex_row()
                .items_baseline()
                .gap_2()
                .on_press(move || ui.act(Act::Goto(None)))
                .child(text(move || ui.me().name).font_family(BRAND).text_size(px(18.0)).font_weight(FontWeight::SEMIBOLD))
                .child(text(effort).text_size(px(12.5)).text_color(mid()).tooltip(hint(move || ui.model.with(|m| m.pick().map(|p| p.effort.hint()).unwrap_or_default())))),
        )
        .child(view().flex_1())
        .child(when(move || importing().is_some(), move || {
            chip(move || importing().map(|p| model::progress_show(&p)).unwrap_or_default(), move || ui.sheet.set(Sheet::Import))
        }))
        .child(chip(|| "☰".to_string(), move || ui.sheet.update(|s| *s = if *s == Sheet::Menu { Sheet::None } else { Sheet::Menu })))
}

/// A hint shown as the pointer rests on what it explains.
pub fn hint(t: impl IntoReactive<SharedString>) -> AnyElement {
    text(t).text_size(px(12.0)).px_2().py_1().rounded(px(6.0)).bg(ink()).text_color(paper()).into_element()
}

/// A gray button of the header.
fn chip(label: impl Fn() -> String + 'static, press: impl Fn() + 'static) -> View {
    view()
        .h(px(32.0))
        .px_3()
        .justify_center()
        .rounded(px(8.0))
        .bg(block())
        .hover(|s| s.bg(hover()))
        .on_press(press)
        .child(text(label).text_size(px(13.5)).font_weight(FontWeight::MEDIUM))
}

/// The note shown, today's cost, the images waiting, the
/// message box, the tabs.
fn footer(ui: Ui) -> impl IntoElement {
    let images = signal(Vec::<Vec<u8>>::new());
    view()
        .px_5()
        .pt_2()
        .pb_3()
        .items_center()
        .child(
            view()
                .w_full()
                .max_w(px(COL))
                .gap_2()
                .child(when(move || ui.toast.get().is_some(), move || {
                    view()
                        .self_center()
                        .px_4()
                        .py_2()
                        .rounded(px(12.0))
                        .bg(ink())
                        .on_press(move || ui.toast.set(None))
                        .child(text(move || ui.toast.get().unwrap_or_default()).text_color(paper()).text_size(px(13.5)))
                }))
                .child(cost_row(ui))
                .child(rows::pending(images))
                .child(composer(ui, images))
                .child(tabs(ui)),
        )
}

/// Today's cost, at the right (a click: the last days).
fn cost_row(ui: Ui) -> impl IntoElement {
    let today = move || ui.model.with(|m| m.state.as_ref().and_then(|s| s.today).filter(|t| t.n > 0).map(|t| format!("{} today", text::usd_show(t.usd))));
    view()
        .flex_row()
        .items_center()
        .gap_2()
        .child(view().flex_1())
        .child(when(move || today().is_some(), move || {
            text(move || today().unwrap_or_default())
                .text_size(px(12.5))
                .text_color(mid())
                .tabular_nums()
                .tooltip(hint(move || {
                    ui.model.with(|m| m.state.as_ref().and_then(|s| s.today).map(|t| format!("{}\nAll of today's calls: replies, summaries, agents. Click for the last days.", text::use_hint(&t))).unwrap_or_default())
                }))
                .on_press(move || {
                    ui.send(Command::Usage);
                    ui.sheet.set(Sheet::Usage);
                })
        }))
}

/// The message box: Return sends (Shift+Return a new line), an empty one
/// stops Acorn's turn; images come by the attach button or a paste.
fn composer(ui: Ui, images: Signal<Vec<Vec<u8>>>) -> impl IntoElement {
    let draft = signal(String::new());
    let field = element_ref();
    let typing = move || ui.model.with(|m| m.typing());
    let stop = move || ui.model.with(|m| m.busy()) && draft.with(|d| d.trim().is_empty()) && images.with(|i| i.is_empty());
    let submit = move || {
        let text = draft.get_untracked();
        draft.set(String::new());
        ui.act(Act::Send { text, images: images.get_untracked() });
        images.set(Vec::new());
    };
    view()
        .flex_row()
        .items_end()
        .gap_2()
        .pl_5()
        .pr(px(7.0))
        .py(px(7.0))
        .rounded(px(24.0))
        .border_1()
        .border_color(move || if typing() { frame() } else { hair() })
        .bg(lift())
        .child(
            view()
                .size(px(38.0))
                .ml(px(-13.0))
                .rounded_full()
                .items_center()
                .justify_center()
                .hover(|s| s.bg(hover()))
                .opacity(move || if typing() { 1.0_f32 } else { 0.0 })
                .on_press(move || rows::attach(images))
                .child(text("+").text_size(px(22.0)).text_color(mid())),
        )
        .child(
            input()
                .element_ref(&field)
                .label("Message")
                .flex_1()
                .multiline(true)
                .auto_focus(true)
                .value(draft)
                .on_change(move |t: &str| draft.set(t.to_string()))
                .submit_behavior(SubmitBehavior::Submit)
                .on_submit(move |_: &str| submit())
                .paste_behavior(PasteBehavior::Insert)
                .on_paste(move |e: &PasteEvent| rows::pasted(images, e))
                .readonly(move || !typing())
                .placeholder(move || if typing() { format!("Message {}", ui.me().name) } else { "Commands only: the ☰ menu".to_string() })
                .placeholder_color(move || color(soft()))
                .text_color(ink())
                .text_size(px(16.0))
                .py(px(7.0))
                .max_h(px(320.0)),
        )
        .child(
            view()
                .size(px(38.0))
                .rounded_full()
                .items_center()
                .justify_center()
                .bg(move || if stop() { block() } else if typing() { ink() } else { hair() })
                .on_press(move || submit())
                .child(text(move || if stop() { "■".to_string() } else { "↑".to_string() }).text_size(px(17.0)).text_color(move || if stop() { ink() } else { paper() })),
        )
}

/// Acorn's chat, each agent at work, and the page shown.
fn tabs(ui: Ui) -> impl IntoElement {
    let tabs = computed(move || ui.model.with(|m| m.tabs()));
    when(move || tabs.with(|t| t.len() > 1), move || {
        view().flex_row().gap_4().child(dynamic(move || {
            let here = ui.model.with(|m| m.page.clone());
            let me = ui.me().name;
            tabs.get()
                .into_iter()
                .map(|name| {
                    let on = name == here;
                    let label = format!("{} {}", if on { "●" } else { "◯" }, name.as_deref().unwrap_or(&me));
                    let to = name.clone();
                    text(label)
                        .text_size(px(13.0))
                        .text_color(if on { ink() } else { mid() })
                        .font_weight(if on { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                        .on_press(move || ui.act(Act::Goto(to.clone())))
                })
                .collect::<Vec<_>>()
        }))
    })
}
