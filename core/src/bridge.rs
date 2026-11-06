//! Bridges
//! =======
//! Another place to chat with Acorn: Telegram now, Discord or Slack
//! later, each a crate of its own. A bridge brings an assistant of its
//! own, beside Acorn and Max in the sidebar ("Telegram App"), whose chat
//! it carries both ways (the user may write there or in the app: one
//! chat), and the tools that assistant alone gets (a group's messages,
//! a post to a channel). The app and the server add the bridges they
//! have before the chats open (`add`); each starts its work once they
//! are (`hub::open_all`).

use crate::ai::{Out, Spec};
use crate::assistant::Assistant;
use futures_util::future::BoxFuture;
use parking_lot::RwLock;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub trait Bridge: Send + Sync + 'static {
  /// Its assistant, whose chat with the user it carries.
  fn assistant(&self) -> Assistant;
  /// The tools its assistant gets in that chat, beside everyone's.
  fn specs(&self) -> Vec<Spec>;
  /// One of those tools called.
  fn call(&self, name: String, input: Value, cancel: CancellationToken) -> BoxFuture<'static, Out>;
  /// Its work begun, once the chats are open (on the runtime).
  fn start(self: Arc<Self>);
}

static BRIDGES: RwLock<Vec<Arc<dyn Bridge>>> = RwLock::new(Vec::new());

/// A bridge added, before the chats open; one whose assistant another
/// already has is refused.
pub fn add(bridge: Arc<dyn Bridge>) -> bool {
  let id = bridge.assistant().id;
  let mut all = BRIDGES.write();
  if all.iter().any(|b| b.assistant().id == id) {
    return false;
  }
  all.push(bridge);
  true
}

/// Every bridge added.
pub fn all() -> Vec<Arc<dyn Bridge>> {
  BRIDGES.read().clone()
}

/// The bridge whose assistant this is.
pub fn of(assistant: &str) -> Option<Arc<dyn Bridge>> {
  BRIDGES.read().iter().find(|b| b.assistant().id == assistant).cloned()
}

/// Each bridge's work begun.
pub(crate) fn start() {
  for bridge in all() {
    bridge.start();
  }
}
