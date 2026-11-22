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
fn opt<T: mlua::FromLua>(opts: &Option<Table>, k: &str) -> mlua::Result<Option<T>> {
  opts.as_ref().map_or(Ok(None), |t| t.get(k))
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

/// The Mac's tools and the web's, as globals of the VM, in `cwd`.
pub(super) fn tools(lua: &Lua, cwd: &Path, run: &Arc<Mutex<Run>>) -> mlua::Result<()> {
  let g = lua.globals();
  let (dir, r) = (cwd.to_path_buf(), run.clone());
  g.set(
    "__read",
    lua.create_async_function(move |lua, (path, opts): (String, Option<Table>)| {
      let (dir, r) = (dir.clone(), r.clone());
      async move {
        let (offset, limit) = (opt::<usize>(&opts, "offset")?, opt::<usize>(&opts, "limit")?);
        let call = Call::start(&r, "read", &path);
        call.end(&lua, blocking(|| machine::text(&dir, &path, offset, limit)))
      }
    })?,
  )?;
  let (dir, r) = (cwd.to_path_buf(), run.clone());
  g.set(
    "__write",
    lua.create_async_function(move |lua, (path, content): (String, mlua::LuaString)| {
      let (dir, r) = (dir.clone(), r.clone());
      async move {
        let content = content.to_str()?.to_string();
        let call = Call::start(&r, "write", &path);
        let bytes = content.len();
        let input = json!({"file_path": path, "content": content});
        let got = blocking(|| machine::write(&dir, &input));
        call.end(&lua, got.map(|_| bytes))
      }
    })?,
  )?;
  let (dir, r) = (cwd.to_path_buf(), run.clone());
  g.set(
    "__edit",
    lua.create_async_function(
      move |lua, (path, old, new, opts): (String, mlua::LuaString, mlua::LuaString, Option<Table>)| {
        let (dir, r) = (dir.clone(), r.clone());
        async move {
          let input = json!({
            "file_path": path,
            "old_string": &*old.to_str()?,
            "new_string": &*new.to_str()?,
            "replace_all": opt::<bool>(&opts, "all")?.unwrap_or(false),
          });
          let call = Call::start(&r, "edit", &path);
          call.end(&lua, blocking(|| machine::edit(&dir, &input)))
        }
      },
    )?,
  )?;
  let (dir, r) = (cwd.to_path_buf(), run.clone());
  g.set(
    "__sh",
    lua.create_async_function(move |lua, (cmd, opts): (String, Option<Table>)| {
      let (dir, r) = (dir.clone(), r.clone());
      async move {
        let secs = opt::<f64>(&opts, "timeout")?
          .filter(|t| !t.is_nan())
          .unwrap_or(SH_TIMEOUT)
          .clamp(1.0, SH_TIMEOUT_MAX);
        let workdir = opt::<String>(&opts, "cwd")?.map_or_else(|| dir.clone(), |d| dir.join(d));
        let call = Call::start(&r, "sh", &cmd);
        let cancel = call.cancel.clone();
        let got = machine::shell(&cmd, &workdir, secs * 1000.0, cancel).await;
        let got = match got {
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
    })?,
  )?;
  let (dir, r) = (cwd.to_path_buf(), run.clone());
  g.set(
    "__view",
    lua.create_async_function(move |lua, path: String| {
      let (dir, r) = (dir.clone(), r.clone());
      async move {
        let call = Call::start(&r, "view", &path);
        let file = dir.join(&path);
        let got = async {
          if r.lock().imgs.len() >= PICS {
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
          r.lock().imgs.push(img);
          Ok(format!("{path}: {w}×{h}, attached: you see it after this call"))
        }
        .await;
        call.end(&lua, got)
      }
    })?,
  )?;
  let (dir, r) = (cwd.to_path_buf(), run.clone());
  g.set(
    "__ocr",
    lua.create_async_function(move |lua, path: String| {
      let (dir, r) = (dir.clone(), r.clone());
      async move {
        let call = Call::start(&r, "ocr", &path);
        let cancel = call.cancel.clone();
        let got = ocr::read(&dir.join(&path), &cancel).await;
        call.end(&lua, got.map_err(|e| format!("ocr {e}")))
      }
    })?,
  )?;
  g.set(
    "__secret",
    lua.create_function(|lua, name: String| {
      Ok(match config::secret(&name) {
        Ok(key) => (true, key.into_lua(lua)?),
        Err(why) => (false, why.into_lua(lua)?),
      })
    })?,
  )?;
  let (dir, r) = (cwd.to_path_buf(), run.clone());
  g.set(
    "__http",
    lua.create_async_function(move |lua, req: Table| {
      let (dir, r) = (dir.clone(), r.clone());
      async move {
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
        let call = Call::start(&r, "http", &bare(&url));
        let cancel = call.cancel.clone();
        let sent = web::Req {
          method,
          url,
          query,
          headers,
          body,
          timeout,
        };
        let to = save.as_ref().map(|path| dir.join(path));
        // Before a request that may cost money, not after.
        if let Some(to) = to.as_ref().filter(|to| to.symlink_metadata().is_ok()) {
          return call.end(
            &lua,
            Err::<Table, _>(format!("http: {} is already there", to.display())),
          );
        }
        let got = async {
          let res = web::request(&sent, &cancel).await?;
          let t = lua.create_table().map_err(|e| e.to_string())?;
          let status = res.status().as_u16();
          t.set("status", status).map_err(|e| e.to_string())?;
          if let Some(to) = &to {
            let bytes = web::save(res, to, &cancel).await?;
            t.set("saved", save.clone()).map_err(|e| e.to_string())?;
            t.set("bytes", bytes).map_err(|e| e.to_string())?;
            return Ok(t);
          }
          let bytes = web::read(res, &cancel).await?;
          if let Ok(v) = serde_json::from_slice::<Json>(&bytes) {
            // A null is no field at all, not a value to trip on.
            t.set("json", lua.to_value_with(&v, no_nulls()).map_err(|e| e.to_string())?)
              .map_err(|e| e.to_string())?;
          }
          t.set("text", lua.create_string(&bytes).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
          Ok(t)
        }
        .await;
        call.end(&lua, got)
      }
    })?,
  )?;
  let r = run.clone();
  g.set(
    "__web_search",
    lua.create_async_function(move |lua, (query, opts): (String, Option<Table>)| {
      let r = r.clone();
      async move {
        let mut input = json!({"query": query});
        if let Some(n) = opt::<u64>(&opts, "count")? {
          input["count"] = n.into();
        }
        if let Some(q) = opt::<String>(&opts, "question")? {
          input["question"] = q.into();
        }
        let call = Call::start(&r, "web_search", &query);
        let cancel = call.cancel.clone();
        let out = web::run("web_search", &input, cancel).await;
        call.end(&lua, if out.fail { Err(out.text) } else { Ok(out.text) })
      }
    })?,
  )?;
  let r = run.clone();
  g.set(
    "__web_fetch",
    lua.create_async_function(move |lua, (url, opts): (String, Option<Table>)| {
      let r = r.clone();
      async move {
        let mut input = json!({"url": url});
        if let Some(at) = opt::<u64>(&opts, "at")? {
          input["at"] = at.into();
        }
        let call = Call::start(&r, "web_fetch", &url);
        let cancel = call.cancel.clone();
        let out = web::run("web_fetch", &input, cancel).await;
        call.end(&lua, if out.fail { Err(out.text) } else { Ok(out.text) })
      }
    })?,
  )?;
  let r = run.clone();
  g.set(
    "__decide",
    lua.create_async_function(move |lua, (state, questions): (Value, Value)| {
      let r = r.clone();
      async move {
        let state: Json = lua.from_value(state)?;
        let questions: Json = lua.from_value(questions)?;
        let names: Vec<_> = questions
          .as_object()
          .map(|q| q.keys().cloned().collect())
          .unwrap_or_default();
        let call = Call::start(&r, "decide", &names.join(", "));
        let cancel = call.cancel.clone();
        let got = match decide::ask(&state, &questions, &cancel).await {
          Ok(answers) => lua.to_value_with(&answers, no_nulls()).map_err(|e| e.to_string()),
          Err(why) => Err(why),
        };
        call.end(&lua, got)
      }
    })?,
  )?;
  let r = run.clone();
  g.set(
    "__relevant",
    lua.create_async_function(move |lua, (items, about, opts): (Value, String, Option<Table>)| {
      let r = r.clone();
      async move {
        // Misused, it fails as a tool does, with its message alone.
        let given = (|| -> Result<(Vec<String>, f64), String> {
          let pieces = match items {
            Value::String(text) => {
              let how = opt::<Value>(&opts, "split").map_err(|e| e.to_string())?;
              decide::pieces(&text.to_string_lossy(), split(&how.unwrap_or(Value::Nil))?)
            }
            Value::Table(list) => list
              .sequence_values::<String>()
              .collect::<mlua::Result<_>>()
              .map_err(|_| "relevant: give a list of strings, or a string".to_string())?,
            _ => return Err("relevant: give a list of strings, or a string".into()),
          };
          let keep = opt::<f64>(&opts, "keep").map_err(|_| "relevant: keep is a number from 0 to 1".to_string())?;
          Ok((pieces, keep.map_or(decide::KEEP, |k| k.clamp(0.0, 1.0))))
        })();
        let (pieces, keep) = match given {
          Ok(given) => given,
          Err(why) => return Ok((false, why.into_lua(&lua)?)),
        };
        let call = Call::start(&r, "relevant", &format!("{} items: {about}", pieces.len()));
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
    })?,
  )?;
  Ok(())
}
