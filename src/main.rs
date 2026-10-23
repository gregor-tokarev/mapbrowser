mod app;
mod blocker;
mod engine;
mod favicons;
mod history;
mod input;
mod launch_error;
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

    /// Previews have no default browser to be.
    pub fn is_default_browser() -> bool {
        false
    }

    /// Previews have no default browser to be.
    pub fn set_as_default_browser() {}

    /// Previews read only the XML form of a .webloc, which macOS also writes.
    pub fn webloc_address(path: &std::path::Path) -> Option<String> {
        crate::app::webloc_address_in_xml(&std::fs::read_to_string(path).ok()?)
    }
}

use app::{BrowserApp, opened_address, run_shortcut};
use blocker::ContentBlocker;
use engine::{Engine, Shortcut};
use gpui::{prelude::*, *};
use model::{BrowserState, Pin};
use std::{cell::RefCell, rc::Rc};
use ui::{command_bar::CommandMode, downloads::request_quit};

actions!(
    mapbrowser,
    [
        Quit,
        About,
        Settings,
        Hide,
        HideOthers,
        ShowAll,
        NewSpace,
        Dismiss,
        Undo,
        Redo,
        Minimize,
        Zoom,
        BringAllToFront,
        SetDefaultBrowser,
        ShowShortcuts,
        ShowHistory,
        ClearHistory
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
        // The key monitor sends these to a focused page first; the shell's
        // text fields take them while they have focus.
        KeyBinding::new("cmd-z", Undo, None),
        KeyBinding::new("cmd-shift-z", Redo, None),
        // Not browser shortcuts: a page that leaves them alone hands them to
        // the menu bar.
        KeyBinding::new("cmd-,", Settings, None),
        KeyBinding::new("cmd-y", ShowHistory, None),
        KeyBinding::new("escape", Dismiss, Some("Browser")),
        KeyBinding::new("cmd-t", Browse(CommandBar), None),
        KeyBinding::new("cmd-l", Browse(EditAddress), None),
        KeyBinding::new("cmd-w", Browse(CloseTab), None),
        KeyBinding::new("cmd-shift-t", Browse(ReopenClosedTab), None),
        KeyBinding::new("ctrl-tab", Browse(NextTab), None),
        KeyBinding::new("ctrl-shift-tab", Browse(PreviousTab), None),
        KeyBinding::new("ctrl-escape", Browse(CancelTabSwitch), None),
        KeyBinding::new("ctrl-shift-escape", Browse(CancelTabSwitch), None),
        KeyBinding::new("cmd-alt-down", Browse(NextTabInList), None),
        KeyBinding::new("cmd-alt-up", Browse(PreviousTabInList), None),
        // Chrome's and Safari's keys for the same, after the menu's own.
        KeyBinding::new("cmd-shift-]", Browse(NextTabInList), None),
        KeyBinding::new("cmd-shift-[", Browse(PreviousTabInList), None),
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
    // Previews have no menu bar, so no Help menu: ⌘/ stands in for it.
    // On macOS web editors keep ⌘/ (comment a line, list their keys).
    #[cfg(not(target_os = "macos"))]
    bindings.push(KeyBinding::new("cmd-/", ShowShortcuts, None));
    bindings
}

/// What the menu bar's changing items follow: Show or Hide Sidebar, what
/// ⌘D and ⌘W do to the active tab, and the spaces ⌃1–⌃9 go to.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MenuBarState {
    pub sidebar_hidden: bool,
    /// The active tab's section.
    pub pin: Pin,
    /// The first nine spaces' names.
    pub spaces: Vec<String>,
}

pub fn menus(state: &MenuBarState) -> Vec<Menu> {
    use Shortcut::*;
    // The tab's own menu words: ⌘W only unloads a pinned tab or favorite.
    let (close, pin) = match state.pin {
        Pin::Today => ("Close Tab", "Pin Tab"),
        Pin::Pinned => ("Unload Tab", "Unpin Tab"),
        Pin::Favorite => ("Unload Tab", "Remove from Favorites"),
    };
    let menu = |name: &'static str, items| Menu {
        name: name.into(),
        items,
    };
    let mut spaces = vec![
        MenuItem::action("New Space", NewSpace),
        MenuItem::separator(),
        MenuItem::action("Next Space", Browse(NextSpace)),
        MenuItem::action("Previous Space", Browse(PreviousSpace)),
    ];
    if !state.spaces.is_empty() {
        spaces.push(MenuItem::separator());
    }
    spaces.extend(
        state
            .spaces
            .iter()
            .zip(1..=9u8)
            .map(|(name, number)| MenuItem::action(name.clone(), Browse(SelectSpace(number)))),
    );
    vec![
        // AppKit titles this menu with the bundle's name, "Map Browser".
        menu(
            "Map Browser",
            vec![
                MenuItem::action("About Map Browser", About),
                MenuItem::separator(),
                MenuItem::action("Settings…", Settings),
                MenuItem::action("Set as Default Browser…", SetDefaultBrowser),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Hide Map Browser", Hide),
                MenuItem::action("Hide Others", HideOthers),
                MenuItem::action("Show All", ShowAll),
                MenuItem::separator(),
                MenuItem::action("Quit Map Browser", Quit),
            ],
        ),
        menu(
            "File",
            vec![
                MenuItem::action("New Tab", Browse(CommandBar)),
                MenuItem::action("Open Location…", Browse(EditAddress)),
                MenuItem::action("Reopen Closed Tab", Browse(ReopenClosedTab)),
                MenuItem::separator(),
                MenuItem::action(close, Browse(CloseTab)),
                MenuItem::separator(),
                MenuItem::action("Print…", Browse(Print)),
            ],
        ),
        // Routed to whichever view has focus, the web page included. Undo
        // and Redo act on the shell's text field that has focus, else on
        // the page.
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
                MenuItem::action(
                    if state.sidebar_hidden {
                        "Show Sidebar"
                    } else {
                        "Hide Sidebar"
                    },
                    Browse(ToggleSidebar),
                ),
                MenuItem::separator(),
                MenuItem::action("Reload", Browse(Reload)),
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
            "History",
            vec![
                MenuItem::action("Show All History", ShowHistory),
                MenuItem::action("Reopen Closed Tab", Browse(ReopenClosedTab)),
                MenuItem::separator(),
                MenuItem::action("Clear History…", ClearHistory),
            ],
        ),
        menu("Spaces", spaces),
        menu(
            "Tab",
            vec![
                MenuItem::action(pin, Browse(TogglePin)),
                MenuItem::action("Copy URL", Browse(CopyUrl)),
                MenuItem::separator(),
                MenuItem::action("Next Tab", Browse(NextTabInList)),
                MenuItem::action("Previous Tab", Browse(PreviousTabInList)),
                MenuItem::separator(),
                MenuItem::action("Clear Today", Browse(ClearToday)),
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
        // The name makes it AppKit's Help menu, with its search field.
        menu(
            "Help",
            vec![MenuItem::action("Keyboard Shortcuts", ShowShortcuts)],
        ),
    ]
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let state = BrowserState::load()
        .unwrap_or_else(|error| launch_error::exit_for_session(&error, &model::data_dir()));
    if let Err(error) = state.remove_orphaned_profiles() {
        eprintln!("Couldn't remove a deleted space's data: {error}");
    }
    let blocker = ContentBlocker::load_in_background(state.root_dir().join("blocker"));
    let (mut engine, events) = Engine::initialize(state.root_dir()).unwrap_or_else(|error| {
        launch_error::exit("Map Browser couldn't start.", &format!("{error:#}"))
    });
    engine.set_blocker(blocker);
    let engine = Rc::new(RefCell::new(engine));
    let retained_engine = engine.clone();
    let (external_urls_tx, external_urls_rx) = async_channel::unbounded();
    // Previews have no Launch Services: addresses and files given on the
    // command line stand in for links and files opened from other apps.
    #[cfg(not(target_os = "macos"))]
    for argument in std::env::args().skip(1) {
        if let Some(url) = app::argument_address(&argument) {
            let _ = external_urls_tx.try_send(url);
        }
    }
    let application = Application::new().with_assets(ui::icons::Assets);
    // Links, and files from Finder, the Dock and `open -a`. Files arrive as
    // file URLs; `opened_address` keeps the kinds a browser shows.
    application.on_open_urls(move |urls| {
        for url in urls.iter().filter_map(|url| opened_address(url)) {
            let _ = external_urls_tx.try_send(url);
        }
    });
    // A click on the Dock icon with no window on screen (minimised, say)
    // brings the browser window back.
    application.on_reopen(|cx| {
        let browser = cx
            .windows()
            .into_iter()
            .find_map(|window| window.downcast::<BrowserApp>());
        if let Some(browser) = browser {
            let _ = browser.update(cx, |_, window, _| window.activate_window());
        }
        cx.activate(true);
    });
    application.run(move |cx: &mut App| {
        // First: of bindings for the same keys, GPUI picks the one added
        // last, so the text fields' and pop-ups' own bindings win.
        cx.bind_keys(key_bindings());
        input::init(cx);
        ui::command_bar::init(cx);
        ui::find_bar::init(cx);
        ui::menu::init(cx);
        ui::shortcuts::init(cx);
        ui::page_notice::init(cx);
        cx.on_action(|_: &About, _| native::show_about_panel());
        cx.on_action(|_: &Hide, cx| cx.hide());
        cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
        cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
        cx.on_action(|_: &BringAllToFront, _| native::bring_all_to_front());
        // The browser window keeps it up to date (`BrowserApp::sync_menu_bar`).
        cx.set_menus(menus(&MenuBarState::default()));
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
                    let handle = window.window_handle().downcast::<BrowserApp>();
                    window.on_window_should_close(cx, move |_, cx| {
                        if let Some(window) = handle {
                            cx.defer(move |cx| request_quit(window, cx));
                        }
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
        cx.on_action(move |_: &Quit, cx| cx.defer(move |cx| request_quit(window, cx)));
        cx.on_action(move |_: &NewSpace, cx| {
            cx.defer(move |cx| {
                let _ = window.update(cx, |app, window, cx| app.new_space(window, cx));
            })
        });
        cx.on_action(move |_: &SetDefaultBrowser, cx| {
            cx.defer(move |cx| {
                let _ = window.update(cx, |app, _, cx| app.set_default_browser(cx));
            })
        });
        cx.on_action(move |_: &ShowShortcuts, cx| {
            cx.defer(move |cx| {
                let _ = window.update(cx, |app, window, cx| app.show_shortcuts(window, cx));
            })
        });
        // History and settings are lists in the command bar.
        cx.on_action(move |_: &ShowHistory, cx| {
            cx.defer(move |cx| {
                let _ = window.update(cx, |app, window, cx| {
                    app.toggle_command_bar(CommandMode::History, window, cx)
                });
            })
        });
        cx.on_action(move |_: &Settings, cx| {
            cx.defer(move |cx| {
                let _ = window.update(cx, |app, window, cx| {
                    app.toggle_command_bar(CommandMode::Settings, window, cx)
                });
            })
        });
        cx.on_action(move |_: &ClearHistory, cx| {
            cx.defer(move |cx| {
                let _ = window.update(cx, |app, window, cx| app.ask_to_clear_history(window, cx));
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
