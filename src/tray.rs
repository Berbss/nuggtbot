//! Desktop system-tray (optional, `--features tray`).
//!
//! Shows the nuggtbot logo in the tray. Left-click opens the web panel in the
//! default browser; right-click opens a menu with "Open panel" and "Quit". The
//! async app runs on a background thread; this runs the tray event loop on the
//! main thread (required on Windows/macOS, and gtk on Linux via `tao`).
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

/// The URL the browser opens. `bind` is `host:port`; we always point the browser
/// at localhost (0.0.0.0 isn't a browsable address).
pub fn panel_url(bind: &str) -> String {
    let port = bind.rsplit(':').next().filter(|p| !p.is_empty()).unwrap_or("7777");
    format!("http://127.0.0.1:{port}")
}

/// On Linux a tray needs a display; without one, the caller should run headless.
pub fn display_available() -> bool {
    if cfg!(target_os = "linux") {
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
    } else {
        true
    }
}

/// Rasterize the embedded SVG logo to an RGBA tray icon (pure Rust, no system libs).
fn load_icon() -> Option<Icon> {
    const SVG: &str = include_str!("../assets/logo.svg");
    let tree = resvg::usvg::Tree::from_str(SVG, &resvg::usvg::Options::default()).ok()?;
    let size: u32 = 64;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size)?;
    let ts = tree.size();
    let scale = size as f32 / ts.width().max(ts.height());
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Icon::from_rgba(pixmap.take(), size, size).ok()
}

/// Run the tray event loop on the main thread. Never returns (process exits on Quit).
pub fn run(url: String) {
    let event_loop = EventLoopBuilder::new().build();

    let menu = Menu::new();
    let open_item = MenuItem::new("Open panel / Abrir painel", true, None);
    let quit_item = MenuItem::new("Quit / Fechar", true, None);
    let _ = menu.append(&open_item);
    let _ = menu.append(&quit_item);
    let open_id = open_item.id().clone();
    let quit_id = quit_item.id().clone();

    let menu_rx = MenuEvent::receiver();
    let tray_rx = TrayIconEvent::receiver();

    // The tray must be created after the event loop is running (gtk on Linux), so
    // build it lazily on the first loop iteration and keep it alive.
    let mut tray = None;

    event_loop.run(move |_event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;

        if tray.is_none() {
            let mut b = TrayIconBuilder::new()
                .with_menu(Box::new(menu.clone()))
                .with_tooltip("nuggtbot");
            if let Some(icon) = load_icon() {
                b = b.with_icon(icon);
            }
            tray = b.build().ok();
        }

        while let Ok(ev) = menu_rx.try_recv() {
            if ev.id == open_id {
                let _ = open::that_detached(&url);
            } else if ev.id == quit_id {
                std::process::exit(0);
            }
        }
        while let Ok(ev) = tray_rx.try_recv() {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                let _ = open::that_detached(&url);
            }
        }
    });
}
