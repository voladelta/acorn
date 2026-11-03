//! Side
//! ====
//! The sidebar, as the nsh gallery's: Acorn's name, then the assistants
//! (Acorn, Max, then those they created, oldest first: `create_assistant`),
//! each a row, who it is in its tooltip; the one you talk to has a bar and
//! a fill, one at work (with you, or answering another) a dot. Cmd+B (or
//! the button atop it) hides it.

use crate::Ui;
use crate::look::*;
use acorn_core::assistant::Assistant;
use nsh_core::prelude::*;

/// Its width, shown.
const WIDTH: f32 = 232.0;

pub fn bar(ui: Ui) -> impl IntoElement {
  when(
    move || ui.side.get(),
    move || {
      view()
        .w(px(WIDTH))
        .flex_shrink_0()
        .bg(inset())
        .border_r_1()
        .border_color(hair())
        .child(
          view()
            .flex_row()
            .items_center()
            .gap_2()
            .h(px(52.0))
            .pl_5()
            .pr_3()
            .border_b_1()
            .border_color(hair())
            .child(text("🌰").text_size(px(20.0)))
            .child(
              text("Acorn")
                .font_family(BRAND)
                .text_size(px(20.0))
                .font_weight(FontWeight::SEMIBOLD),
            )
            .child(view().flex_1())
            .child(crate::side_chip(ui)),
        )
        .child(
          scroll().flex_1().child(
            view()
              .px_3()
              .pb_4()
              .gap(px(2.0))
              .child(
                text("ASSISTANTS")
                  .text_size(px(11.0))
                  .font_weight(FontWeight::SEMIBOLD)
                  .letter_spacing(px(0.6))
                  .text_color(soft())
                  .px(px(10.0))
                  .pt_3()
                  .pb(px(6.0)),
              )
              .child(dynamic(move || {
                ui.assistants.get().into_iter().map(|a| row(ui, a)).collect::<Vec<_>>()
              })),
          ),
        )
    },
  )
}

/// An assistant: its mark (Acorn's, an acorn; another's, its initial),
/// its name (the one you talk to in ink, the rest muted), a dot while it
/// works.
fn row(ui: Ui, a: Assistant) -> impl IntoElement {
  let id = a.id.clone();
  let on = {
    let id = id.clone();
    move || ui.talk.get() == id
  };
  let works = {
    let id = id.clone();
    move || ui.working.with(|w| w.iter().any(|chat| crate::link::whose(chat) == id))
  };
  let acorn = ui
    .assistants
    .with_untracked(|all| all.first().is_some_and(|f| f.id == a.id));
  let mark = if acorn {
    text("🌰").text_size(px(15.0)).into_element()
  } else {
    let initial = a
      .name
      .chars()
      .next()
      .map(|c| c.to_uppercase().to_string())
      .unwrap_or_default();
    text(initial)
      .text_size(px(12.0))
      .font_weight(FontWeight::SEMIBOLD)
      .text_color(mid())
      .into_element()
  };
  let tip = if a.about.is_empty() {
    a.name.clone()
  } else {
    format!("{} · {}", a.name, a.about)
  };
  let (on2, on3, on4) = (on.clone(), on.clone(), on.clone());
  view()
    .flex_row()
    .items_center()
    .gap(px(10.0))
    .h(px(36.0))
    .px(px(10.0))
    .rounded(px(8.0))
    .bg(move || if on() { block() } else { inset() })
    .hover(|s| s.bg(hover()))
    .cursor_pointer()
    .tooltip(crate::hint(tip))
    .on_press(move || {
      if ui.talk.get_untracked() == id {
        ui.back()
      } else {
        ui.talk_to(id.clone())
      }
    })
    .child(
      view()
        .absolute()
        .left(px(0.0))
        .top(px(9.0))
        .w(px(3.0))
        .h(px(18.0))
        .rounded(px(1.5))
        .bg(ink())
        .opacity(move || if on2() { 1.0_f32 } else { 0.0 }),
    )
    .child(
      view()
        .size(px(22.0))
        .rounded(px(6.0))
        .bg(move || if on3() { paper() } else { block() })
        .items_center()
        .justify_center()
        .child(mark),
    )
    .child(
      text(a.name)
        .flex_1()
        .number_of_lines(1)
        .text_size(px(14.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(move || if on4() { ink() } else { mid() }),
    )
    .child(
      view()
        .size(px(7.0))
        .rounded_full()
        .bg(ink())
        .opacity(move || if works() { 1.0_f32 } else { 0.0 }),
    )
}
