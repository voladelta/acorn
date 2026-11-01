//! Plugins
//! =======
//! What the user adds to luau without a new Acorn: <dir>/plugins/<name>.luau,
//! each returning the globals it provides, a `doc` line and a `value` each:
//!
//!   return { web_search = { doc = "web_search(query): string", value = search } }
//!
//! A global of a name already there replaces it, doc and all; of two plugins,
//! the last by name wins, and it is said. A plugin that fails to load is left
//! out, and whatever it would have replaced stays, so every call says so till
//! it is fixed.
//!
//! A plugin has what luau's code has, and `http` and `secret` besides, in an
//! environment of its own the model can't reach. Loading it can use no tool
//! (`secret` alone), so it has no effects and may be done again at will. Its
//! functions, run later, see every plugin's globals.
//!
//! `luau` builds a layer of them (`load`) for each VM, and again at a call's
//! start when the files have changed (`stamp`), so a plugin added, edited or
//! removed counts from the next call, and the model's own globals stay.

use crate::config;
use crate::luau::clean;
use mlua::{Lua, Table, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const EXT: &str = "luau";

/// Where the plugins are.
pub fn dir() -> PathBuf {
  config::get().dir.join("plugins")
}

/// What the prompts say of plugins.
pub fn note() -> String {
  format!(
    "# Plugins

luau's globals can be added to or replaced without restarting Acorn: when the
user wants another search or fetch provider or a service's API, write
{dir}/<name>.luau, and it counts from your next luau call, which says if it
failed to load. The file returns its globals, each a doc line and a value:

  local key = secret(\"EXA_API_KEY\")
  local function search(query)
    return http{{ method = \"POST\", url = \"https://api.exa.ai/search\", headers = {{ [\"x-api-key\"] = key }}, json = {{ query = query }} }}.json
  end
  return {{ web_search = {{ doc = \"web_search(query: string): table -- via Exa\", value = search }} }}

A plugin has everything luau's code has, and these two, which your own code
does not:
http{{url, method?, query?, headers?, json?, body?, timeout?, save?}}: {{status, text, json?}} or, with save (a new file's path), {{status, saved, bytes}}   -- raises on any status but a 2xx; tries a busy one again
secret(name): string   -- the key in that variable, else in config.json's lowercase key; raises if there is none

While the file loads, only secret works: call tools in its functions. Tell the
user to put a key in {config} themselves, never in a message; one they paste
anyway, write there and purge. Remove the file to remove the plugin. Plugins
run as you do, so read one from elsewhere before installing it.",
    dir = dir().display(),
    config = config::get().dir.join("config.json").display(),
  )
}

/// The plugins' files as they are: each one's name, time and size, in order.
pub type Stamp = Vec<(String, SystemTime, u64)>;

pub fn stamp(dir: &Path) -> Stamp {
  let mut files: Stamp = fs::read_dir(dir)
    .into_iter()
    .flatten()
    .flatten()
    .filter(|e| e.path().extension().is_some_and(|x| x == EXT))
    .filter_map(|e| {
      let meta = fs::metadata(e.path()).ok()?;
      let time = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
      Some((e.file_name().to_string_lossy().into_owned(), time, meta.len()))
    })
    .collect();
  files.sort();
  files
}

/// A set of plugins, loaded: what the model's code sees, each global's doc,
/// the globals they provide, and what is wrong with them.
pub struct Layer {
  pub view: Table,
  pub docs: BTreeMap<String, String>,
  pub provided: BTreeMap<String, String>,
  pub warns: Vec<String>,
}

/// The plugins in `dir`, over `base`'s globals and the docs of those, each
/// with `host`'s functions. `tick` runs before each, so one's time is its own.
pub fn load(
  lua: &Lua,
  base: &Table,
  docs: BTreeMap<String, String>,
  host: &Table,
  dir: &Path,
  tick: impl Fn(),
) -> mlua::Result<Layer> {
  let view = lua.create_table()?;
  for pair in base.pairs::<Value, Value>() {
    let (k, v) = pair?;
    // Not _G or coroutine (mlua builds its async calls on coroutines of its
    // own), and no tool's raw form.
    if matches!(&k, Value::String(s) if { let s = s.to_str()?; &*s == "_G" || &*s == "coroutine" || s.starts_with("__") })
    {
      continue;
    }
    view.raw_set(k, v)?;
  }
  let mut layer = Layer {
    view,
    docs,
    provided: BTreeMap::new(),
    warns: Vec::new(),
  };
  let mut values = Vec::new();
  for (file, ..) in stamp(dir) {
    tick();
    let plugin = file.strip_suffix(&format!(".{EXT}")).unwrap_or(&file).to_string();
    let got = fs::read_to_string(dir.join(&file))
      .map_err(|e| e.to_string())
      .and_then(|src| one(lua, &layer.view, host, &plugin, &src));
    match got {
      Ok(globals) => {
        for (name, doc, value) in globals {
          if let Some(was) = layer.provided.insert(name.clone(), plugin.clone()) {
            layer
              .warns
              .push(format!("{name} of plugin {was} is replaced by plugin {plugin}'s"));
          }
          layer.docs.insert(name.clone(), doc);
          values.push((name, value));
        }
      }
      Err(why) => layer.warns.push(format!("plugin {plugin} did not load: {why}")),
    }
  }
  for (name, value) in values {
    layer.view.raw_set(name, value)?;
  }
  layer.view.set_readonly(true);
  Ok(layer)
}

/// A plugin's globals: name, doc and value, its tables frozen.
fn one(lua: &Lua, view: &Table, host: &Table, plugin: &str, src: &str) -> Result<Vec<(String, String, Value)>, String> {
  let bad = |e: mlua::Error| clean(&e);
  let env = lua.create_table().map_err(bad)?;
  let meta = lua.create_table().map_err(bad)?;
  meta.set("__index", view).map_err(bad)?;
  meta.set("__metatable", "locked").map_err(bad)?;
  env.set_metatable(Some(meta)).map_err(bad)?;
  for pair in host.pairs::<Value, Value>() {
    let (k, v) = pair.map_err(bad)?;
    env.raw_set(k, v).map_err(bad)?;
  }
  let got: Value = lua
    .load(src)
    .set_name(format!("={plugin}"))
    .set_environment(env)
    .eval()
    .map_err(bad)?;
  let Value::Table(got) = got else {
    return Err("it must return a table of globals".into());
  };
  let mut globals = Vec::new();
  for pair in got.pairs::<Value, Value>() {
    let (k, entry) = pair.map_err(bad)?;
    let name = match &k {
      Value::String(s) => s.to_string_lossy(),
      _ => return Err("each global needs a name".into()),
    };
    if !is_name(&name) {
      return Err(format!("{name:?} can't be a global's name"));
    }
    let Value::Table(entry) = entry else {
      return Err(format!("{name} must be {{doc = ..., value = ...}}"));
    };
    let doc: Option<String> = entry.get("doc").map_err(bad)?;
    let doc = doc.map(|d| d.trim().to_string()).filter(|d| !d.is_empty());
    let Some(doc) = doc else {
      return Err(format!("{name} has no doc"));
    };
    if doc.contains('\n') {
      return Err(format!("{name}'s doc must be one line"));
    }
    let value: Value = entry.get("value").map_err(bad)?;
    if value.is_nil() {
      return Err(format!("{name} has no value"));
    }
    freeze(&value);
    globals.push((name, doc, value));
  }
  Ok(globals)
}

/// A global's name: an identifier, and none of the VM's own `__` ones.
fn is_name(name: &str) -> bool {
  !name.starts_with("__")
    && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
    && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A value's tables, read-only all the way down, so the model's code can't
/// change what a plugin's functions rely on.
fn freeze(value: &Value) {
  if let Value::Table(t) = value
    && !t.is_readonly()
  {
    t.set_readonly(true);
    for (_, v) in t.pairs::<Value, Value>().flatten() {
      freeze(&v);
    }
  }
}
