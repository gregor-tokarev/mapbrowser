//! Which pages play sound. CEF tells only an audio handler that a page
//! became audible, and that handler then takes the sound away from the
//! speakers; it never tells that a page went quiet. Instead a short script
//! in each of a page's frames watches its audio and video elements and
//! reports, through a console message the display handler keeps to itself,
//! whether any of them plays audibly. It runs on media events only, with
//! no timer, and holds only the elements playing now. Sound from Web Audio,
//! or from elements outside the document, isn't seen.
//!
//! Each frame's script gets a token from the browser when it is injected,
//! and reports with any other token are ignored, so a page can't grow what
//! the browser keeps nor speak for another frame. A page keeps the tokens
//! of at most `MAX_FRAMES` frames, until its next document.

use cef::*;
use std::{
    collections::{HashMap, HashSet, hash_map::RandomState},
    hash::BuildHasher,
    sync::atomic::{AtomicU64, Ordering},
};

/// Starts the script's reports: an invisible separator no page logs.
const MARKER: &str = "\u{2063}mapbrowser-audible ";

/// The most frames of one page whose sound is followed. Pages with more
/// (ad-heavy ones) lose a silent frame's reports to make room.
const MAX_FRAMES: usize = 64;

/// Reports "<MARKER><TOKEN> <0|1>" whenever the frame's elements start or
/// stop playing audibly. An element is held only while it plays: pausing,
/// ending, emptying or leaving the document (which pauses it) lets it go.
/// Its own listeners stay, as a removed element's events no longer reach
/// the document; adding the same listener twice adds it once. Every
/// listener goes when the page is hidden: a page kept in the back-forward
/// cache keeps its scripts, and the browser runs a fresh copy with a new
/// token when the page comes back, so copies never pile up.
const SCRIPT: &str = r#"(() => {
  const debug = console.debug.bind(console);
  const off = new AbortController();
  const signal = off.signal;
  const playing = new Set();
  let audible = false;
  const tell = (now) => {
    if (now === audible) return;
    audible = now;
    debug("\u2063mapbrowser-audible TOKEN " + (now ? 1 : 0));
  };
  const report = () => tell([...playing].some((element) =>
    !element.muted && element.volume > 0));
  const changed = (event) => {
    if (event.type === "playing") playing.add(event.target);
    else if (event.type !== "volumechange") playing.delete(event.target);
    report();
  };
  const watch = (element) => {
    if (!(element instanceof HTMLMediaElement)) return;
    for (const type of ["playing", "pause", "ended", "emptied", "volumechange"]) {
      element.addEventListener(type, changed, { signal });
    }
    if (!element.paused && !element.ended) playing.add(element);
  };
  document.addEventListener("play", (event) => { watch(event.target); report(); },
    { capture: true, signal });
  document.querySelectorAll("audio, video").forEach(watch);
  addEventListener("pagehide", () => {
    tell(false);
    off.abort();
    playing.clear();
  }, { signal });
  report();
})();"#;

/// The frames of one page that report their sound, and which play it.
#[derive(Default)]
pub(super) struct PageAudio {
    /// The token each frame's script reports with, by frame identifier.
    tokens: HashMap<String, String>,
    /// The tokens of the frames that play sound.
    audible: HashSet<String>,
}

impl PageAudio {
    pub(super) fn is_audible(&self) -> bool {
        !self.audible.is_empty()
    }

    /// The frame `frame` loaded a document: its last token ends, and the
    /// script for the new one is returned, if it is a web page. Chromium's
    /// own pages (errors, the PDF viewer) play nothing.
    pub(super) fn loaded(&mut self, frame: String, url: &str) -> Option<String> {
        if let Some(token) = self.tokens.remove(&frame) {
            self.audible.remove(&token);
        }
        if !(url.starts_with("http:") || url.starts_with("https:")) {
            return None;
        }
        if self.tokens.len() >= MAX_FRAMES {
            let silent = self
                .tokens
                .iter()
                .find(|(_, token)| !self.audible.contains(*token))
                .map(|(frame, _)| frame.clone())?;
            self.tokens.remove(&silent);
        }
        let token = new_token();
        let script = SCRIPT.replace("TOKEN", &token);
        self.tokens.insert(frame, token);
        Some(script)
    }

    /// Takes a script's report; true if the page's sound changed with it.
    pub(super) fn report(&mut self, token: &str, audible: bool) -> bool {
        if !self.tokens.values().any(|issued| issued == token) {
            return false;
        }
        let before = self.is_audible();
        if audible {
            self.audible.insert(token.to_owned());
        } else {
            self.audible.remove(token);
        }
        before != self.is_audible()
    }

    /// The page's document went, or its renderer: every token ends. True
    /// if it played sound.
    pub(super) fn clear(&mut self) -> bool {
        self.tokens.clear();
        !std::mem::take(&mut self.audible).is_empty()
    }
}

/// A token no page can guess: hashed with the process's random keys.
fn new_token() -> String {
    static ISSUED: AtomicU64 = AtomicU64::new(0);
    let token = RandomState::new().hash_one(ISSUED.fetch_add(1, Ordering::Relaxed));
    format!("{token:016x}")
}

/// Runs `script` from `PageAudio::loaded` in `frame`.
pub(super) fn run(frame: &Frame, script: &str) {
    frame.execute_java_script(
        Some(&CefString::from(script)),
        Some(&CefString::from("")),
        0,
    );
}

/// A report in the form the script sends: a token and whether its frame
/// plays sound. Other console messages are left alone, without copying
/// them.
pub(super) fn report(message: &CefString) -> Option<(String, bool)> {
    let utf16 = message.as_slice()?;
    let length = MARKER.encode_utf16().count();
    if utf16.len() < length || !MARKER.encode_utf16().zip(utf16).all(|(a, &b)| a == b) {
        return None;
    }
    let rest = String::from_utf16_lossy(&utf16[length..]);
    let (token, state) = rest.split_once(' ')?;
    let audible = match state {
        "1" => true,
        "0" => false,
        _ => return None,
    };
    let well_formed = token.len() == 16 && token.bytes().all(|byte| byte.is_ascii_hexdigit());
    well_formed.then(|| (token.to_owned(), audible))
}
