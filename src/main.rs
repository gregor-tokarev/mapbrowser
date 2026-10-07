mod app;
mod blocker;
mod engine;
mod favicons;
mod input;
mod model;
#[cfg(target_os = "macos")]
mod native;
mod ui;

#[cfg(not(target_os = "macos"))]
mod native {
    /// Previews have no system setting; MAPBROWSER_REDUCE_MOTION stands in.
    pub fn reduce_motion() -> bool {
        std::env::var_os("MAPBROWSER_REDUCE_MOTION").is_some()
    }

    /// Only macOS has an application menu with About.
    pub fn show_about_panel() {}
}

use app::{BrowserApp, external_web_url, run_shortcut};
use blocker::ContentBlocker;
use engine::{Engine, Shortcut};
use gpui::{prelude::*, *};
use model::BrowserState;
use std::{cell::RefCell, rc::Rc};

actions!(mapbrowser, [Quit, About, NewSpace, Dismiss]);

/// A browser shortcut. GPUI binds the keys the macOS key monitor intercepts,
/// so they also work where the monitor doesn't run, and the menu bar shows them.
#[derive(Clone, Copy, Debug, PartialEq, Action)]
#[action(namespace = mapbrowser, no_json)]
pub struct Browse(pub Shortcut);

/// Puts the traffic lights' centres at x 20/40/60, y 20. GPUI moves the
/// close button's 14 × 16 pt frame to this top-left point inside the 28 pt
/// titlebar, which clips buttons hanging below it, so they can't sit as low
/// as Arc's (y 24). The sidebar's navigation row is level with them.
const TRAFFIC_LIGHTS: Point<Pixels> = point(px(13.), px(12.));

fn key_bindings() -> Vec<KeyBinding> {
    use Shortcut::*;
    let mut bindings = vec![
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("escape", Dismiss, Some("Browser")),
        KeyBinding::new("cmd-t", Browse(CommandBar), None),
        KeyBinding::new("cmd-l", Browse(EditAddress), None),
        KeyBinding::new("cmd-w", Browse(CloseTab), None),
        KeyBinding::new("cmd-shift-t", Browse(ReopenClosedTab), None),
        KeyBinding::new("ctrl-tab", Browse(NextTab), None),
        KeyBinding::new("ctrl-shift-tab", Browse(PreviousTab), None),
        KeyBinding::new("cmd-alt-down", Browse(NextTabInList), None),
        KeyBinding::new("cmd-alt-up", Browse(PreviousTabInList), None),
        KeyBinding::new("cmd-alt-left", Browse(PreviousSpace), None),
        KeyBinding::new("cmd-alt-right", Browse(NextSpace), None),
        KeyBinding::new("cmd-r", Browse(Reload), None),
        KeyBinding::new("cmd-[", Browse(Back), None),
        KeyBinding::new("cmd-]", Browse(Forward), None),
        KeyBinding::new("cmd-s", Browse(ToggleSidebar), None),
        KeyBinding::new("cmd-d", Browse(TogglePin), None),
        KeyBinding::new("cmd-shift-c", Browse(CopyUrl), None),
        KeyBinding::new("cmd-shift-k", Browse(ClearToday), None),
    ];
    for digit in 1..=9u8 {
        bindings.push(KeyBinding::new(
            &format!("cmd-{digit}"),
            Browse(SelectTab(digit)),
            None,
        ));
        bindings.push(KeyBinding::new(
            &format!("ctrl-{digit}"),
            Browse(SelectSpace(digit)),
            None,
        ));
    }
    bindings
}

fn menus() -> Vec<Menu> {
    use Shortcut::*;
    let menu = |name: &'static str, items| Menu {
        name: name.into(),
        items,
    };
    vec![
        menu(
            "Map",
            vec![
                MenuItem::action("About Map", About),
                MenuItem::separator(),
                MenuItem::action("Quit Map", Quit),
            ],
        ),
        menu(
            "File",
            vec![
                MenuItem::action("New Tab", Browse(CommandBar)),
                MenuItem::action("Open Location…", Browse(EditAddress)),
                MenuItem::action("Reopen Closed Tab", Browse(ReopenClosedTab)),
                MenuItem::separator(),
                MenuItem::action("Close Tab", Browse(CloseTab)),
            ],
        ),
        // Routed to whichever view has focus, the web page included.
        menu(
            "Edit",
            vec![
                MenuItem::os_action("Cut", input::Cut, OsAction::Cut),
                MenuItem::os_action("Copy", input::Copy, OsAction::Copy),
                MenuItem::os_action("Paste", input::Paste, OsAction::Paste),
                MenuItem::os_action("Select All", input::SelectAll, OsAction::SelectAll),
            ],
        ),
        menu(
            "View",
            vec![
                MenuItem::action("Toggle Sidebar", Browse(ToggleSidebar)),
                MenuItem::separator(),
                MenuItem::action("Reload Page", Browse(Reload)),
                MenuItem::action("Back", Browse(Back)),
                MenuItem::action("Forward", Browse(Forward)),
            ],
        ),
        menu(
            "Spaces",
            vec![
                MenuItem::action("New Space", NewSpace),
                MenuItem::separator(),
                MenuItem::action("Next Space", Browse(NextSpace)),
                MenuItem::action("Previous Space", Browse(PreviousSpace)),
            ],
        ),
        menu(
            "Tab",
            vec![
                MenuItem::action("Pin or Unpin Tab", Browse(TogglePin)),
                MenuItem::action("Copy URL", Browse(CopyUrl)),
                MenuItem::separator(),
                MenuItem::action("Clear Today Tabs", Browse(ClearToday)),
            ],
        ),
    ]
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let state = BrowserState::load()?;
    let blocker = ContentBlocker::load_in_background(state.root_dir().join("blocker"));
    let (mut engine, events) = Engine::initialize(state.root_dir())?;
    engine.set_blocker(blocker);
    let engine = Rc::new(RefCell::new(engine));
    let retained_engine = engine.clone();
    let (external_urls_tx, external_urls_rx) = async_channel::unbounded();
    let application = Application::new().with_assets(ui::icons::Assets);
    application.on_open_urls(move |urls| {
        for url in urls.iter().filter_map(|url| external_web_url(url)) {
            let _ = external_urls_tx.try_send(url);
        }
    });
    application.run(move |cx: &mut App| {
        input::init(cx);
        ui::command_bar::init(cx);
        cx.bind_keys(key_bindings());
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_action(|_: &About, _| native::show_about_panel());
        cx.set_menus(menus());
        let bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(820.), px(540.))),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Map Browser".into()),
                        appears_transparent: true,
                        traffic_light_position: Some(TRAFFIC_LIGHTS),
                    }),
                    ..Default::default()
                },
                |window, cx| {
                    #[cfg(target_os = "macos")]
                    if let Some(view) = native::ns_view(window) {
                        engine
                            .borrow_mut()
                            .attach(view)
                            .expect("attach browser view");
                    }
                    window.on_window_should_close(cx, |_, cx| {
                        cx.quit();
                        true
                    });
                    cx.new(|cx| {
                        BrowserApp::new(state, engine, events, external_urls_rx, window, cx)
                    })
                },
            )
            .expect("open browser window");
        // Shortcuts and menu commands reach the browser window from any
        // window, the command bar included. They run once the dispatching
        // window, which may be the browser window itself, is free again.
        cx.on_action(move |&Browse(shortcut), cx| {
            cx.defer(move |cx| run_shortcut(window, shortcut, cx))
        });
        cx.on_action(move |_: &NewSpace, cx| {
            cx.defer(move |cx| {
                let _ = window.update(cx, |app, window, cx| app.new_space(window, cx));
            })
        });
        cx.activate(true);
    });
    retained_engine.borrow_mut().shutdown()?;
    Ok(())
}
