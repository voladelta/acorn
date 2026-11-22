//! Import pages are staged on the chat's connection, then published into
//! the indexed log together. `more` belongs to each original message.

use super::*;

impl Memory {
  /// Old imports used 'more' for the whole batch. Each published page
  /// stays individually addressable. Move its restart record alongside
  /// the messages, after load has discarded the unfinished tail.
  pub(super) fn migrate_import(&mut self) -> Result<()> {
    if self.db.query_row("pragma user_version", [], |r| r.get::<_, u32>(0))? != 0 {
      return Ok(());
    }
    #[derive(Deserialize)]
    struct Legacy {
      a: u64,
      b: u64,
      said: u64,
      spent: f64,
      why: Option<String>,
    }
    let file = self.dir.join("import.json");
    let legacy = match fs::read(&file) {
      Ok(data) => serde_json::from_slice::<Legacy>(&data)
        .map_err(|e| eprintln!("acorn: old import record: {e}"))
        .ok(),
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
      Err(e) => return Err(e.into()),
    };
    let tx = self.db.transaction()?;
    tx.execute("update msgs set more = 0 where log = '' and imported and more", [])?;
    if let Some(k) = &legacy
      && k.a < k.b
      && k.b <= self.count
    {
      tx.execute(
        "insert into import_batch values (1,?,?,?,?,?)",
        params![k.a, k.b, k.said, k.spent, k.why.as_deref() == Some("paused")],
      )?;
    }
    tx.pragma_update(None, "user_version", 1)?;
    tx.commit()?;
    if legacy.is_some() {
      let _ = fs::remove_file(file);
    }
    Ok(())
  }

  /// Stage incoming import pages outside the log. SQLite discards TEMP
  /// at disconnect, so a crash before publication leaves no partial chat.
  pub(crate) fn begin_import(&self) -> Result<()> {
    self.db.execute_batch(
      "drop table if exists temp.import_pages;
      create temp table import_pages (kind text, text text, date text, more integer, size integer)",
    )?;
    Ok(())
  }

  /// Let go of the pages still staged: those of an import that failed.
  pub(crate) fn end_import(&self) {
    if let Err(err) = self.db.execute_batch("drop table if exists temp.import_pages") {
      eprintln!("memory: staged import: {err}");
    }
  }

  /// Keep just the page sizes for the batch estimate as the text goes to disk.
  pub(crate) fn stage_import(&mut self, msgs: Vec<crate::import::Msg>) -> Result<Vec<usize>> {
    let mut sizes = Vec::with_capacity(msgs.len());
    let tx = self.db.transaction()?;
    {
      let mut insert = tx.prepare_cached("insert into temp.import_pages values (?, ?, ?, ?, ?)")?;
      for m in msgs {
        let page = root(
          0,
          Msg {
            date: Some(m.date),
            more: m.more,
            imported: true,
            ..Msg::new(m.kind, m.text)
          },
        );
        insert.execute(params![page.kind.as_str(), page.text, page.date, page.more, page.size])?;
        sizes.push(page.size);
      }
    }
    tx.commit()?;
    Ok(sizes)
  }

  pub(crate) fn finish_import(&mut self, said: u64) -> Result<()> {
    anyhow::ensure!(self.open == 0, "a chat message is still open");
    let tx = self.db.transaction()?;
    let n = tx.execute(
      "insert into msgs (log,i,kind,text,date,images,imported,line,more,size)
        select '', ?1 + rowid - 1, kind, text, date, '[]', 1, null, more, size from temp.import_pages",
      [self.count],
    )?;
    tx.execute(
      "insert into import_batch values (1,?,?,?,0,0)",
      params![self.count, self.count + n as u64, said],
    )?;
    tx.execute_batch("drop table temp.import_pages")?;
    tx.commit()?;
    self.sides.extend((self.count..self.count + n as u64).map(|i| (i, 1)));
    self.publish(n as u64);
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn page(text: &str, more: bool) -> crate::import::Msg {
    crate::import::Msg {
      kind: Kind::User,
      text: text.into(),
      date: iso_now(),
      more,
    }
  }

  #[test]
  fn only_a_published_import_is_visible_and_purge_follows_message_boundaries() {
    let dir = tempfile::tempdir().unwrap();
    let mut mem = Memory::load(dir.path()).unwrap();
    mem.begin_import().unwrap();
    mem.stage_import(vec![page("abandoned", false)]).unwrap();
    drop(mem);

    let mut mem = Memory::load(dir.path()).unwrap();
    assert_eq!(mem.count, 0);
    mem.begin_import().unwrap();
    mem
      .stage_import(vec![page("first", false), page("middle", true)])
      .unwrap();
    mem
      .stage_import(vec![page("continued", false), page("last", false)])
      .unwrap();
    assert!(mem.holding("middle").is_none());
    mem.db.execute_batch("create temp trigger interrupt before insert on msgs when new.i = 2 begin select raise(abort, 'interrupted'); end").unwrap();
    assert!(mem.finish_import(3).is_err());
    assert_eq!(mem.count, 0);
    assert!(mem.holding("first").is_none());
    mem.db.execute_batch("drop trigger interrupt").unwrap();
    mem.finish_import(3).unwrap();
    drop(mem);

    let mut mem = Memory::load(dir.path()).unwrap();
    assert_eq!(mem.count, 4);
    assert_eq!(
      mem
        .db
        .query_row("select a,b,said from import_batch", [], |r| Ok((
          r.get::<_, u64>(0)?,
          r.get::<_, u64>(1)?,
          r.get::<_, u64>(2)?
        )))
        .unwrap(),
      (0, 4, 3)
    );
    assert!(mem.search(CHAT, "continued", 1).starts_with("2|"));
    mem.purge(None, 2).unwrap();
    let texts = |mem: &Memory| (0..mem.count).map(|i| mem.message(i).unwrap().text).collect::<Vec<_>>();
    assert_eq!(texts(&mem), ["first", PURGED, PURGED, "last"]);
    mem.unpurge(None, 1, &[]).unwrap();
    assert_eq!(texts(&mem), ["first", "middle", "continued", "last"]);
  }

  #[test]
  fn old_imports_keep_each_published_page_and_discard_an_interrupted_batch() {
    let dir = tempfile::tempdir().unwrap();
    let mem = Memory::load(dir.path()).unwrap();
    mem
      .db
      .execute_batch(
        "pragma user_version = 0;
       insert into msgs values ('',0,'user','first','','[]',1,null,1,11),
                               ('',1,'user','last','','[]',1,null,0,10),
                               ('',2,'user','unfinished','','[]',1,null,1,16)",
      )
      .unwrap();
    drop(mem);
    fs::write(
      dir.path().join("import.json"),
      r#"{"a":0,"b":2,"said":2,"spent":0.125,"why":"paused"}"#,
    )
    .unwrap();
    let mut mem = Memory::load(dir.path()).unwrap();
    assert_eq!(mem.count, 2);
    assert_eq!(
      mem
        .db
        .query_row(
          "select spent from import_batch where paused and a = 0 and b = 2",
          [],
          |r| r.get::<_, f64>(0)
        )
        .unwrap(),
      0.125
    );
    assert!(mem.holding("unfinished").is_none());
    mem.purge(None, 1).unwrap();
    assert_eq!(mem.message(0).unwrap().text, "first");
  }

  #[test]
  fn reload_keeps_seen_at_its_frontier_while_import_is_unbuilt() {
    let dir = tempfile::tempdir().unwrap();
    let mut mem = Memory::load(dir.path()).unwrap();
    mem
      .push(vec![Msg::new(Kind::User, "chat 0"), Msg::new(Kind::Acorn, "chat 1")])
      .unwrap();
    mem
      .store(&[(Part::new(0, 0), "0".into()), (Part::new(0, 1), "1".into())])
      .unwrap();
    mem.fit(false);
    assert_eq!(mem.seen.end(), 2);
    mem.begin_import().unwrap();
    mem
      .stage_import(vec![
        crate::import::Msg {
          kind: Kind::User,
          text: "imported 0".into(),
          date: "2026-01-01T00:00:00Z".into(),
          more: false,
        },
        crate::import::Msg {
          kind: Kind::Acorn,
          text: "imported 1".into(),
          date: "2026-01-01T00:01:00Z".into(),
          more: false,
        },
      ])
      .unwrap();
    mem.finish_import(2).unwrap();
    mem.fit(false);
    assert_eq!(mem.seen.end(), 2);
    drop(mem);
    let mem = Memory::load(dir.path()).unwrap();
    assert_eq!(mem.count, 4);
    assert_eq!(mem.seen.end(), 2);
  }

  #[test]
  fn reload_caches_nodes_past_seen_when_view_already_merged_them() {
    let dir = tempfile::tempdir().unwrap();
    let mut mem = Memory::load(dir.path()).unwrap();
    mem
      .push(vec![Msg::new(Kind::User, "chat 0"), Msg::new(Kind::Acorn, "chat 1")])
      .unwrap();
    mem
      .store(&[(Part::new(0, 0), "0".into()), (Part::new(0, 1), "1".into())])
      .unwrap();
    mem.fit(false);
    mem.begin_import().unwrap();
    mem
      .stage_import(vec![
        crate::import::Msg {
          kind: Kind::User,
          text: "imported 2".into(),
          date: "2026-01-01T00:00:00Z".into(),
          more: false,
        },
        crate::import::Msg {
          kind: Kind::Acorn,
          text: "imported 3".into(),
          date: "2026-01-01T00:01:00Z".into(),
          more: false,
        },
      ])
      .unwrap();
    mem.finish_import(2).unwrap();
    mem
      .push(vec![Msg::new(Kind::User, "chat 4"), Msg::new(Kind::Acorn, "chat 5")])
      .unwrap();
    mem
      .store(&[
        (Part::new(0, 4), "summary 4".into()),
        (Part::new(0, 5), "summary 5".into()),
        (Part::new(1, 2), "merged 4-5".into()),
      ])
      .unwrap();
    // Force chat view to merge (1, 2) while seen stays at 2 due to unbuilt import 2..4.
    mem.view.high = 1;
    mem.fit(true);
    assert!(mem.view.parts.contains(&Part::new(1, 2)));
    assert_eq!(mem.seen.end(), 2);
    drop(mem);

    let mut mem = Memory::load(dir.path()).unwrap();
    assert_eq!(mem.seen.end(), 2);
    assert!(mem.built(Part::leaf(4)));
    assert!(mem.built(Part::leaf(5)));
    assert!(mem.built(Part::new(1, 2)));

    mem
      .store(&[
        (Part::new(0, 2), "summary 2".into()),
        (Part::new(0, 3), "summary 3".into()),
      ])
      .unwrap();
    mem.fit(false);
    assert_eq!(mem.seen.end(), 6);
    let blocks: String = mem
      .compaction_blocks(Part::leaf(6))
      .into_iter()
      .map(|b| b.text)
      .collect();
    assert!(blocks.contains("summary 4"));
    assert!(blocks.contains("summary 5"));
  }
}
