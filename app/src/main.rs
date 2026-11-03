//! Acorn on this machine (macOS or Linux): the root in a window of its
//! own. As the window closes, every process the agent started dies with
//! it. The engines are Unix-only, so there is no Windows build; in a
//! browser, the window is `acorn-web` and the engines `acorn-server`.

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[cfg(target_os = "macos")]
fn main() -> impl std::process::Termination {
  env_logger::init();
  let done = nsh_platform_macos::run_app(
    nsh_platform_macos::AppSource::Rust(nsh_platform_macos::Root::of::<acorn::App>()),
    nsh_platform_macos::AppOptions {
      title: "Acorn".to_owned(),
      size: (1140, 820),
      ..Default::default()
    },
  );
  acorn_core::machine::end();
  done
}

#[cfg(target_os = "linux")]
fn main() -> impl std::process::Termination {
  env_logger::init();
  let done = nsh_platform_linux::run_app(
    nsh_platform_linux::AppSource::Rust(nsh_platform_linux::Root::of::<acorn::App>()),
    nsh_platform_linux::AppOptions {
      app_id: "dev.acorn.app".to_owned(),
      title: "Acorn".to_owned(),
      size: (1140, 820),
      ..Default::default()
    },
  );
  acorn_core::machine::end();
  done
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!("Acorn's desktop app runs on macOS and Linux; in a browser, build acorn-web and run acorn-server.");
