//! Sheets
//! ======
//! The boxes over the page: the ☰ menu (Import History, Usage, Export
//! memory, Memory tree, fold, stop), the last days' usage, and Import
//! History: the Claude and ChatGPT chats found on this Mac (and in the
//! website exports dropped on the window), picked by session, by day and
//! with or without tool calls, priced before they go.

use crate::look::*;
use crate::model::{self, Act};
use crate::{Sheet, Ui, send};
use acorn_core::import::{self, Want};
use acorn_core::wire::Command;
use nsh_base::{input, text_view};
use nsh_core::prelude::*;

pub fn sheet(ui: Ui) -> impl IntoElement {
    dynamic(move || match ui.sheet.get() {
        Sheet::None => None,
        Sheet::Menu => Some(menu(ui).into_element()),
        Sheet::Usage => Some(boxed(ui, usage(ui)).into_element()),
        Sheet::Import => Some(boxed(ui, import(ui)).into_element()),
    })
}

/// The menu, under its button.
fn menu(ui: Ui) -> impl IntoElement {
    let folded = move || ui.model.with(|m| m.fold.is_some());
    let item = move |label: Box<dyn Fn() -> String>, press: Box<dyn Fn()>| {
        view()
            .px_2()
            .py_2()
            .rounded(px(10.0))
            .hover(|s| s.bg(hover()))
            .on_press(move || {
                ui.sheet.set(Sheet::None);
                press();
            })
            .child(text(label).text_size(px(14.5)))
    };
    let stamp = || chrono::Local::now().format("%Y-%m-%d").to_string();
    view()
        .absolute()
        .inset_0()
        .on_press(move || ui.sheet.set(Sheet::None))
        .child(
            view()
                .absolute()
                .top(px(56.0))
                .right(px(20.0))
                .w(px(200.0))
                .p(px(6.0))
                .rounded(px(14.0))
                .border_1()
                .border_color(hair())
                .bg(lift())
                .shadow([BoxShadow { offset_y: px(8.0), blur: px(24.0), color: rgba(0x0000001f).into(), ..BoxShadow::default() }])
                .child(item(Box::new(|| "Import History".into()), Box::new(move || open_import(ui))))
                .child(item(
                    Box::new(|| "Usage".into()),
                    Box::new(move || {
                        send(Command::Usage);
                        ui.sheet.set(Sheet::Usage);
                    }),
                ))
                .child(item(
                    Box::new(|| "Export memory".into()),
                    Box::new(move || {
                        let file = acorn_core::config::home().join("Downloads").join(format!("acorn-memory-{}.json", stamp()));
                        send(Command::Export(file));
                    }),
                ))
                .child(item(
                    Box::new(|| "Memory tree".into()),
                    Box::new(move || send(Command::Tree(std::env::temp_dir().join(format!("acorn-tree-{}.html", stamp()))))),
                ))
                .child(item(
                    Box::new(move || if folded() { "Unfold all".into() } else { "Fold all".into() }),
                    Box::new(move || ui.act(Act::Fold(!folded()))),
                ))
                .child(item(Box::new(|| "Stop all work".into()), Box::new(|| send(Command::Stop(None))))),
        )
}

/// A box over the veiled page, centered; a click on the veil closes it.
fn boxed(ui: Ui, body: impl IntoElement) -> impl IntoElement {
    view()
        .absolute()
        .inset_0()
        .bg(veil())
        .items_center()
        .justify_center()
        .on_press(move || ui.sheet.set(Sheet::None))
        .child(
            scroll()
                .w(px(520.0))
                .max_h(percentage(0.85))
                .rounded(px(14.0))
                .border_1()
                .border_color(hair())
                .bg(lift())
                .on_press(|| {})
                .child(view().px(px(22.0)).py(px(20.0)).gap_3().child(body)),
        )
}

fn title(t: &str) -> Text {
    text(t.to_string()).text_size(px(16.0)).font_weight(FontWeight::SEMIBOLD)
}

fn sub(t: impl Into<String>) -> Text {
    text(t.into()).text_size(px(12.5)).text_color(mid())
}

/// A button of a box: the main one ink, the rest gray.
fn button(label: impl Fn() -> String + 'static, main: bool, press: impl Fn() + 'static) -> impl IntoElement {
    view()
        .px_4()
        .py(px(7.0))
        .rounded(px(8.0))
        .bg(if main { ink() } else { block() })
        .hover(move |s| if main { s.opacity(0.85) } else { s.bg(hover()) })
        .on_press(press)
        .child(text(label).text_size(px(14.0)).font_weight(FontWeight::MEDIUM).text_color(if main { paper() } else { ink() }))
}

/// The last 30 days' usage, by day and kind.
fn usage(ui: Ui) -> impl IntoElement {
    view()
        .gap_3()
        .child(title("Usage"))
        .child(dynamic(move || match ui.usage.get() {
            None => sub("Reading…").into_element(),
            Some(days) => text_view().markdown(model::usage_md(&days)).text_size(px(14.0)).text_color(ink()).into_element(),
        }))
        .child(sub("At DeepSeek's list prices, peak or off-peak as each request ran; cached input is billed at a fraction."))
}

/// Import History, its sessions looked for anew.
fn open_import(ui: Ui) {
    ui.found.set(None);
    send(Command::Find(Vec::new()));
    ui.sheet.set(Sheet::Import);
}

/// A website export dropped on the window: its chats join those found.
pub fn dropped(ui: Ui, e: &FileDropEvent) {
    let files: Vec<std::path::PathBuf> = e
        .uris
        .iter()
        .filter_map(|u| u.strip_prefix("file://"))
        .map(|p| std::path::PathBuf::from(percent_decoded(p)))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("zip") || x.eq_ignore_ascii_case("json")))
        .collect();
    if !files.is_empty() {
        ui.found.set(None);
        send(Command::Find(files));
        ui.sheet.set(Sheet::Import);
    }
}

fn percent_decoded(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let b = s.as_bytes();
    let mut k = 0;
    while k < b.len() {
        match (b[k], s.get(k + 1..k + 3).and_then(|h| u8::from_str_radix(h, 16).ok())) {
            (b'%', Some(v)) => {
                out.push(v);
                k += 3;
            }
            (c, _) => {
                out.push(c);
                k += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What an import would take, as picked; an import running, its progress.
fn import(ui: Ui) -> impl IntoElement {
    let importing = move || ui.model.with(|m| m.state.as_ref().and_then(|s| s.importing.clone()));
    let want = signal(Want::default());
    let replan = move |w: Want| {
        want.set(w.clone());
        send(Command::Plan(w));
    };
    let exports = import::EXPORTS.iter().map(|(app, how)| format!("{app}: {how}")).collect::<Vec<_>>().join("; ");
    view()
        .gap_3()
        .child(title("Import your past AI chats"))
        .child(when(move || importing().is_some(), move || {
            view()
                .gap_2()
                .child(text(move || importing().map(|p| model::progress_show(&p)).unwrap_or_default()).text_size(px(14.0)))
                .child(
                    view().h(px(6.0)).rounded(px(3.0)).bg(block()).child(
                        view()
                            .h_full()
                            .rounded(px(3.0))
                            .bg(move || if importing().is_some_and(|p| p.why.is_some()) { mid() } else { ink() })
                            .w(move || percentage(importing().map_or(0.0, |p| if p.total > 0.0 { (p.done / p.total) as f32 } else { 0.0 }))),
                    ),
                )
                .child(view().flex_row().justify_end().child(button(
                    move || if importing().is_some_and(|p| p.why.is_some()) { "Resume".into() } else { "Pause".into() },
                    false,
                    move || send(Command::Go(importing().is_some_and(|p| p.why.is_some()))),
                )))
        }))
        .child(sub(format!(
            "Claude Code's and Codex's chats on this Mac are found by themselves. For the websites' chats, drop their export (.zip) on this window; each emails it on request ({exports})."
        )))
        .child(dynamic(move || match ui.found.get() {
            None => sub("Looking for Claude and ChatGPT chats on this Mac…").into_element(),
            Some(found) => found_view(ui, found, want, replan).into_element(),
        }))
}

fn found_view(ui: Ui, found: import::Found, want: Signal<Want>, replan: impl Fn(Want) + Copy + 'static) -> impl IntoElement {
    let head = if found.msgs == 0 && found.first.is_empty() {
        import::OLD.to_string()
    } else {
        format!("Found {}. Importing them costs about {}.", import::count(found.msgs, "new "), import::guess_usd(found.usd))
    };
    let apps = import::APPS.iter().map(|&app| {
        let rows: Vec<_> = found.rows.iter().filter(|r| r.app == app).cloned().collect();
        let picked = rows.iter().filter(|r| r.picked).count();
        view()
            .gap_1()
            .p_3()
            .rounded(px(12.0))
            .border_1()
            .border_color(hair())
            .child(text(format!("{app} · {picked} of {} sessions", rows.len())).text_size(px(13.5)).font_weight(FontWeight::SEMIBOLD))
            .child(scroll().max_h(px(150.0)).child(view().gap_1().child(
                rows.into_iter()
                    .map(|r| {
                        let id = r.id.clone();
                        text(format!("{}  {}   {:>4}   {}", if r.picked { "☑" } else { "☐" }, r.cells[0], r.cells[1], r.cells[2]))
                            .font_family(MONO)
                            .text_size(px(12.0))
                            .number_of_lines(1)
                            .on_press(move || {
                                let mut w = want.get_untracked();
                                match w.off.iter().position(|o| *o == id) {
                                    Some(k) => drop(w.off.remove(k)),
                                    None => w.off.push(id.clone()),
                                }
                                replan(w);
                            })
                    })
                    .collect::<Vec<_>>(),
            )))
    });
    let since = signal(found.want.since.clone());
    view()
        .gap_3()
        .child(text(head).text_size(px(14.0)).font_weight(FontWeight::MEDIUM))
        .child(apps.collect::<Vec<_>>())
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_3()
                .child(sub("From day"))
                .child(
                    input()
                        .w(px(120.0))
                        .px_2()
                        .py_1()
                        .rounded(px(6.0))
                        .border_1()
                        .border_color(frame())
                        .font_family(MONO)
                        .text_size(px(12.5))
                        .placeholder(if found.first.is_empty() { "YYYY-MM-DD".to_string() } else { found.first.clone() })
                        .value(since)
                        .on_change(move |t: &str| since.set(t.to_string()))
                        .on_submit(move |t: &str| replan(Want { since: t.trim().to_string(), ..want.get_untracked() })),
                )
                .child(
                    text(move || format!("{} tool calls", if want.with(|w| w.tools) { "☑" } else { "☐" }))
                        .text_size(px(13.0))
                        .on_press(move || replan(Want { tools: !want.get_untracked().tools, ..want.get_untracked() })),
                ),
        )
        .child(
            view()
                .flex_row()
                .justify_end()
                .gap_2()
                .child(button(|| "Cancel".into(), false, move || ui.sheet.set(Sheet::None)))
                .child(button(
                    || "Import".into(),
                    true,
                    move || {
                        send(Command::Import(want.get_untracked()));
                        ui.note("Sending them to Acorn's memory…".into());
                    },
                )),
        )
}
