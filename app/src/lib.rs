//! Acorn
//! =====
//! The window around the assistants' chats (`acorn_core`): their engines
//! run on the runtime's threads (`hub`), the window on its own. You act by
//! words alone: the message box talks to the assistant you picked, and a
//! message sent while it works cuts in; `@Name` alone talks to another,
//! and in a message has yours ask them. Hands only look: pick an
//! assistant, open a block, look at an agent's work, or at a thread where
//! another assistant answered the one you talk to. The person's acts go
//! to the model (`model::Model::act`), the shown chat's events too
//! (`hear`), and both answer what to send it. The page: the sidebar
//! (`side`: the assistants), a header (the sidebar's button, the
//! assistant's name and who it is, an import's progress), the chat, and a
//! footer (a note, the goal, today's cost, the images waiting, the message
//! box, the tabs while agents work).

mod call;
mod look;
mod model;
mod rows;
mod side;
mod text;

use acorn_core::assistant::Assistant;
use acorn_core::chat::hub;
use acorn_core::wire::{Command, Event};
use look::*;
use model::{Act, Model, Then};
use nsh_base::input;
use nsh_core::prelude::*;
use std::collections::HashSet;
use std::sync::LazyLock;
use std::time::Duration;

/// The engines' threads: their calls, tools and files never hold the
/// window's.
static RUNTIME: LazyLock<tokio::runtime::Runtime> =
    LazyLock::new(|| tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("acorn").build().expect("the engine's runtime"));

/// What every part of the window shares: the model (of the chat shown),
/// the assistants, the one you talk to, the chat shown (its chat with
/// you, or a thread it asked in), the chats at work, whether the sidebar
/// shows, the note shown, the message being written and its field, the
/// clock times read.
#[derive(Clone, Copy)]
pub struct Ui {
    pub model: Signal<Model>,
    pub assistants: Signal<Vec<Assistant>>,
    pub talk: Signal<String>,
    pub shown: Signal<String>,
    pub working: Signal<HashSet<String>>,
    pub side: Signal<bool>,
    pub toast: Signal<Option<String>>,
    pub draft: Signal<String>,
    pub field: ElementRef,
    pub now: Signal<i64>,
}

impl Ui {
    /// A person's act, done.
    pub fn act(self, act: Act) {
        let mut then = Then::default();
        self.model.update(|m| then = m.act(act));
        self.then(then);
    }

    /// A command to the shown chat's engine.
    pub fn send(self, cmd: Command) {
        if let Some(engine) = hub::get(&self.shown.get_untracked()) {
            let _in = RUNTIME.enter();
            engine.command(cmd);
        }
    }

    /// The assistant you talk to.
    pub fn me(self) -> Assistant {
        let talk = self.talk.get();
        self.assistants.with(|all| all.iter().find(|a| a.id == talk).cloned()).unwrap_or_else(Assistant::acorn)
    }

    /// An assistant's name, by id.
    fn named(self, id: &str) -> String {
        self.assistants.with_untracked(|all| all.iter().find(|a| a.id == id).map(|a| a.name.clone())).unwrap_or_else(|| id.to_string())
    }

    /// A chat's model, empty: an assistant's opens with the AI notice; a
    /// thread's, with who asks and who answers there.
    fn fresh(self, id: &str) -> Model {
        match id.split_once('/') {
            Some((answers, asks)) => {
                let (answers, asks) = (self.named(answers), self.named(asks));
                Model::headed(format!("{answers}'s thread with {asks}: {asks} asks, {answers} answers. Esc goes back."), false)
            }
            None => Model::headed(model::notice(&self.named(id)), true),
        }
    }

    /// A chat shown, from scratch: its engine tells it all anew. The one
    /// shown again: back to its end.
    pub fn show(self, id: String) {
        if self.shown.get_untracked() == id {
            self.act(Act::Goto(None));
            return;
        }
        self.model.set(self.fresh(&id));
        self.shown.set(id);
        self.send(Command::Attach);
    }

    /// An assistant to talk to: its chat with you shown.
    pub fn talk_to(self, id: String) {
        self.talk.set(id.clone());
        self.show(id);
    }

    /// Back to your chat with the assistant, from whatever you look at: a
    /// thread, an agent's page, the overview.
    pub fn back(self) {
        let talk = self.talk.get_untracked();
        if self.shown.get_untracked() != talk {
            self.show(talk);
        } else {
            self.act(Act::Goto(None));
            self.act(Act::Fold(false));
        }
    }

    /// Your words, to the assistant you talk to, its chat shown: sent while
    /// it works, they cut in; `@Name` alone, that assistant's chat shown
    /// instead. Whether they went (not while its chat still opens).
    pub fn say(self, text: String, images: Vec<Vec<u8>>) -> bool {
        if text.trim().is_empty() && images.is_empty() {
            return false;
        }
        if images.is_empty()
            && let Some(id) = self.assistants.with_untracked(|all| {
                let names: Vec<String> = all.iter().map(|a| a.name.clone()).collect();
                text::mention_alone(&text, &names).and_then(|n| all.iter().find(|a| a.name == n)).map(|a| a.id.clone())
            })
        {
            self.talk_to(id);
            return true;
        }
        let talk = self.talk.get_untracked();
        if hub::get(&talk).is_none() {
            self.note(format!("{}'s chat is still opening: send again in a moment.", self.named(&talk)));
            return false;
        }
        if self.shown.get_untracked() != talk {
            self.show(talk);
        }
        self.act(Act::Send { text, images });
        true
    }

    /// A name in the chat, opened: an assistant's, its thread with the one
    /// you talk to; an agent's, its page.
    pub fn open_name(self, name: &str) {
        match self.assistants.with_untracked(|all| all.iter().find(|a| a.name == name).map(|a| a.id.clone())) {
            Some(id) => self.show(format!("{id}/{}", self.talk.get_untracked())),
            None => self.act(Act::Goto(Some(name.to_string()))),
        }
    }

    fn then(self, then: Then) {
        for cmd in then.cmds {
            self.send(cmd);
        }
        if let Some(note) = then.notes.into_iter().last() {
            self.note(note);
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
    let first = assistants[0].id.clone();
    let ui = Ui {
        model: signal_with(Model::default(), SignalEquals::Never),
        assistants: signal(assistants),
        talk: signal(first.clone()),
        shown: signal(first),
        working: signal(HashSet::new()),
        side: signal(true),
        toast: signal(None),
        draft: signal(String::new()),
        field: element_ref(),
        now: signal(acorn_core::now_ms()),
    };
    provide_context(ui);
    ui.model.set(ui.fresh(&ui.shown.get_untracked()));
    hear(ui);
    set_interval(Duration::from_secs(60), move || ui.now.set(acorn_core::now_ms()));
    keys(ui);
    view()
        .flex_1()
        .flex_row()
        .bg(paper())
        .text_color(ink())
        .on_file_drop(move |e: &FileDropEvent| dropped(ui, e))
        .child(side::bar(ui))
        .child(view().flex_1().child(header(ui)).child(rows::chat(ui)).child(footer(ui)))
}

/// Every chat opened, its events heard as long as the window: whether each
/// works, an assistant created or restored (listed) or archived (dropped, and the one
/// you talk to Acorn, if it was), a hidden chat's notes (named),
/// always; the rest, of the chat shown.
fn hear(ui: Ui) {
    let (tx, mut heard) = tokio::sync::mpsc::unbounded_channel::<(String, Event)>();
    if !hub::listen(tx) {
        return;
    }
    let first = ui.shown.get_untracked();
    RUNTIME.spawn(async move {
        hub::open_all();
        if let Some(engine) = hub::get(&first) {
            engine.attach();
        }
    });
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
                ui.assistants.update(|all| all.push(made));
                continue;
            }
            if let Event::Gone(gone) = ev {
                ui.assistants.update(|all| all.retain(|a| a.id != gone));
                ui.working.update(|w| w.retain(|chat| hub::whose(chat) != gone && !chat.ends_with(&format!("/{gone}"))));
                let shown = ui.shown.get_untracked();
                if ui.talk.get_untracked() == gone || hub::whose(&shown) == gone || shown.ends_with(&format!("/{gone}")) {
                    ui.talk_to(ui.assistants.with_untracked(|all| all[0].id.clone()));
                }
                continue;
            }
            if ui.shown.get_untracked() != id {
                if let Event::Note(text) = ev {
                    let who = match id.split_once('/') {
                        Some((answers, asks)) => format!("{} (asked by {})", ui.named(answers), ui.named(asks)),
                        None => ui.named(&id),
                    };
                    ui.note(format!("{who}: {text}"));
                }
                continue;
            }
            let mut then = Then::default();
            ui.model.update(|m| then = m.hear(ev));
            ui.then(then);
        }
    });
}

/// The keys, all for looking: Esc goes back to your chat; Ctrl+Tab to the
/// next tab; Cmd+Shift+F shows the overview, and again the chat; Cmd+B
/// hides the sidebar, and again shows it.
fn keys(ui: Ui) {
    on_key("escape", move |_| ui.back());
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

/// Files dropped on the window: their paths join the message being
/// written, to say what to do with them.
fn dropped(ui: Ui, e: &FileDropEvent) {
    let paths: Vec<String> = e.uris.iter().filter_map(|u| u.strip_prefix("file://")).map(text::percent_decoded).collect();
    if paths.is_empty() {
        return;
    }
    ui.draft.update(|d| {
        if !d.is_empty() && !d.ends_with(char::is_whitespace) {
            d.push(' ');
        }
        d.push_str(&paths.join(" "));
    });
}

/// The sidebar's button (while the sidebar is hidden; shown, it has its
/// own), the assistant's name (back to its chat) and who it is, an
/// import's progress.
fn header(ui: Ui) -> impl IntoElement {
    let importing = move || ui.model.with(|m| m.state.as_ref().and_then(|s| s.importing.clone()));
    view()
        .flex_row()
        .items_center()
        .gap_3()
        .h(px(52.0))
        .px_5()
        .border_b_1()
        .border_color(hair())
        .child(when(move || !ui.side.get(), move || side_chip(ui)))
        .child(
            // One paragraph, two runs: they share its baseline (flex has none for text).
            text(move || ui.me().name)
                .font_family(BRAND)
                .text_size(px(18.0))
                .font_weight(FontWeight::SEMIBOLD)
                .number_of_lines(1)
                .on_press(move || ui.back())
                .child(
                    text(move || Some(ui.me().about).filter(|a| !a.is_empty()).map(|a| format!("  ·  {a}")).unwrap_or_default())
                        .font_family("system-ui")
                        .text_size(px(13.0))
                        .font_weight(FontWeight::NORMAL)
                        .text_color(mid()),
                ),
        )
        .child(view().flex_1())
        .child(when(move || importing().is_some(), move || {
            chip(move || importing().map(|p| model::progress_show(&p)).unwrap_or_default()).tooltip(hint("Your past chats going into memory. Ask to pause or resume it."))
        }))
}

/// A hint shown as the pointer rests on what it explains.
pub fn hint(t: impl IntoReactive<SharedString>) -> AnyElement {
    text(t).text_size(px(12.0)).px_2().py_1().rounded(px(6.0)).bg(ink()).text_color(paper()).into_element()
}

/// The button that hides or shows the sidebar.
pub fn side_chip(ui: Ui) -> View {
    chip(|| "◧".to_string()).hover(|s| s.bg(hover())).on_press(move || ui.side.update(|s| *s = !*s)).tooltip(hint("Hide or show the assistants (⌘B)"))
}

/// A gray chip of the header.
fn chip(label: impl Fn() -> String + 'static) -> View {
    view().h(px(32.0)).px_3().justify_center().rounded(px(8.0)).bg(block()).child(text(label).text_size(px(13.5)).font_weight(FontWeight::MEDIUM))
}

/// The note shown, the goal, today's cost, the images waiting, the
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
                .child(status_row(ui))
                .child(rows::pending(images))
                .child(composer(ui, images))
                .child(tabs(ui)),
        )
}

/// The goal the assistant works to, at the left; today's cost, at the
/// right.
fn status_row(ui: Ui) -> impl IntoElement {
    let goal = move || ui.model.with(|m| m.state.as_ref().and_then(|s| s.goal.clone()));
    let today = move || ui.model.with(|m| m.state.as_ref().and_then(|s| s.today).filter(|t| t.n > 0).map(|t| format!("{} today", text::usd_show(t.usd))));
    view()
        .flex_row()
        .items_center()
        .gap_3()
        .child(when(move || goal().is_some(), move || {
            text(move || format!("Goal: {}", goal().unwrap_or_default()))
                .number_of_lines(1)
                .text_size(px(12.5))
                .text_color(mid())
                .tooltip(hint(move || format!("{}\nIt keeps working till a check finds this met. Say so to drop it.", goal().unwrap_or_default())))
        }))
        .child(view().flex_1())
        .child(when(move || today().is_some(), move || {
            text(move || today().unwrap_or_default())
                .text_size(px(12.5))
                .text_color(mid())
                .tabular_nums()
                .tooltip(hint(move || {
                    ui.model.with(|m| m.state.as_ref().and_then(|s| s.today).map(|t| format!("{}\nToday's calls in this chat: replies, summaries, agents. Ask for usage to see more.", text::use_hint(&t))).unwrap_or_default())
                }))
        }))
}

/// The message box: Return sends (Shift+Return a new line); images come
/// by the attach button or a paste. Sent while the assistant works, a
/// message cuts in. An `@` word being typed shows the other assistants
/// it may name, over the caret (`names_menu`): ↑ and ↓ pick one, Return
/// or Tab takes it (Return sends a word already a name), a click takes
/// the one clicked, Esc closes it for that word.
fn composer(ui: Ui, images: Signal<Vec<Vec<u8>>>) -> impl IntoElement {
    let field = ui.field;
    let caret = signal(0u32);
    let at = signal(None::<Bounds<Points>>);
    let pick = signal(0usize);
    let hushed = signal(None::<usize>);
    let ready = move || ui.draft.with(|d| !d.trim().is_empty()) || images.with(|i| !i.is_empty());
    // The `@` word's start and the assistants it may name, with whether it is one already.
    let naming = computed(move || {
        let me = ui.me().name;
        let names: Vec<String> = ui.assistants.with(|all| all.iter().map(|a| a.name.clone()).filter(|n| *n != me).collect());
        ui.draft.with(|d| {
            let (start, word) = text::mention_at(d, caret.get())?;
            let found: Vec<String> = text::names_for(word, &names).into_iter().map(String::from).collect();
            let whole = found.iter().any(|n| n.eq_ignore_ascii_case(word));
            (!found.is_empty() && hushed.get() != Some(start)).then_some((start, found, whole))
        })
    });
    let take = move |name: String| {
        let Some((start, ..)) = naming.get_untracked() else { return };
        let (text, to) = ui.draft.with_untracked(|d| text::mention_done(d, start, caret.get_untracked(), &name));
        ui.draft.set(text);
        caret.set(to);
        field.focus();
        field.set_selection(to, to);
    };
    let submit = move || {
        if let Some((_, found, false)) = naming.get_untracked() {
            take(found[pick.get_untracked().min(found.len() - 1)].clone());
            return;
        }
        if ui.say(ui.draft.get_untracked(), images.get_untracked()) {
            ui.draft.set(String::new());
            images.set(Vec::new());
        }
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
        .border_color(frame())
        .bg(lift())
        .child(
            view()
                .size(px(38.0))
                .ml(px(-13.0))
                .rounded_full()
                .items_center()
                .justify_center()
                .hover(|s| s.bg(hover()))
                .on_press(move || rows::attach(images))
                .tooltip(hint("Attach images"))
                .child(text("+").text_size(px(22.0)).text_color(mid())),
        )
        .child(
            input()
                .element_ref(&field)
                .label(move || format!("Message {}", ui.me().name))
                .flex_1()
                .multiline(true)
                .auto_focus(true)
                .value(ui.draft)
                .on_change(move |t: &str| {
                    ui.draft.set(t.to_string());
                    pick.set(0);
                    // the word closed is gone with its `@`
                    if hushed.get_untracked().is_some_and(|start| t.get(start..start + 1) != Some("@")) {
                        hushed.set(None);
                    }
                })
                .on_selection_change(move |s: &Selection| caret.set(s.end))
                .on_caret_move(move |e: &CaretEvent| at.set(Some(e.page_bounds)))
                .submit_behavior(SubmitBehavior::Submit)
                .on_submit(move |_: &str| submit())
                .paste_behavior(PasteBehavior::Insert)
                .on_paste(move |e: &PasteEvent| rows::pasted(images, e))
                .placeholder(move || format!("Message {}", ui.me().name))
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
                .bg(move || if ready() { ink() } else { hair() })
                .on_press(submit)
                .child(text("↑").text_size(px(17.0)).text_color(paper())),
        )
        .child(when(move || naming.with(Option::is_some) && at.get().is_some(), move || names_menu(ui, Menu { at, naming, pick, hushed }, take)))
}

/// What the names menu shows and keeps: where the caret is, the `@`
/// word's start and names, the one picked, the word it was closed for.
#[derive(Clone, Copy)]
struct Menu {
    at: Signal<Option<Bounds<Points>>>,
    naming: Memo<Option<(usize, Vec<String>, bool)>>,
    pick: Signal<usize>,
    hushed: Signal<Option<usize>>,
}

/// The assistants an `@` word may name, over the caret: each its name and
/// who it is, the one picked marked. Its keys are bound while it shows,
/// in the message box too, over the window's.
fn names_menu(ui: Ui, menu: Menu, take: impl Fn(String) + Copy + 'static) -> impl IntoElement {
    let Menu { at, naming, pick, hushed } = menu;
    let keys = KeyOptions { when: nsh_core::keymap::When::Always, repeat: true };
    let found = move || naming.get_untracked().map(|(_, found, _)| found).unwrap_or_default();
    on_key_with("up", keys, move |_| pick.update(|p| *p = (*p + found().len().max(1) - 1) % found().len().max(1)));
    on_key_with("down", keys, move |_| pick.update(|p| *p = (*p + 1) % found().len().max(1)));
    on_key_with("tab", keys, move |_| {
        if let Some(name) = found().get(pick.get_untracked()).cloned() {
            take(name);
        }
    });
    on_key_with("escape", KeyOptions { repeat: false, ..keys }, move |_| hushed.set(naming.get_untracked().map(|(start, ..)| start)));
    overlay()
        .anchor_bounds(move || at.get().unwrap_or_default())
        .placement(Placement::Top)
        .align(Align::Start)
        .offset(px(6.0))
        .child(view().py_1().min_w(px(220.0)).rounded(px(10.0)).border_1().border_color(frame()).bg(lift()).child(dynamic(move || {
            let found = naming.get().map(|(_, found, _)| found).unwrap_or_default();
            found
                .into_iter()
                .enumerate()
                .map(|(k, name)| {
                    let about = ui.assistants.with(|all| all.iter().find(|a| a.name == name).map(|a| a.about.clone())).unwrap_or_default();
                    let to = name.clone();
                    view()
                        .flex_row()
                        .gap_2()
                        .px_3()
                        .py(px(6.0))
                        .bg(move || if pick.get() == k { hover() } else { lift() })
                        .hover(|s| s.bg(hover()))
                        .on_press(move || take(to.clone()))
                        .child(text(format!("@{name}")).text_size(px(14.0)).font_weight(FontWeight::MEDIUM))
                        .child(text(about).text_size(px(13.0)).text_color(mid()).number_of_lines(1))
                })
                .collect::<Vec<_>>()
        })))
}

/// The chat, each agent at work, and the page shown.
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
