//! Which pages play sound. CEF tells only an audio handler that a page
//! became audible, and that handler then takes the sound away from the
//! speakers; it never tells that a page went quiet. Instead a short script
//! in each of a page's frames watches its audio and video elements and
//! reports, through a console message the display handler keeps to itself,
//! whether any of them plays audibly. It runs on media events only, with
//! no timer. Sound from Web Audio, or from elements outside the document,
//! isn't seen.

use cef::*;

/// Starts the script's reports: an invisible separator no page logs.
const MARKER: &str = "\u{2063}mapbrowser-audible ";

/// Reports "<MARKER><frame token> <0|1>" whenever the frame's elements
/// start or stop playing audibly. Runs once per document.
const SCRIPT: &str = r#"(() => {
  const key = Symbol.for("mapbrowser.audible");
  if (document[key]) return;
  document[key] = true;
  const debug = console.debug.bind(console);
  const token = Math.random().toString(36).slice(2);
  const media = new Set();
  let audible = false;
  const tell = (now) => {
    if (now === audible) return;
    audible = now;
    debug("⁣mapbrowser-audible " + token + " " + (now ? 1 : 0));
  };
  const report = () => tell([...media].some((element) =>
    !element.paused && !element.ended && !element.muted && element.volume > 0));
  const watch = (element) => {
    if (!(element instanceof HTMLMediaElement) || media.has(element)) return;
    media.add(element);
    for (const type of ["playing", "pause", "ended", "volumechange", "emptied"]) {
      element.addEventListener(type, report);
    }
  };
  document.addEventListener("play", (event) => { watch(event.target); report(); }, true);
  document.querySelectorAll("audio, video").forEach(watch);
  addEventListener("pagehide", () => tell(false));
  report();
})();"#;

/// Watches the media of the document `frame` just loaded. Only web pages:
/// Chromium's own pages (errors, the PDF viewer) play nothing.
pub(super) fn watch(frame: &Frame) {
    let url = CefString::from(&frame.url()).to_string();
    if url.starts_with("http:") || url.starts_with("https:") {
        frame.execute_java_script(
            Some(&CefString::from(SCRIPT)),
            Some(&CefString::from("")),
            0,
        );
    }
}

/// A report of the script: the frame's token and whether it plays sound.
/// Other console messages are left alone, without copying them.
pub(super) fn report(message: &CefString) -> Option<(String, bool)> {
    let utf16 = message.as_slice()?;
    let length = MARKER.encode_utf16().count();
    if utf16.len() < length || !MARKER.encode_utf16().zip(utf16).all(|(a, &b)| a == b) {
        return None;
    }
    let rest = String::from_utf16_lossy(&utf16[length..]);
    let (token, state) = rest.split_once(' ')?;
    Some((token.to_owned(), state == "1"))
}
