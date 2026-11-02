//! Acorn in a browser: the window (`acorn`) on nsh's web runtime. Its
//! engines run in `acorn-server`, which serves this module and its page.

#![cfg(target_arch = "wasm32")]

nsh_platform_web::rust_app!(vec![nsh_base::registry()], nsh_core::element::Root::of::<acorn::App>());
