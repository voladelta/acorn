//! Computer
//! ========
//! The actions of the computer tool, Anthropic's computer use tool in
//! shape, run on this Mac. computer.swift, carried by Acorn and compiled
//! into its dir (swiftc) when missing or changed, does the mouse and
//! keyboard and tells the screenshot's size; screencapture and sips take
//! it as a JPEG. Every action answers with a screenshot taken after it;
//! zoom with its region. wait and hold_key wait here, so a stop ends them.
//! macOS asks for the Screen Recording and Accessibility permissions.

use crate::ai::{Out, Spec, nap};
use crate::{Img, config};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const SOURCE: &str = include_str!("computer.swift");

/// The longest wait or hold_key, in seconds.
pub const WAIT: f64 = 100.0;

/// How long the screen gets to show the last action before a screenshot.
const SETTLE: Duration = Duration::from_millis(500);

/// What computer.swift prints: the screenshot's size, its scale (screen
/// points per pixel), the action's answer.
#[derive(Deserialize)]
struct Done {
    w: f64,
    h: f64,
    scale: f64,
    text: String,
}

/// The tool of a computer task: its actions and fields as Anthropic's (so
/// the model does as it was trained).
pub fn spec() -> Spec {
    let px = json!({"type": "array", "items": {"type": "integer"}, "minItems": 2, "maxItems": 2});
    let with = |mut v: Value, description: &str| {
        v["description"] = json!(description);
        v
    };
    crate::machine::spec(
        "computer",
        "Use a mouse and keyboard to interact with the user's Mac, and take screenshots. Every action answers with a screenshot taken after it.
* This is an interface to a desktop GUI. Some applications may take time to start or process actions, so you may need to wait and take successive screenshots to see the results of your actions. E.g. if you click on Safari and a window doesn't open, try waiting and taking another screenshot.
* Coordinates are pixels of the screenshot (1280x800 at most), x from its left edge, y from its top edge.
* Whenever you intend to move the cursor to click on an element like an icon, you should consult a screenshot to determine the coordinates of the element before moving the cursor.
* If you tried clicking on a program or link but it failed to load, even after waiting, try adjusting your cursor position so that the tip of the cursor visually falls on the element that you want to click.
* Make sure to click any buttons, links, icons, etc with the cursor tip in the center of the element. Don't click boxes on their edges unless asked.",
        json!({
            "action": {"type": "string", "enum": ["key", "hold_key", "type", "cursor_position", "mouse_move", "left_mouse_down", "left_mouse_up", "left_click", "left_click_drag", "right_click", "middle_click", "double_click", "triple_click", "scroll", "wait", "screenshot", "zoom"], "description": "The action to perform. The available actions are:
* `key`: Press a key or key-combination on the keyboard. This supports xdotool's `key` syntax. Examples: \"a\", \"Return\", \"cmd+l\", \"ctrl+s\", \"Up\".
* `hold_key`: Hold down a key or multiple keys for a specified duration (in seconds). Supports the same syntax as `key`.
* `type`: Type a string of text on the keyboard.
* `cursor_position`: Get the current (x, y) pixel coordinate of the cursor on the screen.
* `mouse_move`: Move the cursor to a specified (x, y) pixel coordinate on the screen.
* `left_mouse_down`: Press the left mouse button.
* `left_mouse_up`: Release the left mouse button.
* `left_click`: Click the left mouse button at the specified (x, y) pixel coordinate on the screen. You can also include a key combination to hold down while clicking using the `text` parameter.
* `left_click_drag`: Click and drag the cursor from `start_coordinate` to a specified (x, y) pixel coordinate on the screen.
* `right_click`: Click the right mouse button at the specified (x, y) pixel coordinate on the screen.
* `middle_click`: Click the middle mouse button at the specified (x, y) pixel coordinate on the screen.
* `double_click`: Double-click the left mouse button at the specified (x, y) pixel coordinate on the screen.
* `triple_click`: Triple-click the left mouse button at the specified (x, y) pixel coordinate on the screen.
* `scroll`: Scroll the screen in a specified direction by a specified amount of clicks of the scroll wheel, at the specified (x, y) pixel coordinate. DO NOT use PageUp/PageDown to scroll.
* `wait`: Wait for a specified duration (in seconds).
* `screenshot`: Take a screenshot of the screen.
* `zoom`: View a region of the screen at full resolution."},
            "coordinate": with(px.clone(), "(x, y): The x (pixels from the left edge) and y (pixels from the top edge) coordinates to move the mouse to. Required by `action=mouse_move` and `action=left_click_drag`; optional for clicks and `scroll` (default: where the cursor is)."),
            "start_coordinate": with(px, "(x, y): The x (pixels from the left edge) and y (pixels from the top edge) coordinates to start the drag from. Required only by `action=left_click_drag`."),
            "text": {"type": "string", "description": "Required only by `action=type`, `action=key`, and `action=hold_key`. Can also be used by click or scroll actions to hold down keys while clicking or scrolling."},
            "scroll_direction": {"type": "string", "enum": ["up", "down", "left", "right"], "description": "The direction to scroll the screen. Required only by `action=scroll`."},
            "scroll_amount": {"type": "integer", "minimum": 0, "description": "The number of 'clicks' to scroll. Required only by `action=scroll`."},
            "duration": {"type": "number", "minimum": 0, "maximum": WAIT, "description": "The duration to hold the key down for, or to wait, in seconds. Required only by `action=hold_key` and `action=wait`."},
            "region": {"type": "array", "items": {"type": "integer"}, "minItems": 4, "maxItems": 4, "description": "(x0, y0, x1, y1): The top-left and bottom-right corners of the region to view. Required only by `action=zoom`."},
        }),
        &["action"],
    )
}

pub async fn act(input: &Value, cancel: CancellationToken) -> Out {
    match step(input, &cancel).await {
        Ok(out) => out,
        Err(err) => Out::fail(err.to_string()),
    }
}

async fn step(input: &Value, cancel: &CancellationToken) -> anyhow::Result<Out> {
    let name = input["action"].as_str().unwrap_or_default();
    if name == "zoom" {
        return zoom(input).await;
    }
    let time = Duration::from_secs_f64(input["duration"].as_f64().unwrap_or(0.0).clamp(0.0, WAIT));
    let done = match name {
        "wait" => {
            nap(time, cancel).await;
            hands(name, input).await?
        }
        "hold_key" => {
            let done = hands("press", input).await?;
            nap(time, cancel).await;
            hands("release", input).await?;
            done
        }
        _ => hands(name, input).await?,
    };
    tokio::time::sleep(SETTLE).await;
    let shot = shot(&[], done.w, done.h).await?;
    Ok(Out { text: format!("{}\n{}", done.text, shot.text), ..shot })
}

/// The region [x0, y0, x1, y1] of the screenshot, at the screen's own
/// resolution, as large as the screenshot.
async fn zoom(input: &Value) -> anyhow::Result<Out> {
    let done = hands("zoom", input).await?;
    let r: Vec<f64> = input["region"].as_array().into_iter().flatten().filter_map(|n| n.as_f64()).map(|n| n * done.scale).collect();
    let [x0, y0, x1, y1] = r[..] else { return Ok(Out::fail("region must be [x0, y0, x1, y1], with x1 > x0 and y1 > y0")) };
    if !(x1 > x0 && y1 > y0) {
        return Ok(Out::fail("region must be [x0, y0, x1, y1], with x1 > x0 and y1 > y0"));
    }
    let fit = (done.w / (x1 - x0)).min(done.h / (y1 - y0));
    let area = format!("{},{},{},{}", x0.round(), y0.round(), (x1 - x0).round(), (y1 - y0).round());
    shot(&["-R", &area], ((x1 - x0) * fit).round(), ((y1 - y0) * fit).round()).await
}

/// An action of computer.swift, compiled first if missing or changed.
async fn hands(name: &str, input: &Value) -> anyhow::Result<Done> {
    let dir = &config::get().dir;
    let (bin, src) = (dir.join("computer"), dir.join("computer.swift"));
    if !bin.exists() || std::fs::read_to_string(&src).ok().as_deref() != Some(SOURCE) {
        std::fs::create_dir_all(dir)?;
        std::fs::write(&src, SOURCE)?;
        exec("swiftc", &["-O", &src.to_string_lossy(), "-o", &bin.to_string_lossy()]).await?;
    }
    let arg = json!({"name": name, "input": input}).to_string();
    Ok(serde_json::from_str(&exec(&bin.to_string_lossy(), &[&arg]).await?)?)
}

/// The screen (or the region `opts` gives), scaled to w by h, as a JPEG.
async fn shot(opts: &[&str], w: f64, h: f64) -> anyhow::Result<Out> {
    let file = std::env::temp_dir().join(format!("acorn-{:016x}.jpg", rand::random::<u64>()));
    let path = file.to_string_lossy().to_string();
    let taken = async {
        let mut args = vec!["-x", "-t", "jpg"];
        args.extend(opts);
        args.push(&path);
        exec("screencapture", &args).await?;
        exec("sips", &["-z", &h.to_string(), &w.to_string(), &path]).await?;
        anyhow::Ok(std::fs::read(&file)?)
    }
    .await;
    let _ = std::fs::remove_file(&file);
    let data = base64::engine::general_purpose::STANDARD.encode(taken?);
    Ok(Out { text: format!("[image {w}x{h}]"), fail: false, imgs: vec![Img { mime: "image/jpeg".into(), data }] })
}

async fn exec(cmd: &str, args: &[&str]) -> anyhow::Result<String> {
    let out = tokio::process::Command::new(cmd).args(args).output().await?;
    let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if !out.status.success() {
        let why = if stderr.trim().is_empty() { stdout.trim() } else { stderr.trim() };
        anyhow::bail!("{}", if why.is_empty() { format!("{cmd}: {}", out.status) } else { why.to_string() });
    }
    Ok(stdout.trim().to_string())
}
