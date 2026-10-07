mod app;
mod blocker;
mod engine;
mod favicons;
mod input;
mod model;
#[cfg(target_os = "macos")]
mod native;
mod recent;
mod ui;

#[cfg(not(target_os = "macos"))]
mod native {
    /// Previews have no system setting; MAPBROWSER_REDUCE_MOTION stands in.
    pub fn reduce_motion() -> bool {
        std::env::var_os("MAPBROWSER_REDUCE_MOTION").is_some()
    }

    /// Only macOS has an application menu with About.
    pub fn show_about_panel() {}

    /// Only macOS has a Window menu.
    pub fn bring_all_to_front() {}

    /// Previews have no Finder.
    pub fn reveal_in_finder(_path: &std::path::Path) {}
}

use app::{BrowserApp, external_web_url, run_shortcut};
use blocker::ContentBlocker;
use engine::{Engine, Shortcut};
use gpui::{prelude::*, *};
use model::BrowserState;
use std::{cell::RefCell, rc::Rc};

actions!(
    mapbrowser,
    [
        Quit,
        About,
        Hide,
        HideOthers,
        ShowAll,
        NewSpace,
        Dismiss,
        Undo,
        Redo,
        Minimize,
        Zoom,
        BringAllToFront
    ]
);

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
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
        // The key monitor sends these to a focused page first; bound here
        // so the Edit menu shows them.
        KeyBinding::new("cmd-z", Undo, None),
        KeyBinding::new("cmd-shift-z", Redo, None),
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
        // Text fields bind these too, later, so theirs move the caret there.
        KeyBinding::new("cmd-left", Browse(Back), None),
        KeyBinding::new("cmd-right", Browse(Forward), None),
        KeyBinding::new("cmd-s", Browse(ToggleSidebar), None),
        KeyBinding::new("cmd-d", Browse(TogglePin), None),
        KeyBinding::new("cmd-shift-c", Browse(CopyUrl), None),
        KeyBinding::new("cmd-shift-k", Browse(ClearToday), None),
        // The first binding of an action is the one the menu bar shows.
        KeyBinding::new("cmd-+", Browse(ZoomIn), None),
        KeyBinding::new("cmd-=", Browse(ZoomIn), None),
        KeyBinding::new("cmd--", Browse(ZoomOut), None),
        KeyBinding::new("cmd-0", Browse(ZoomReset), None),
        KeyBinding::new("cmd-.", Browse(Stop), None),
        KeyBinding::new("alt-cmd-i", Browse(DevTools), None),
        KeyBinding::new("cmd-f", Browse(Find), None),
        KeyBinding::new("cmd-g", Browse(FindNext), None),
        KeyBinding::new("cmd-shift-g", Browse(FindPrevious), None),
        KeyBinding::new("cmd-p", Browse(Print), None),
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
                MenuItem::action("Hide Map", Hide),
                MenuItem::action("Hide Others", HideOthers),
                MenuItem::action("Show All", ShowAll),
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
                MenuItem::separator(),
                MenuItem::action("Print…", Browse(Print)),
            ],
        ),
        // Routed to whichever view has focus, the web page included. Undo
        // and Redo act on the page only; the shell's text fields have none.
        menu(
            "Edit",
            vec![
                MenuItem::os_action("Undo", Undo, OsAction::Undo),
                MenuItem::os_action("Redo", Redo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action("Cut", input::Cut, OsAction::Cut),
                MenuItem::os_action("Copy", input::Copy, OsAction::Copy),
                MenuItem::os_action("Paste", input::Paste, OsAction::Paste),
                MenuItem::os_action("Select All", input::SelectAll, OsAction::SelectAll),
                MenuItem::separator(),
                MenuItem::submenu(Menu {
                    name: "Find".into(),
                    items: vec![
                        MenuItem::action("Find…", Browse(Find)),
                        MenuItem::action("Find Next", Browse(FindNext)),
                        MenuItem::action("Find Previous", Browse(FindPrevious)),
                    ],
                }),
            ],
        ),
        menu(
            "View",
            vec![
                MenuItem::action("Toggle Sidebar", Browse(ToggleSidebar)),
                MenuItem::separator(),
                MenuItem::action("Reload Page", Browse(Reload)),
                MenuItem::action("Stop", Browse(Stop)),
                MenuItem::action("Back", Browse(Back)),
                MenuItem::action("Forward", Browse(Forward)),
                MenuItem::separator(),
                MenuItem::action("Actual Size", Browse(ZoomReset)),
                MenuItem::action("Zoom In", Browse(ZoomIn)),
                MenuItem::action("Zoom Out", Browse(ZoomOut)),
                MenuItem::separator(),
                MenuItem::action("Developer Tools", Browse(DevTools)),
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
        // The name makes it AppKit's Window menu, which lists the windows.
        menu(
            "Window",
            vec![
                MenuItem::action("Minimize", Minimize),
                MenuItem::action("Zoom", Zoom),
                MenuItem::separator(),
                MenuItem::action("Bring All to Front", BringAllToFront),
            ],
        ),
    ]
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let state = BrowserState::load()?;
    if let Err(error) = state.remove_orphaned_profiles() {
        eprintln!("Couldn't remove a deleted space's data: {error}");
    }
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
        // First: of bindings for the same keys, GPUI picks the one added
        // last, so the text fields' and pop-ups' own bindings win.
        cx.bind_keys(key_bindings());
        input::init(cx);
        ui::command_bar::init(cx);
        ui::find_bar::init(cx);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_action(|_: &About, _| native::show_about_panel());
        cx.on_action(|_: &Hide, cx| cx.hide());
        cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
        cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
        cx.on_action(|_: &BringAllToFront, _| native::bring_all_to_front());
        cx.set_menus(menus());
        let (bounds, display_id) = ui::window_frame::initial(state.window.as_ref(), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(bounds),
                    display_id,
                    window_min_size: Some(ui::window_frame::MIN_SIZE),
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
                    // The close button quits the way ⌘Q does. The window, and
                    // with it the browser state that saves on quit, must stay
                    // until App::shutdown has run the quit observers.
                    window.on_window_should_close(cx, |_, cx| {
                        cx.quit();
                        false
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
        // Like NewSpace, these act on the browser window, also while the
        // command bar (a child window) has focus.
        cx.on_action(move |_: &Minimize, cx| {
            cx.defer(move |cx| {
                let _ = window.update(cx, |_, window, _| window.minimize_window());
            })
        });
        cx.on_action(move |_: &Zoom, cx| {
            cx.defer(move |cx| {
                let _ = window.update(cx, |_, window, _| window.zoom_window());
            })
        });
        cx.on_action(move |_: &Undo, cx| {
            cx.defer(move |cx| {
                let _ = window.update(cx, |app, _, _| app.engine.borrow().undo_in_page());
            })
        });
        cx.on_action(move |_: &Redo, cx| {
            cx.defer(move |cx| {
                let _ = window.update(cx, |app, _, _| app.engine.borrow().redo_in_page());
            })
        });
        cx.activate(true);
    });
    retained_engine.borrow_mut().shutdown()?;
    Ok(())
}
