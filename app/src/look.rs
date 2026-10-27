//! Look
//! ====
//! Black ink on white paper (soft light ink on a soft black, dark): what
//! the machine wrote is filled (a block, a summary; a tool call), every
//! result is a file's lines, framed, what a person said is framed, what
//! Acorn said is bare. A call is told by its tool's emoji; a diff's lines
//! are green and red.

use nsh_core::prelude::*;

macro_rules! palette {
    ($($name:ident: $light:literal, $dark:literal; $doc:literal)*) => {
        $(
            #[doc = $doc]
            pub fn $name() -> AdaptiveColor {
                AdaptiveColor::rgb($light, $dark)
            }
        )*
    };
}

palette! {
    ink: 0x111111, 0xe3e3e6; "Text, frames."
    mid: 0x5f5f5f, 0xa6a6ad; "Secondary text, levels."
    soft: 0x9a9a9a, 0x8a8a92; "Hints, placeholders."
    hair: 0xe6e6e6, 0x36363c; "Rules."
    paper: 0xffffff, 0x171719; "The page."
    inset: 0xf7f7f7, 0x1d1d20; "The sidebar."
    lift: 0xffffff, 0x222226; "What floats over the page: the message box."
    block: 0xf2f2f2, 0x2c2c31; "A summary's, a tool's, a button's, code's fill."
    hover: 0xe8e8e8, 0x37373d; "Under the pointer."
    frame: 0xcccccc, 0x4c4c53; "A call's code's frame, a file's lines', the message box's."
    edge: 0x111111, 0x85858d; "A person's frame: the user's message, a report."
    add: 0x1a7f37, 0x55b874; "A diff's line added."
    del: 0xcf222e, 0xf8857c; "A diff's line dropped."
}

pub const SERIF: &str = "Iowan Old Style";
pub const BRAND: &str = "Avenir Next";
pub const MONO: &str = "Menlo";

/// The column the chat reads in.
pub const COL: f32 = 740.0;

/// Each tool's emoji; any other's is 🔧.
pub fn tool_glyph(name: &str) -> &'static str {
    match name {
        "zoom" => "🔍",
        "date" => "📅",
        "shell" => "⌨️",
        "read" => "📖",
        "write" => "📄",
        "edit" => "📝",
        "spawn" => "🤖",
        "tell" => "💬",
        "stop" => "✋",
        "create_assistant" => "🐿️",
        "list_assistants" | "search_assistants" => "👥",
        "ask_assistant" => "🗣️",
        "goal" => "🎯",
        "usage" => "💵",
        "import" => "📥",
        "export" => "💾",
        "tree" => "🌳",
        "computer" => "🖱️",
        _ => "🔧",
    }
}
