//! A chat's memory, as Acorn reads (dump) or writes (write) it, to check
//! it against UniiChat's: `compat write|dump <dir>`.

use acorn_core::memory::{self, Memory, Msg, Part};
use acorn_core::Kind;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (mode, dir) = (&args[1], std::path::Path::new(&args[2]));
    let mut mem = Memory::load(dir)?;
    if mode == "write" {
        let kinds = [Kind::User, Kind::Acorn, Kind::Tool, Kind::Echo, Kind::Work, Kind::Note];
        let mut seed: u64 = 7;
        for k in 0..300 {
            seed = (seed * 75 + 74) % 65537;
            let n = (seed as f64 / 65537.0 * 1500.0) as usize;
            let text = format!("é∑ {k} {}{}", "x".repeat(n), if k % 7 == 0 { "\nline two\n\n  three" } else { "" });
            mem.push(vec![Msg { imported: k % 11 == 0, ..Msg::new(kinds[k % kinds.len()], text) }])?;
        }
        mem.push(vec![Msg::new(Kind::User, "y".repeat(70000))])?;
        let mut l = 0;
        while (1u64 << l) <= mem.count {
            let mut i = 0;
            while (i + 1) << l <= mem.count {
                let part = Part::new(l, i);
                if mem.node(part).is_none() && let Some(src) = mem.source(part) {
                    let text = if src.len() <= memory::NODE { src } else { format!("L{l}#{i}: {}", memory::cut(&memory::flat(&src)).chars().take(80).collect::<String>()) };
                    mem.store(&[(part, text)])?;
                }
                i += 1;
            }
            l += 1;
        }
        mem.fit(false);
        mem.save()?;
    }
    let mut msgs = Vec::new();
    for i in 0..mem.count {
        let m = mem.message(i).unwrap();
        msgs.push(serde_json::json!([m.i, m.kind, m.text, m.size, m.more, m.imported]));
    }
    let mut nodes = Vec::new();
    for l in 0..mem.levels() {
        for (p, t) in mem.level(l) {
            nodes.push(serde_json::json!([p.l, p.i, t]));
        }
    }
    let view: Vec<_> = mem.view.parts.iter().map(|p| (p.l, p.i)).collect();
    let seen: Vec<_> = mem.seen.parts.iter().map(|p| (p.l, p.i)).collect();
    let out = serde_json::json!({"count": mem.count, "view": view, "size": mem.view.size, "seen": seen, "msgs": msgs, "nodes": nodes, "zoom": mem.zoom(256, 32, 0), "turn": mem.turn()});
    println!("{out}");
    Ok(())
}
