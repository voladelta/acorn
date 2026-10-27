//! Purges
//! ======
//! A message erased for good, and chat.db wiped of its old bytes.

use super::*;

impl Memory {
    /// Erases a message for good: the chat's message id (each of its
    /// pages), or message id of an agent's log (`agent`). Its text becomes
    /// PURGED, its line and images go, and, in the chat, every node built
    /// from it: those in a view open into their halves, and it and those
    /// above build anew (a call building any node now is not stored: the
    /// caller stops them). A wipe of chat.db is then owed. Answers what it
    /// erased.
    pub fn purge(&mut self, agent: Option<&str>, id: u64) -> Result<String> {
        let log = agent.unwrap_or(CHAT);
        let (mut a, mut b) = (id, id);
        match agent {
            Some(name) if self.read(name, id).is_none() => return Ok(format!("No message {id} in {name}'s chat.")),
            Some(_) => {}
            None if id >= self.count => return Ok(format!("No message {id}.")),
            None => {
                while a > 0 && self.message(a - 1).is_some_and(|m| m.more) {
                    a -= 1;
                }
                while b + 1 < self.count && self.message(b).is_some_and(|m| m.more) {
                    b += 1;
                }
            }
        }
        let mut gone = HashSet::new();
        if agent.is_none() {
            for i in a..=b {
                let mut part = Part::leaf(i);
                while part.span().1 < self.count {
                    if self.node(part).is_some() {
                        gone.insert(part.slot());
                    }
                    part = part.up();
                }
            }
        }
        let wipe = self.dir.join(WIPE);
        fs::write(&wipe, "")?;
        disk::wrote(&wipe);
        let mut images = Vec::new();
        let tx = self.db.transaction()?;
        for i in a..=b {
            let Some(mut root) = tx.query_row("select * from msgs where log = ? and i = ?", params![log, i], from_row).optional()? else { continue };
            images.append(&mut root.images);
            root.text = PURGED.into();
            root.line = None;
            root.size = root.show().len();
            tx.execute("update msgs set text = ?, images = '[]', line = null, size = ? where log = ? and i = ?", params![root.text, root.size, log, i])?;
        }
        for &slot in &gone {
            tx.execute("delete from nodes where slot = ?", [slot])?;
        }
        tx.commit()?;
        let mut said = match agent {
            Some(name) => format!("Purged message {id} of {name}'s chat."),
            None => {
                self.unbuild(a, b, &gone);
                let pages = if a == b { String::new() } else { format!(" (its pages {a} to {b})") };
                format!("Purged message {id}{pages} and the {} summaries made from it; they are being written anew without it.", gone.len())
            }
        };
        for name in images {
            let used = self.db.query_row("select exists (select 1 from msgs where images like '%' || ? || '%')", [&name], |r| r.get::<_, bool>(0));
            match used {
                Ok(true) => {}
                Ok(false) => {
                    let dir = self.images_dir();
                    for file in [dir.join(&name), dir.join(format!("{name}.jpg"))] {
                        let _ = fs::remove_file(file);
                    }
                }
                Err(err) => said += &format!(" Its image {name} was kept: {err}."),
            }
        }
        self.purged += 1;
        said += if self.stuck { " Its old bytes are wiped from Acorn's files when Acorn starts again." } else { " Its old bytes are wiped from Acorn's files once the chat is idle." };
        Ok(said)
    }

    /// A wipe done of the purges up to `upto`; none owed after, its marker
    /// goes.
    pub fn wiped(&mut self, upto: u64) {
        self.wiped = upto;
        if self.wiped == self.purged {
            let _ = fs::remove_file(self.dir.join(WIPE));
        }
    }

    /// The nodes `gone` (by slot) let go, the views' open into the halves
    /// left, the leaves a..=b and those above them to build anew.
    fn unbuild(&mut self, a: u64, b: u64, gone: &HashSet<u64>) {
        self.age += 1;
        self.nodes.retain(|slot, _| !gone.contains(slot));
        for i in a..=b {
            let mut part = Part::leaf(i).up();
            while part.span().1 < self.count {
                self.ready(part, false);
                part = part.up();
            }
            self.ready(Part::leaf(i), true);
        }
        for k in 0..2 {
            let view = if k == 0 { &self.view } else { &self.seen };
            let parts: Vec<Part> = view.parts.clone().into_iter().flat_map(|p| self.open_up(p, gone)).collect();
            let size = parts.iter().map(|&p| self.size(p)).sum();
            let view = if k == 0 { &mut self.view } else { &mut self.seen };
            (view.parts, view.size) = (parts, size);
        }
        self.fit(false);
        if let Err(err) = self.save() {
            eprintln!("memory: state: {err}");
        }
    }

    /// A view's line, or, if gone, the lines under it left, kept.
    fn open_up(&mut self, part: Part, gone: &HashSet<u64>) -> Vec<Part> {
        match part.halves() {
            Some(halves) if gone.contains(&part.slot()) => halves.into_iter().flat_map(|h| self.open_up(h, gone)).collect(),
            _ => {
                if let Some(text) = self.node(part) {
                    self.nodes.insert(part.slot(), text);
                }
                vec![part]
            }
        }
    }
}

/// Deleted rows erased from a db's file: rewritten whole (secure_delete
/// zeroes only cells and pages freed, not old bytes left in a page still
/// used), its WAL truncated. It takes as long as the db is big (2 s for
/// 450 MB) and holds up every write meanwhile.
pub fn erase(db: &Connection) -> Result<()> {
    db.execute_batch("vacuum")?;
    let busy: i64 = db.query_row("pragma wal_checkpoint(truncate)", [], |r| r.get(0))?;
    anyhow::ensure!(busy == 0, "its WAL is in use");
    Ok(())
}

/// The chat in `dir` wiped (`erase`), on a connection of its own: blocking.
pub fn wipe(dir: &Path) -> Result<()> {
    let db = Connection::open(dir.join("chat.db"))?;
    db.busy_timeout(std::time::Duration::from_secs(60))?;
    erase(&db)
}
