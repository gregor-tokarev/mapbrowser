//! Errors that stop the app as it launches. An app opened from Finder or
//! the Dock has nowhere to print them, so on macOS they show in an alert
//! before it quits; previews print them only.

use std::{io, path::Path};

/// The saved session in `dir` couldn't be read. Its files stay as they are.
pub fn exit_for_session(error: &io::Error, dir: &Path) -> ! {
    let (message, detail) = session_text(error, dir);
    exit(message, &detail)
}

fn session_text(error: &io::Error, dir: &Path) -> (&'static str, String) {
    if error.kind() == io::ErrorKind::Unsupported {
        (
            "This session was created by a newer version of Map Browser.",
            format!(
                "Install the latest version to open it. Your spaces and tabs are kept as they are in {}.",
                dir.display()
            ),
        )
    } else {
        (
            "Map Browser couldn't open its saved session.",
            format!("{error}\n\nFolder: {}", dir.display()),
        )
    }
}

/// Says why the app can't start, then exits with a failure status.
pub fn exit(message: &str, detail: &str) -> ! {
    eprintln!("{message}\n{detail}");
    #[cfg(target_os = "macos")]
    alert(message, detail);
    std::process::exit(1)
}

/// A modal alert, before the app has a window or runs its event loop.
#[cfg(target_os = "macos")]
fn alert(message: &str, detail: &str) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSAlert, NSAlertStyle, NSApplication, NSApplicationActivationPolicy};
    use objc2_foundation::NSString;
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    // Launched from a terminal, the process isn't a regular app yet.
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(NSAlertStyle::Critical);
    alert.setMessageText(&NSString::from_str(message));
    alert.setInformativeText(&NSString::from_str(detail));
    alert.runModal();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_sessions_ask_for_the_latest_version_and_others_show_the_error() {
        let dir = Path::new("/Users/me/Library/Application Support/MapBrowser");
        let newer = io::Error::new(io::ErrorKind::Unsupported, "newer");
        let (message, detail) = session_text(&newer, dir);
        assert!(message.contains("newer version of Map Browser"));
        assert!(detail.contains("Install the latest version"));
        assert!(detail.contains("Application Support/MapBrowser"));
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        let (message, detail) = session_text(&denied, dir);
        assert_eq!(message, "Map Browser couldn't open its saved session.");
        assert!(detail.starts_with(&denied.to_string()));
        assert!(detail.ends_with("Folder: /Users/me/Library/Application Support/MapBrowser"));
    }
}
