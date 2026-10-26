//! Acorn on this Mac: the root in a window of its own. As the window
//! closes, every process the agent started dies with it.

#[cfg(target_os = "macos")]
fn main() -> impl std::process::Termination {
    env_logger::init();
    let done = nsh_platform_macos::run_app(
        nsh_platform_macos::AppSource::Rust(nsh_platform_macos::Root::of::<acorn::App>()),
        nsh_platform_macos::AppOptions { title: "Acorn".to_owned(), size: (1140, 820), ..Default::default() },
    );
    acorn_core::machine::end();
    done
}

#[cfg(not(target_os = "macos"))]
compile_error!("Acorn runs on macOS.");
