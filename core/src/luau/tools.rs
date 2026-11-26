use super::*;

/// A url as a call's log shows it: no query or credentials, which may hold a
/// key.
pub(super) fn bare(url: &str) -> String {
  let Ok(mut url) = reqwest::Url::parse(url) else {
    return "(a bad url)".into();
  };
  url.set_query(None);
  url.set_fragment(None);
  let _ = url.set_username("");
  let _ = url.set_password(None);
  url.to_string().trim_end_matches('/').to_string()
}

/// A field of an options table, if given.
fn opt<T: mlua::FromLua>(opts: Option<&Table>, k: &str) -> mlua::Result<Option<T>> {
  opts.map_or(Ok(None), |t| t.get(k))
}

/// JSON as Luau: a null is no field at all, not a value to trip on.
fn no_nulls() -> mlua::serde::SerializeOptions {
  mlua::serde::SerializeOptions::new()
    .serialize_none_to_null(false)
    .serialize_unit_to_null(false)
}

/// relevant's split option: nil or "lines", "paragraphs", "sections", or a
/// number of lines a window.
fn split(v: &Value) -> Result<decide::Split, String> {
  let word = |s: &mlua::LuaString| s.to_string_lossy();
  match v {
    Value::Nil => Ok(decide::Split::Lines),
    Value::String(s) if word(s) == "lines" => Ok(decide::Split::Lines),
    Value::String(s) if word(s) == "paragraphs" => Ok(decide::Split::Paragraphs),
    Value::String(s) if word(s) == "sections" => Ok(decide::Split::Sections),
    Value::Integer(n) if *n >= 1 => Ok(decide::Split::Window(*n as usize)),
    Value::Number(n) if *n >= 1.0 => Ok(decide::Split::Window(*n as usize)),
    _ => Err("relevant: split is \"lines\", \"paragraphs\", \"sections\" or a number of lines".into()),
  }
}

/// A tool's work on the disk, done as its call is made. On the runtime's
/// workers their other tasks move to another thread meanwhile. It isn't
/// handed to a blocking thread: the call would have to wait, which Luau
/// can't everywhere (a sort's order, a metamethod), and the work would go
/// on after a call given up.
fn blocking<T>(f: impl FnOnce() -> T) -> T {
  use tokio::runtime::{Handle, RuntimeFlavor};
  match Handle::try_current().map(|h| h.runtime_flavor()) {
    Ok(RuntimeFlavor::MultiThread) => tokio::task::block_in_place(f),
    _ => f(),
  }
}

/// Whose tools they are: the dir they act in, and the run their calls are
/// named in.
#[derive(Clone)]
struct Caller {
  dir: PathBuf,
  run: Arc<Mutex<Run>>,
}

/// What came of a tool's call, as Luau gets it (`Call::end`).
type Told = mlua::Result<(bool, Value)>;

/// The machine's tools and the web's, as globals of the VM, in `cwd`.
pub(super) fn tools(lua: &Lua, cwd: &Path, run: &Arc<Mutex<Run>>) -> mlua::Result<()> {
  let caller = Caller {
    dir: cwd.to_path_buf(),
    run: run.clone(),
  };
  set(lua, &caller, "__read", read)?;
  set(lua, &caller, "__write", write)?;
  set(lua, &caller, "__edit", edit)?;
  set(lua, &caller, "__sh", sh)?;
  set(lua, &caller, "__view", view)?;
  set(lua, &caller, "__ocr", ocr_text)?;
  lua.globals().set("__secret", lua.create_function(secret)?)?;
  set(lua, &caller, "__http", http)?;
  set(lua, &caller, "__web_search", web_search)?;
  set(lua, &caller, "__web_fetch", web_fetch)?;
  set(lua, &caller, "__decide", decide_all)?;
  set(lua, &caller, "__relevant", relevant)
}

/// A tool as a global of the VM: each call of it gets its caller.
fn set<A, F, Fut>(lua: &Lua, caller: &Caller, name: &str, tool: F) -> mlua::Result<()>
where
  A: mlua::FromLuaMulti,
  F: Fn(Lua, Caller, A) -> Fut + Send + 'static,
  Fut: Future<Output = Told> + Send + 'static,
{
  let caller = caller.clone();
  let tool = lua.create_async_function(move |lua, args| tool(lua, caller.clone(), args))?;
  lua.globals().set(name, tool)
}

/// A web tool's output as a call's end takes it.
fn told(out: Out) -> Result<String, String> {
  if out.fail { Err(out.text) } else { Ok(out.text) }
}

async fn read(lua: Lua, who: Caller, (path, opts): (String, Option<Table>)) -> Told {
  let (offset, limit) = (
    opt::<usize>(opts.as_ref(), "offset")?,
    opt::<usize>(opts.as_ref(), "limit")?,
  );
  let call = Call::start(&who.run, "read", &path);
  call.end(&lua, blocking(|| machine::text(&who.dir, &path, offset, limit)))
}

async fn write(lua: Lua, who: Caller, (path, content): (String, mlua::LuaString)) -> Told {
  let content = content.to_str()?.to_string();
  let call = Call::start(&who.run, "write", &path);
  let bytes = content.len();
  let input = json!({"file_path": path, "content": content});
  let got = blocking(|| machine::write(&who.dir, &input));
  call.end(&lua, got.map(|_| bytes))
}

async fn edit(
  lua: Lua,
  who: Caller,
  (path, old, new, opts): (String, mlua::LuaString, mlua::LuaString, Option<Table>),
) -> Told {
  let input = json!({
    "file_path": path,
    "old_string": &*old.to_str()?,
    "new_string": &*new.to_str()?,
    "replace_all": opt::<bool>(opts.as_ref(), "all")?.unwrap_or(false),
  });
  let call = Call::start(&who.run, "edit", &path);
  call.end(&lua, blocking(|| machine::edit(&who.dir, &input)))
}

async fn sh(lua: Lua, who: Caller, (cmd, opts): (String, Option<Table>)) -> Told {
  let secs = opt::<f64>(opts.as_ref(), "timeout")?
    .filter(|t| !t.is_nan())
    .unwrap_or(SH_TIMEOUT)
    .clamp(1.0, SH_TIMEOUT_MAX);
  let workdir = opt::<String>(opts.as_ref(), "cwd")?.map_or_else(|| who.dir.clone(), |d| who.dir.join(d));
  let call = Call::start(&who.run, "sh", &cmd);
  let cancel = call.cancel.clone();
  let got = match machine::shell(&cmd, &workdir, secs * 1000.0, cancel).await {
    Ok(ran) => {
      let ((stdout, stderr), (full, full_err)) = (ran.tails(), ran.spills());
      let t = lua.create_table()?;
      t.set("stdout", stdout)?;
      t.set("stderr", stderr)?;
      t.set("full", full)?;
      t.set("full_err", full_err)?;
      t.set("code", ran.code())?;
      t.set("timed_out", ran.timed_out)?;
      t.set("truncated", ran.truncated())?;
      Ok(t)
    }
    Err(why) => Err(why),
  };
  call.end(&lua, got)
}

async fn view(lua: Lua, who: Caller, path: String) -> Told {
  let call = Call::start(&who.run, "view", &path);
  let file = who.dir.join(&path);
  let got = (|| {
    if who.run.lock().imgs.len() >= PICS {
      return Err(format!("view {path}: at most {PICS} images a call"));
    }
    let (img, (w, h)) = blocking(|| {
      let meta = std::fs::metadata(&file).map_err(|e| e.to_string())?;
      if !meta.is_file() {
        return Err("not a regular file".to_string());
      }
      if meta.len() > VIEW_BYTES as u64 {
        return Err(format!("{} bytes, over {} MB", meta.len(), VIEW_BYTES >> 20));
      }
      let bytes = std::fs::read(&file).map_err(|e| e.to_string())?;
      image_for_model(&bytes).map_err(|e| e.to_string())
    })
    .map_err(|e| format!("view {path}: {e}"))?;
    who.run.lock().imgs.push(img);
    Ok(format!("{path}: {w}×{h}, attached: you see it after this call"))
  })();
  call.end(&lua, got)
}

async fn ocr_text(lua: Lua, who: Caller, path: String) -> Told {
  let call = Call::start(&who.run, "ocr", &path);
  let cancel = call.cancel.clone();
  let meter = who.run.lock().meter.clone();
  let got = ocr::read(&who.dir.join(&path), meter.as_ref(), &cancel).await;
  call.end(&lua, got.map_err(|e| format!("ocr {e}")))
}

fn secret(lua: &Lua, name: String) -> Told {
  Ok(match config::secret(&name) {
    Ok(key) => (true, key.into_lua(lua)?),
    Err(why) => (false, why.into_lua(lua)?),
  })
}

async fn http(lua: Lua, who: Caller, req: Table) -> Told {
  let url: String = req.get("url")?;
  if !(url.starts_with("http://") || url.starts_with("https://")) {
    return Err(mlua::Error::runtime("http: give an http or https url"));
  }
  let pairs = |k: &str| -> mlua::Result<Vec<(String, String)>> {
    Ok(match req.get::<Option<Table>>(k)? {
      Some(t) => t.pairs::<String, String>().collect::<mlua::Result<_>>()?,
      None => Vec::new(),
    })
  };
  let (query, mut headers) = (pairs("query")?, pairs("headers")?);
  let body = match req.get::<Value>("json")? {
    Value::Nil => req
      .get::<Option<mlua::LuaString>>("body")?
      .map(|b| b.as_bytes().to_vec()),
    v => {
      headers.push(("content-type".into(), "application/json".into()));
      Some(lua.from_value::<Json>(v)?.to_string().into_bytes())
    }
  };
  let method = req.get::<Option<String>>("method")?;
  let method = method.unwrap_or_else(|| if body.is_some() { "POST" } else { "GET" }.into());
  let timeout = req
    .get::<Option<f64>>("timeout")?
    .filter(|t| !t.is_nan())
    .map_or(web::TIMEOUT, |t| Duration::from_secs_f64(t.clamp(1.0, SH_TIMEOUT_MAX)));
  let save = req.get::<Option<String>>("save")?;
  let call = Call::start(&who.run, "http", &bare(&url));
  let cancel = call.cancel.clone();
  let sent = web::Req {
    method,
    url,
    query,
    headers,
    body,
    timeout,
  };
  let to = save.as_ref().map(|path| who.dir.join(path));
  // Before a request that may cost money, not after.
  if let Some(to) = to.as_ref().filter(|to| to.symlink_metadata().is_ok()) {
    return call.end(
      &lua,
      Err::<Table, _>(format!("http: {} is already there", to.display())),
    );
  }
  let got = async {
    let said = |e: mlua::Error| e.to_string();
    let res = web::request(&sent, &cancel).await?;
    let t = lua.create_table().map_err(said)?;
    t.set("status", res.status().as_u16()).map_err(said)?;
    if let Some(to) = &to {
      let bytes = web::save(res, to, &cancel).await?;
      t.set("saved", save.clone()).map_err(said)?;
      t.set("bytes", bytes).map_err(said)?;
      return Ok(t);
    }
    let bytes = web::read(res, &cancel).await?;
    if let Ok(v) = serde_json::from_slice::<Json>(&bytes) {
      t.set("json", lua.to_value_with(&v, no_nulls()).map_err(said)?)
        .map_err(said)?;
    }
    t.set("text", lua.create_string(&bytes).map_err(said)?).map_err(said)?;
    Ok(t)
  }
  .await;
  call.end(&lua, got)
}

async fn web_search(lua: Lua, who: Caller, (query, opts): (String, Option<Table>)) -> Told {
  let mut input = json!({"query": query});
  if let Some(n) = opt::<u64>(opts.as_ref(), "count")? {
    input["count"] = n.into();
  }
  if let Some(q) = opt::<String>(opts.as_ref(), "question")? {
    input["question"] = q.into();
  }
  let call = Call::start(&who.run, "web_search", &query);
  let cancel = call.cancel.clone();
  let out = web::run("web_search", &input, cancel).await;
  call.end(&lua, told(out))
}

async fn web_fetch(lua: Lua, who: Caller, (url, opts): (String, Option<Table>)) -> Told {
  let mut input = json!({"url": url});
  if let Some(at) = opt::<u64>(opts.as_ref(), "at")? {
    input["at"] = at.into();
  }
  if let Some(fresh) = opt::<bool>(opts.as_ref(), "fresh")? {
    input["fresh"] = fresh.into();
  }
  if let Some(version) = opt::<String>(opts.as_ref(), "version")? {
    input["version"] = version.into();
  }
  let call = Call::start(&who.run, "web_fetch", &url);
  let cancel = call.cancel.clone();
  let out = web::run("web_fetch", &input, cancel).await;
  call.end(&lua, told(out))
}

async fn decide_all(lua: Lua, who: Caller, (state, questions): (Value, Value)) -> Told {
  let state: Json = lua.from_value(state)?;
  let questions: Json = lua.from_value(questions)?;
  let names: Vec<_> = questions
    .as_object()
    .map(|q| q.keys().cloned().collect())
    .unwrap_or_default();
  let call = Call::start(&who.run, "decide", &names.join(", "));
  let cancel = call.cancel.clone();
  let got = match decide::ask(&state, &questions, &cancel).await {
    Ok(answers) => lua.to_value_with(&answers, no_nulls()).map_err(|e| e.to_string()),
    Err(why) => Err(why),
  };
  call.end(&lua, got)
}

/// relevant's items and keep option, as given: a list of strings, or a
/// string split as its split option says.
fn relevant_given(items: Value, opts: Option<&Table>) -> Result<(Vec<String>, f64), String> {
  const ITEMS: &str = "relevant: give a list of strings, or a string";
  let pieces = match items {
    Value::String(text) => {
      let how = opt::<Value>(opts, "split").map_err(|e| e.to_string())?;
      decide::pieces(&text.to_string_lossy(), split(&how.unwrap_or(Value::Nil))?)
    }
    Value::Table(list) => list
      .sequence_values::<String>()
      .collect::<mlua::Result<_>>()
      .map_err(|_| ITEMS.to_string())?,
    _ => return Err(ITEMS.into()),
  };
  let keep = opt::<f64>(opts, "keep").map_err(|_| "relevant: keep is a number from 0 to 1".to_string())?;
  Ok((pieces, keep.map_or(decide::KEEP, |k| k.clamp(0.0, 1.0))))
}

async fn relevant(lua: Lua, who: Caller, (items, about, opts): (Value, String, Option<Table>)) -> Told {
  // Misused, it fails as a tool does, with its message alone.
  let (pieces, keep) = match relevant_given(items, opts.as_ref()) {
    Ok(given) => given,
    Err(why) => return Ok((false, why.into_lua(&lua)?)),
  };
  let call = Call::start(&who.run, "relevant", &format!("{} items: {about}", pieces.len()));
  let cancel = call.cancel.clone();
  let got = match decide::judge(&about, &pieces, &cancel).await {
    Ok(judged) => {
      let s = decide::sift(&pieces, &judged, keep);
      let kept: Vec<&str> = s.kept.iter().map(|&i| pieces[i].as_str()).collect();
      let t = (|| {
        let t = lua.create_table()?;
        t.set("kept", kept)?;
        t.set("note", s.note)?;
        t.set("ps", s.ps)?;
        Ok::<_, mlua::Error>(t)
      })();
      t.map_err(|e| e.to_string())
    }
    Err(why) => Err(why),
  };
  call.end(&lua, got)
}
