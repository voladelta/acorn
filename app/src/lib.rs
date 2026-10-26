//! Acorn
//! =====
//! The window around Acorn's chat (`acorn_core`): the engine runs on the
//! runtime's threads, the window on its own; the person's acts go to the
//! model (`model::Model::act`), the engine's events too (`hear`), and both
//! answer what to send the engine. The page: a header (the name, an
//! import's progress, the ☰ menu), the chat, and a footer (the pick and
//! today's cost, the images waiting, the message box, the tabs while
//! agents work).

mod call;
mod look;
mod model;
mod rows;
mod sheets;
mod text;

use acorn_core::chat::Engine;
use acorn_core::import::Found;
use acorn_core::pick::Move;
use acorn_core::wire::{Command, Usage};
use look::*;
use model::{Act, Model, Then};
use nsh_base::input;
use nsh_core::prelude::*;
use std::sync::{LazyLock, OnceLock};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

/// Where the window's commands go: the engine.
static ENGINE: OnceLock<UnboundedSender<Command>> = OnceLock::new();

/// The engine's threads: its calls, tools and files never hold the
/// window's.
static RUNTIME: LazyLock<tokio::runtime::Runtime> =
    LazyLock::new(|| tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("acorn").build().expect("the engine's runtime"));

fn send(cmd: Command) {
    if let Some(tx) = ENGINE.get() {
        let _ = tx.send(cmd);
    }
}

/// A box over the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sheet {
    None,
    Menu,
    Usage,
    Import,
}

/// What every part of the window shares: the model, the box over the
/// page, the notes shown, what the usage and import boxes show, the
/// clock their times read.
#[derive(Clone, Copy)]
pub struct Ui {
    pub model: Signal<Model>,
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

    fn then(self, then: Then) {
        then.cmds.into_iter().for_each(send);
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
    let ui = Ui {
        model: signal_with(Model::default(), SignalEquals::Never),
        sheet: signal(Sheet::None),
        toast: signal(None),
        usage: signal(None),
        found: signal(None),
        now: signal(acorn_core::now_ms()),
    };
    provide_context(ui);
    start(ui);
    set_interval(Duration::from_secs(60), move || ui.now.set(acorn_core::now_ms()));
    keys(ui);
    view()
        .flex_1()
        .bg(paper())
        .text_color(ink())
        .on_file_drop(move |e: &FileDropEvent| sheets::dropped(ui, e))
        .child(header(ui))
        .child(rows::chat(ui))
        .child(footer(ui))
        .child(sheets::sheet(ui))
}

/// The engine, started on the runtime's threads, its events heard here.
fn start(ui: Ui) {
    let (cmds, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (events, mut heard) = tokio::sync::mpsc::unbounded_channel();
    if ENGINE.set(cmds).is_err() {
        return;
    }
    let failed = events.clone();
    RUNTIME.spawn(async move {
        match Engine::start(events) {
            Ok(engine) => {
                engine.attach();
                while let Some(cmd) = rx.recv().await {
                    engine.command(cmd);
                }
            }
            Err(err) => {
                let _ = failed.send(acorn_core::wire::Event::Note(format!("Acorn can't open its chat: {err:#}")));
            }
        }
    });
    spawn(async move {
        while let Some(ev) = heard.recv().await {
            let mut then = Then::default();
            ui.model.update(|m| then = m.hear(ev));
            ui.then(then);
        }
    });
}

/// The keys: Esc interrupts Acorn's turn (agents go on); Shift+Up and
/// Shift+Down move the effort; Ctrl+Tab goes to the next tab; Cmd+Shift+F
/// folds the chat, and again unfolds it.
fn keys(ui: Ui) {
    on_key("escape", move |_| {
        if ui.sheet.get_untracked() != Sheet::None {
            ui.sheet.set(Sheet::None);
        } else {
            ui.act(Act::Send { text: String::new(), images: vec![] });
        }
    });
    on_key("shift+up", move |_| ui.act(Act::Move(Move::Up)));
    on_key("shift+down", move |_| ui.act(Act::Move(Move::Down)));
    on_key("ctrl+tab", move |_| {
        let tabs = ui.model.with_untracked(|m| m.tabs());
        let at = ui.model.with_untracked(|m| tabs.iter().position(|t| *t == m.page).map_or(0, |k| k + 1));
        ui.act(Act::Goto(tabs[at % tabs.len()].clone()));
    });
    on_key("mod+shift+f", move |_| {
        let folded = ui.model.with_untracked(|m| m.fold.is_some());
        ui.act(Act::Fold(!folded));
    });
}

/// The name (back to Acorn's chat), an import's progress, the menu.
fn header(ui: Ui) -> impl IntoElement {
    let importing = move || ui.model.with(|m| m.state.as_ref().and_then(|s| s.importing.clone()));
    view()
        .flex_row()
        .items_center()
        .gap_2()
        .h(px(52.0))
        .px_5()
        .border_b_1()
        .border_color(hair())
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_2()
                .on_press(move || ui.act(Act::Goto(None)))
                .child(text("🌰").text_size(px(20.0)))
                .child(text("Acorn").font_family(BRAND).text_size(px(20.0)).font_weight(FontWeight::SEMIBOLD)),
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
fn chip(label: impl Fn() -> String + 'static, press: impl Fn() + 'static) -> impl IntoElement {
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

/// The note shown, the pick and today's cost, the images waiting, the
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
                .child(pick_row(ui))
                .child(rows::pending(images))
                .child(composer(ui, images))
                .child(tabs(ui)),
        )
}

/// A rule, then the effort (a click moves it), then today's cost (a
/// click: the last days).
fn pick_row(ui: Ui) -> impl IntoElement {
    let effort = move || ui.model.with(|m| m.pick().map(|p| p.label().to_string()).unwrap_or_default());
    let today = move || ui.model.with(|m| m.state.as_ref().and_then(|s| s.today).filter(|t| t.n > 0).map(|t| format!("{} today", text::usd_show(t.usd))));
    view()
        .flex_row()
        .items_center()
        .gap_2()
        .child(view().flex_1().h(px(1.0)).bg(hair()))
        .child(
            text(effort)
                .text_size(px(12.5))
                .text_color(mid())
                .px_1()
                .rounded(px(6.0))
                .hover(|s| s.bg(block()))
                .tooltip(hint(move || ui.model.with(|m| m.pick().map(|p| p.effort.hint()).unwrap_or_default())))
                .on_press(move || ui.act(Act::Move(Move::Around))),
        )
        .child(when(move || today().is_some(), move || {
            text(move || today().unwrap_or_default())
                .text_size(px(12.5))
                .text_color(mid())
                .tabular_nums()
                .tooltip(hint(move || {
                    ui.model.with(|m| m.state.as_ref().and_then(|s| s.today).map(|t| format!("{}\nAll of today's calls: replies, summaries, agents. Click for the last days.", text::use_hint(&t))).unwrap_or_default())
                }))
                .on_press(move || {
                    send(Command::Usage);
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
                .label("Message Acorn")
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
                .placeholder(move || if typing() { "Message Acorn".to_string() } else { "Commands only: the ☰ menu".to_string() })
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
            tabs.get()
                .into_iter()
                .map(|name| {
                    let on = name == here;
                    let label = format!("{} {}", if on { "●" } else { "◯" }, name.as_deref().unwrap_or(model::MAIN));
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
