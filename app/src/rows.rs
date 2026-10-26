//! Rows
//! ====
//! The chat as drawn: a list of the model's items (a part each), each its
//! rows, then what streams. A block is a summary on the block's fill (a
//! click opens it into its halves); an open block, its bar over its
//! halves in a frame; the user's words in a bubble at the right; a report
//! framed, its agent's name on its edge; Acorn's words bare, as Markdown;
//! thinking and notes faint; a tool call on its fill, its emoji and name
//! first; a result as a file's lines, numbered, a diff's in green and red.

use crate::call::Call;
use crate::look::*;
use crate::model::{Act, Row};
use crate::text::{self, Out};
use crate::Ui;
use acorn_core::Kind;
use nsh_base::{TextViewLinkPressEvent, text_view};
use nsh_core::prelude::*;

/// The page shown, as a list that keeps to its end as it grows.
pub fn chat(ui: Ui) -> impl IntoElement {
    let items = computed(move || ui.model.with(|m| m.body()));
    let tail = computed(move || ui.model.with(|m| m.tail()));
    virtual_list(move || items.with(Vec::len), move |at| {
        let item = computed(move || items.with(|v| v.get(at.index()).cloned()));
        column().child(dynamic(move || item.get().unwrap_or_default().into_iter().map(|r| row(ui, r)).collect::<Vec<_>>()))
    })
    .footer(column().pb(px(28.0)).child(dynamic(move || tail.get().into_iter().map(|r| row(ui, r)).collect::<Vec<_>>())))
    .follow_end(px(120.0))
    .estimated_item_size(px(80.0))
    .content_padding_top(px(24.0))
    .flex_1()
}

/// A page's column of the chat, centered.
fn column() -> View {
    view().w_full().max_w(px(COL)).self_center().px_5().gap(px(14.0)).pt(px(14.0))
}

fn row(ui: Ui, it: Row) -> AnyElement {
    match it {
        Row::Cap(cap) => text(cap).font_family(SERIF).italic().text_size(px(14.5)).text_color(mid()).into_element(),
        Row::Use(spend) => text(move || text::use_text(&spend, ui.now.get()))
            .font_family(MONO)
            .text_size(px(12.0))
            .text_color(soft())
            .tabular_nums()
            .mt(px(-6.0))
            .tooltip(crate::hint(text::use_hint(&spend)))
            .into_element(),
        Row::Sum { seg, text: summary, call } => view()
            .flex_row()
            .gap_3()
            .px(px(14.0))
            .py(px(10.0))
            .rounded(px(10.0))
            .bg(block())
            .hover(|s| s.bg(hover()))
            .on_press(move || ui.act(Act::Open(seg)))
            .child(text(format!("{}x", seg.1)).min_w(px(38.0)).text_size(px(11.5)).line_height(px(24.0)).font_weight(FontWeight::SEMIBOLD).text_color(mid()))
            .child(match call {
                Some(call) => call_head(&call, true).flex_1().into_element(),
                None => text(summary).flex_1().font_family(SERIF).text_size(px(15.0)).line_height(px(23.0)).into_element(),
            })
            .into_element(),
        Row::Box { seg, text: bar, rows } => view()
            .border(px(1.25))
            .border_color(frame())
            .rounded(px(12.0))
            .px(px(10.0))
            .pb(px(10.0))
            .gap(px(10.0))
            .child(
                text(bar)
                    .py(px(6.0))
                    .text_size(px(12.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(mid())
                    .border_b_1()
                    .border_color(hair())
                    .on_press(move || ui.act(Act::Shut(seg))),
            )
            .child(rows.into_iter().map(|r| row(ui, r)).collect::<Vec<_>>())
            .into_element(),
        Row::Msg { kind, who, text: body, call, images } => message(ui, kind, who, body, call, images),
    }
}

fn message(ui: Ui, kind: Kind, who: String, body: String, call: Option<Call>, images: Vec<String>) -> AnyElement {
    let pics = (!images.is_empty()).then(|| pictures(&images));
    match kind {
        Kind::User => view()
            .self_end()
            .max_w(px(560.0))
            .px(px(17.0))
            .py(px(10.0))
            .border(px(1.25))
            .border_color(edge())
            .rounded(px(20.0))
            .rounded_br(px(6.0))
            .child(plain(ui, &body, 16.0))
            .child(pics)
            .into_element(),
        Kind::Work => view()
            .px_5()
            .pt(px(10.0))
            .pb(px(14.0))
            .gap_1()
            .border(px(1.25))
            .border_color(edge())
            .rounded(px(14.0))
            .child(text(who).text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(mid()))
            .child(plain(ui, &body, 16.0))
            .into_element(),
        Kind::Tool => {
            let call = call.unwrap_or(Call { name: String::new(), head: body, blocks: vec![] });
            view()
                .px(px(14.0))
                .py(px(9.0))
                .rounded(px(10.0))
                .bg(block())
                .gap_1()
                .child(call_head(&call, false))
                .child(call.blocks.iter().map(|b| framed(if b.k.is_empty() { None } else { Some(b.k.clone()) }, code(&b.x))).collect::<Vec<_>>())
                .into_element()
        }
        Kind::Echo => view().gap_2().child(text::output(&body).into_iter().map(file).collect::<Vec<_>>()).into_element(),
        Kind::Mind | Kind::Note => view()
            .gap_1()
            .child((!who.is_empty()).then(|| text(who).text_size(px(12.0)).font_weight(FontWeight::BOLD).text_color(soft())))
            .child(markdown(ui, &body).text_size(px(14.5)).line_height(px(23.0)).text_color(mid()).italic())
            .into_element(),
        Kind::Acorn => view().child(markdown(ui, &body).text_size(px(16.0)).line_height(px(25.0))).child(pics).into_element(),
    }
}

/// Markdown, each agent's name a link to its page.
fn markdown(ui: Ui, body: &str) -> nsh_base::TextViewElement {
    text_view()
        .markdown(text::link_names(body))
        .selectable(true)
        .text_color(ink())
        .on_link_press(move |e: &TextViewLinkPressEvent| match e.url.strip_prefix("agent:") {
            Some(name) => ui.act(Act::Goto(Some(name.to_string()))),
            None => {
                let _ = open_url(e.url.to_string());
            }
        })
}

/// Text as it is: paragraphs and lines, each agent's name a link.
fn plain(ui: Ui, body: &str, size: f32) -> impl IntoElement {
    let mut line = text("").text_size(px(size)).line_height(px(size * 1.55)).selectable(true);
    let mut rest = body;
    while let Some(at) = rest.find('[') {
        let tail = &rest[at + 1..];
        match tail.find(']').filter(|&close| acorn_core::names::is_name(&tail[..close])) {
            Some(close) => {
                let name = tail[..close].to_string();
                line = line.child(rest[..at].to_string());
                line = line.child(text(format!("[{name}]")).underline().on_press(move || ui.act(Act::Goto(Some(name.clone())))));
                rest = &tail[close + 1..];
            }
            None => {
                line = line.child(rest[..=at].to_string());
                rest = tail;
            }
        }
    }
    line.child(rest.to_string())
}

/// A tool call's head: its emoji, its name in bold, then the call.
fn call_head(call: &Call, folded: bool) -> Text {
    let head = if folded { crate::call::line(call) } else { call.head.clone() };
    text("")
        .font_family(MONO)
        .text_size(px(12.5))
        .line_height(px(20.0))
        .selectable(true)
        .child(text(format!("{}  ", tool_glyph(&call.name))).text_size(px(15.0)))
        .child(text(format!("{} ", call.name)).font_weight(FontWeight::BOLD))
        .child(head)
}

fn code(x: &str) -> Text {
    text(x.to_string()).font_family(MONO).text_size(px(12.5)).line_height(px(19.0)).selectable(true)
}

/// Lines in a frame, a name on its top edge.
fn framed(name: Option<String>, body: impl IntoElement) -> View {
    view()
        .border(px(1.25))
        .border_color(frame())
        .rounded(px(6.0))
        .px(px(10.0))
        .py(px(6.0))
        .gap_1()
        .child(name.map(|n| text(n).font_family(MONO).text_size(px(12.0)).text_color(ink()).font_weight(FontWeight::SEMIBOLD)))
        .child(body)
}

/// A result's file: its lines numbered, a diff's marked and colored.
fn file(out: Out) -> View {
    let number = |n: u64, width: usize| text(format!("{n:>width$} ")).text_color(soft());
    match out {
        Out::File { k, n, x } => {
            let width = (n + x.len() as u64).to_string().len();
            let mut lines = text("").font_family(MONO).text_size(px(12.5)).line_height(px(19.0)).selectable(true);
            for (k, line) in x.into_iter().enumerate() {
                lines = lines.child(number(n + k as u64, width)).child(format!("{line}\n"));
            }
            framed(Some(k), lines)
        }
        Out::Diff { k, x } => {
            let width = x.iter().flatten().map(|l| l.n).max().unwrap_or(1).to_string().len();
            let mut lines = text("").font_family(MONO).text_size(px(12.5)).line_height(px(19.0)).selectable(true);
            for (h, hunk) in x.into_iter().enumerate() {
                if h > 0 {
                    lines = lines.child(text(format!("{:>width$}\n", "⋯")).text_color(soft()));
                }
                for l in hunk {
                    let color = match l.s {
                        '+' => add(),
                        '-' => del(),
                        _ => mid(),
                    };
                    lines = lines.child(number(l.n, width)).child(text(format!("{}{}\n", l.s, l.x)).text_color(color));
                }
            }
            framed(Some(k), lines)
        }
    }
}

/// A message's images, each opened by a click.
fn pictures(names: &[String]) -> View {
    let dir = acorn_core::config::get().chat_dir().join("docs/images");
    view().flex_row().flex_wrap().gap_2().mt_2().child(
        names
            .iter()
            .map(|name| {
                let url = format!("file://{}", dir.join(name).display());
                let open = url.clone();
                image(url).h(px(120.0)).w(px(160.0)).resize_mode(ResizeMode::Cover).rounded(px(8.0)).border_1().border_color(hair()).on_press(move || {
                    let _ = open_url(open.clone());
                })
            })
            .collect::<Vec<_>>(),
    )
}

// Images to send
// ==============

/// The images waiting to go with the next message: a click drops one.
pub fn pending(images: Signal<Vec<Vec<u8>>>) -> impl IntoElement {
    when(move || images.with(|i| !i.is_empty()), move || {
        view().flex_row().gap_2().child(dynamic(move || {
            (0..images.with(Vec::len))
                .map(|k| {
                    view()
                        .px_3()
                        .py_1()
                        .rounded(px(8.0))
                        .bg(block())
                        .on_press(move || images.update(|i| drop(i.remove(k))))
                        .child(text(format!("🖼 image {} ✕", k + 1)).text_size(px(12.5)).text_color(mid()))
                })
                .collect::<Vec<_>>()
        }))
    })
}

/// The attach button: images picked from this Mac.
pub fn attach(images: Signal<Vec<Vec<u8>>>) {
    let request = nsh_core::picker::PickRequest::new(true, 20);
    spawn(async move {
        let picked = pick_images(request).await;
        images.update(|i| i.extend(picked.into_iter().map(|f| f.data.bytes().to_vec())));
    });
}

/// A paste's images join those waiting.
pub fn pasted(images: Signal<Vec<Vec<u8>>>, e: &PasteEvent) {
    let pics: Vec<Vec<u8>> = e.files.iter().filter(|f| f.mime.starts_with("image/")).map(|f| f.data.clone()).collect();
    if !pics.is_empty() {
        images.update(|i| i.extend(pics));
    }
}
