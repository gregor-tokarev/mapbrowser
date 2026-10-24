//! Questions a page's loads wait on: a sign-in (HTTP authentication), a
//! certificate that didn't verify, and "Leave site?". Like permission
//! requests the shell answers them by id, and each ends with its page: a
//! new navigation, a crash or closing dismisses it (`RequestDismissed`).
//! A page that stopped responding waits here too, until the user decides.
//!
//! Sign-ins arrive on Chromium's IO thread, the others on the UI thread.
//! CEF's callbacks may be answered on any thread, and an answer can call
//! straight back into a handler, so none is answered with the lock held.
//! Nor is one dropped with it: the last reference to an unanswered
//! certificate or "Leave site?" callback answers it as it goes, at once on
//! the UI thread, and a "Leave site?" answered that way resets the page's
//! dialogs, which comes straight back here.

use super::BrowserLink;
use crate::engine::EngineEvent;
use cef::*;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
};

#[derive(Clone, Default)]
pub(super) struct PageRequests(Arc<Mutex<Requests>>);

#[derive(Default)]
struct Requests {
    next_id: u64,
    pending: HashMap<u64, Pending>,
    /// Pages that stopped responding, by browser instance.
    unresponsive: HashMap<u64, UnresponsiveProcessCallback>,
}

struct Pending {
    instance: u64,
    answer: Answer,
}

pub(super) enum Answer {
    SignIn(AuthCallback),
    /// `may_continue`: the card offers Continue for this error and host
    /// (`certificate_continue_allowed`); without it the load never goes on.
    Certificate {
        callback: Callback,
        may_continue: bool,
    },
    Leave(JsdialogCallback),
}

impl Answer {
    /// Ends the question without the user: no sign-in, no exception for
    /// the certificate. A "Leave site?" left alone is CEF's to end, which
    /// it does as the dialog's page goes.
    fn give_up(self) {
        match self {
            Answer::SignIn(callback) => callback.cancel(),
            Answer::Certificate { callback, .. } => callback.cancel(),
            Answer::Leave(_) => {}
        }
    }
}

impl PageRequests {
    fn requests(&self) -> MutexGuard<'_, Requests> {
        self.0.lock().expect("page requests")
    }

    /// Keeps `answer` for the page in `link`'s browser; the caller tells
    /// the shell, with the id returned.
    pub(super) fn add(&self, link: &BrowserLink, answer: Answer) -> u64 {
        let mut requests = self.requests();
        requests.next_id += 1;
        let id = requests.next_id;
        requests.pending.insert(
            id,
            Pending {
                instance: link.instance,
                answer,
            },
        );
        id
    }

    fn take(&self, id: u64) -> Option<Pending> {
        self.requests().pending.remove(&id)
    }

    /// Signs in with `credentials`, or cancels without them.
    pub(super) fn respond_sign_in(&self, id: u64, credentials: Option<(&str, &str)>) {
        if let Some(Pending {
            answer: Answer::SignIn(callback),
            ..
        }) = self.take(id)
        {
            match credentials {
                Some((username, password)) => callback.cont(
                    Some(&CefString::from(username)),
                    Some(&CefString::from(password)),
                ),
                None => callback.cancel(),
            }
        }
    }

    pub(super) fn respond_certificate(&self, id: u64, proceed: bool) {
        if let Some(Pending {
            answer:
                Answer::Certificate {
                    callback,
                    may_continue,
                },
            ..
        }) = self.take(id)
        {
            if proceed && may_continue {
                callback.cont();
            } else {
                callback.cancel();
            }
        }
    }

    /// Leaves the page or stays; returns its browser instance.
    pub(super) fn respond_leave(&self, id: u64, leave: bool) -> Option<u64> {
        let Some(Pending {
            instance,
            answer: Answer::Leave(callback),
        }) = self.take(id)
        else {
            return None;
        };
        callback.cont(i32::from(leave), None);
        Some(instance)
    }

    /// Ends the questions of the page in a browser instance, once it
    /// crashed, closed or goes to another page, and tells the shell. With
    /// `leave`, a "Leave site?" is answered: the page is closing anyway.
    pub(super) fn drop_page(&self, link: &BrowserLink, leave: bool) {
        let ended: Vec<(u64, Pending)> = self
            .requests()
            .pending
            .extract_if(|_, pending| pending.instance == link.instance)
            .collect();
        for (id, pending) in ended {
            match pending.answer {
                Answer::Leave(callback) if leave => callback.cont(1, None),
                answer => answer.give_up(),
            }
            link.send(EngineEvent::RequestDismissed { id });
        }
    }

    /// Ends the page's "Leave site?" without an answer: CEF reset its
    /// dialogs (the page navigated or closed). The callbacks go once the
    /// lock is released: dropping one can reset the dialogs again, and the
    /// call that brings back here then finds nothing.
    pub(super) fn drop_leave(&self, link: &BrowserLink) {
        let ended: Vec<(u64, Pending)> = self
            .requests()
            .pending
            .extract_if(|_, pending| {
                pending.instance == link.instance && matches!(pending.answer, Answer::Leave(_))
            })
            .collect();
        for (id, pending) in ended {
            drop(pending);
            link.send(EngineEvent::RequestDismissed { id });
        }
    }

    /// Quitting: every page may go, and nobody is left to sign in.
    pub(super) fn leave_all(&self) {
        let pending = std::mem::take(&mut self.requests().pending);
        for (_, pending) in pending {
            match pending.answer {
                Answer::Leave(callback) => callback.cont(1, None),
                answer => answer.give_up(),
            }
        }
    }

    /// Keeps, or with `None` drops, the callback of a page that stopped
    /// responding. True if one was kept before; it goes once the lock is
    /// released.
    pub(super) fn set_unresponsive(
        &self,
        instance: u64,
        callback: Option<UnresponsiveProcessCallback>,
    ) -> bool {
        let replaced = {
            let mut requests = self.requests();
            match callback {
                Some(callback) => requests.unresponsive.insert(instance, callback),
                None => requests.unresponsive.remove(&instance),
            }
        };
        replaced.is_some()
    }

    /// Waits for the page that stopped responding, or ends it (a crash
    /// follows).
    pub(super) fn respond_unresponsive(&self, instance: u64, wait: bool) {
        let callback = self.requests().unresponsive.remove(&instance);
        if let Some(callback) = callback {
            if wait {
                callback.wait();
            } else {
                callback.terminate();
            }
        }
    }
}
