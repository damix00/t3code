use std::time::Duration;

use clap::Parser;
use gpui::{
    App, AppContext, Application, Bounds, Focusable, Menu, MenuItem, TitlebarOptions, WindowBounds,
    WindowOptions, point, px, size,
};
use t3_backend::Backend;
use t3_desktop::cli::Cli;
use t3_desktop::environment;
use t3_desktop::ui::{text_input, workspace};
use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_new(&cli.log).unwrap_or_else(|_| EnvFilter::new("info")))
        .with_writer(std::io::stderr)
        .init();

    // Networking and the server process live on tokio; GPUI owns the main thread.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("t3-runtime")
        .enable_all()
        .build()?;
    let handle = runtime.handle().clone();

    let mut environments = Vec::new();
    if !cli.no_local {
        match environment::start_local(&cli, &handle) {
            Ok(local) => environments.push(local),
            // Without a local server the window still opens to pair with a remote.
            Err(error) => tracing::error!(%error, "could not start the local T3 Code server"),
        }
    }
    if let Some(pairing_url) = cli.remote_pairing_url() {
        match environment::start_remote(&pairing_url, &handle) {
            Ok(remote) => environments.push(remote),
            Err(error) => anyhow::bail!("--remote: {error}"),
        }
    }

    let initial_thread = cli.thread.clone().map(t3_contracts::ThreadId::new);
    let backends: Vec<_> = environments
        .iter()
        .filter_map(|environment| environment.backend().cloned())
        .collect();
    stop_backends_on_signal(&handle, backends.clone());
    stop_backends_on_panic(backends);

    Application::new()
        .with_assets(t3_desktop::ui::assets::Assets)
        .run(move |cx: &mut App| {
            t3_desktop::ui::theme::register_fonts(cx);
            cx.bind_keys(text_input::bindings());
            cx.bind_keys(workspace::bindings());
            cx.on_action(|_: &workspace::Quit, cx| cx.quit());
            cx.set_menus(vec![Menu {
                name: "T3 Code".into(),
                items: vec![MenuItem::action("Quit T3 Code", workspace::Quit)],
            }]);

            let bounds = Bounds::centered(None, size(px(1200.), px(800.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("T3 Code".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(16.), px(19.))),
                }),
                window_min_size: Some(size(px(720.), px(480.))),
                ..Default::default()
            };
            let window = cx.open_window(options, |window, cx| {
                cx.new(|cx| {
                    let mut workspace =
                        workspace::Workspace::new(handle.clone(), environments, window, cx);
                    if let Some(thread) = initial_thread {
                        workspace.open_initial_thread(thread, window, cx);
                    }
                    workspace
                })
            });
            let window = match window {
                Ok(window) => window,
                Err(error) => {
                    tracing::error!(%error, "could not open a window");
                    cx.quit();
                    return;
                }
            };
            let _ = window.update(cx, |workspace, window, cx| {
                workspace.focus_composer(window, cx);
                let _ = workspace.focus_handle(cx);
            });
            let workspace = window.entity(cx).ok();
            cx.on_app_quit(move |cx| {
                if let Some(workspace) = &workspace {
                    workspace.update(cx, |workspace, _| workspace.shutdown());
                }
                async {}
            })
            .detach();
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
    drop(runtime);
    Ok(())
}

/// A terminated app must not leave its server running unparented.
fn stop_backends_on_signal(runtime: &tokio::runtime::Handle, backends: Vec<Backend>) {
    runtime.spawn(async move {
        termination_signal().await;
        tracing::info!("stopping on signal");
        let stops = backends.iter().map(Backend::shutdown);
        let _ =
            tokio::time::timeout(Duration::from_secs(8), futures::future::join_all(stops)).await;
        std::process::exit(0);
    });
}

#[cfg(unix)]
async fn termination_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let streams = [
        SignalKind::interrupt(),
        SignalKind::terminate(),
        SignalKind::hangup(),
    ]
    .map(signal);
    let mut waits = Vec::new();
    for stream in streams {
        match stream {
            Ok(mut stream) => waits.push(Box::pin(async move { stream.recv().await })),
            Err(error) => tracing::warn!(%error, "could not listen for a signal"),
        }
    }
    if waits.is_empty() {
        return std::future::pending().await;
    }
    futures::future::select_all(waits).await;
}

#[cfg(not(unix))]
async fn termination_signal() {
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// A panic on the UI thread takes the app down past GPUI's quit handlers, so signal the
/// servers directly. Panics in runtime tasks are contained and leave the servers alone.
fn stop_backends_on_panic(backends: Vec<Backend>) {
    let ui_thread = std::thread::current().id();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().id() == ui_thread {
            for backend in &backends {
                backend.terminate_now();
            }
        }
        previous(info);
    }));
}
