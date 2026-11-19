//! acorn-host
//! ==========
//! One machine, an Acorn each. Acorn's core serves one person per process
//! (its config, open chats, facts and timers are process-wide), so the host
//! runs one unchanged `acorn-server` per person, each with its own HOME and
//! `ACORN_DIR` under `<data>/users/<id>`, in a sandbox, on a host-owned Unix
//! socket under `<data>/run/<id>`. People sign in from an invite link
//! (`/join/<code>`), and their requests go to their own server only.

pub mod proxy;
pub mod runner;
pub mod sandbox;
pub mod store;
