//! Browser approval for a Teams connection link. One attempt runs at a time and
//! owns its dialogs, so the approval prompt closes as soon as pairing ends and a
//! repeated link brings the same attempt forward instead of starting another.

use adw::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// What pairing needs from the window it runs in. Tests supply their own.
pub(super) struct PairingHooks {
    pub parent: Box<dyn Fn() -> Option<gtk::Window>>,
    pub feedback: Box<dyn Fn(&str, bool)>,
    /// Show the connected Teams view.
    pub connected: Box<dyn Fn()>,
    pub open_url: Box<dyn Fn(&str)>,
}

/// Runs the pairing protocol on a worker thread: the cancellation flag, then a
/// callback for the browser URL and device check.
pub(super) type PairFn =
    Box<dyn FnOnce(&AtomicBool, &dyn Fn(&str, &str)) -> Result<(), String> + Send>;

enum Event {
    Challenge { url: String, check: String },
    Finished(Result<(), String>),
}

struct Attempt {
    hooks: PairingHooks,
    cancel: Arc<AtomicBool>,
    check: RefCell<Option<String>>,
    /// The origin confirmation, until it is answered.
    confirm: RefCell<Option<adw::MessageDialog>>,
    /// The approval prompt, while it is shown.
    pending: RefCell<Option<adw::MessageDialog>>,
}

thread_local! {
    static CURRENT: RefCell<Option<Rc<Attempt>>> = const { RefCell::new(None) };
}

fn current() -> Option<Rc<Attempt>> {
    CURRENT.with(|current| current.borrow().clone())
}

fn clear(attempt: &Rc<Attempt>) {
    CURRENT.with(|current| {
        let mut current = current.borrow_mut();
        if current.as_ref().is_some_and(|c| Rc::ptr_eq(c, attempt)) {
            *current = None;
        }
    });
}

/// Whether an attempt is waiting for confirmation or browser approval.
#[cfg(test)]
pub(super) fn is_pending() -> bool {
    current().is_some()
}

/// Ask to connect to `origin`, then pair through the browser. While another
/// attempt is waiting, its dialog comes forward instead and nothing new starts.
#[allow(deprecated)]
pub(super) fn request(hooks: PairingHooks, origin: &str, pair: PairFn) {
    if let Some(attempt) = current() {
        attempt.bring_forward();
        return;
    }
    let Some(parent) = (hooks.parent)() else {
        return;
    };
    let attempt = Rc::new(Attempt {
        hooks,
        cancel: Arc::new(AtomicBool::new(false)),
        check: RefCell::new(None),
        confirm: RefCell::new(None),
        pending: RefCell::new(None),
    });
    CURRENT.with(|current| *current.borrow_mut() = Some(Rc::clone(&attempt)));
    let dialog = adw::MessageDialog::new(
        Some(&parent),
        Some("Sign in to sync?"),
        Some(&crate::teams::pairing_confirm_copy(
            origin,
            crate::registry::load().is_ok_and(|r| r.team.is_some()),
        )),
    );
    dialog.set_size_request(520, -1);
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("connect", "Continue to browser");
    dialog.set_close_response("cancel");
    let pair = RefCell::new(Some(pair));
    let owner = Rc::clone(&attempt);
    dialog.connect_response(None, move |dialog, response| {
        // Closing a message dialog emits its close response too. Only the first
        // answer counts; the pairing closure is taken once.
        let Some(pair) = pair.borrow_mut().take() else {
            return;
        };
        owner.confirm.borrow_mut().take();
        if response == "connect" {
            owner.start(pair);
        } else {
            clear(&owner);
        }
        dialog.close();
    });
    *attempt.confirm.borrow_mut() = Some(dialog.clone());
    dialog.present();
}

impl Attempt {
    fn bring_forward(self: &Rc<Self>) {
        if let Some(dialog) = self.confirm.borrow().as_ref() {
            dialog.present();
            return;
        }
        if self.check.borrow().is_some() {
            self.show_pending();
        } else {
            (self.hooks.feedback)(
                "Sign-in is starting. Wait for the browser approval page.",
                false,
            );
        }
    }

    fn start(self: &Rc<Self>, pair: PairFn) {
        let (sender, events) = std::sync::mpsc::channel::<Event>();
        let cancel = Arc::clone(&self.cancel);
        std::thread::spawn(move || {
            let challenge = sender.clone();
            let result = pair(&cancel, &move |url: &str, check: &str| {
                let _ = challenge.send(Event::Challenge {
                    url: url.into(),
                    check: check.into(),
                });
            });
            let _ = sender.send(Event::Finished(result));
        });
        let attempt = Rc::clone(self);
        gtk::glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            while let Ok(event) = events.try_recv() {
                match event {
                    Event::Challenge { url, check } => {
                        (attempt.hooks.feedback)(
                            &format!("Browser approval pending. Match device check {check}."),
                            false,
                        );
                        *attempt.check.borrow_mut() = Some(check);
                        attempt.show_pending();
                        (attempt.hooks.open_url)(&url);
                    }
                    Event::Finished(result) => {
                        attempt.finish(result);
                        return gtk::glib::ControlFlow::Break;
                    }
                }
            }
            gtk::glib::ControlFlow::Continue
        });
    }

    #[allow(deprecated)]
    fn show_pending(self: &Rc<Self>) {
        if let Some(dialog) = self.pending.borrow().as_ref() {
            dialog.present();
            return;
        }
        let Some(check) = self.check.borrow().clone() else {
            return;
        };
        let Some(parent) = (self.hooks.parent)() else {
            return;
        };
        let dialog = adw::MessageDialog::new(Some(&parent), Some("Approve this device in your browser"), Some(&format!("Device check: {check}\n\nApprove only if the browser shows this same check, the intended setup and your account. This request expires in five minutes. You can hide this message; Toolport finishes connecting when you approve.")));
        dialog.set_size_request(460, -1);
        dialog.add_response("cancel", "Cancel request");
        dialog.add_response("hide", "Hide");
        dialog.set_response_appearance("cancel", adw::ResponseAppearance::Destructive);
        dialog.set_close_response("hide");
        dialog.set_default_response(Some("hide"));
        let owner = Rc::downgrade(self);
        dialog.connect_response(None, move |dialog, response| {
            if let Some(owner) = owner.upgrade() {
                owner.pending.borrow_mut().take();
                if response == "cancel" {
                    owner.cancel.store(true, Ordering::SeqCst);
                    (owner.hooks.feedback)("Cancelling the connection request…", false);
                }
            }
            dialog.close();
        });
        *self.pending.borrow_mut() = Some(dialog.clone());
        dialog.present();
    }

    /// Pairing ended. Close this attempt's prompt, never another dialog, and say how
    /// it ended.
    #[allow(deprecated)]
    fn finish(self: &Rc<Self>, result: Result<(), String>) {
        clear(self);
        // Release the borrow first: closing emits the dialog's close response.
        let pending = self.pending.borrow_mut().take();
        if let Some(dialog) = pending {
            dialog.close();
        }
        match result {
            Ok(()) => {
                (self.hooks.feedback)("Toolport is signed in to sync.", false);
                (self.hooks.connected)();
            }
            // Only the cancellation itself; a real failure after a late cancel
            // still says what went wrong.
            Err(error) if error == crate::teams::PAIRING_CANCELLED => {
                (self.hooks.feedback)(crate::teams::PAIRING_CANCELLED, false);
            }
            Err(error) => {
                (self.hooks.feedback)(&error, true);
                let Some(parent) = (self.hooks.parent)() else {
                    return;
                };
                let dialog = adw::MessageDialog::new(
                    Some(&parent),
                    Some("Connection not completed"),
                    Some(&format!("{error}\n\nNothing was connected. Start again from the sync website when you are ready.")),
                );
                dialog.set_size_request(460, -1);
                dialog.add_response("close", "Close");
                dialog.set_close_response("close");
                dialog.connect_response(None, |dialog, _| dialog.close());
                dialog.present();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;

    /// A pairing stand-in: sends the challenge, then waits for the test to decide
    /// the result, returning early if the request is cancelled.
    fn scripted(started: Arc<AtomicUsize>, outcome: mpsc::Receiver<Result<(), String>>) -> PairFn {
        Box::new(move |cancel, show| {
            started.fetch_add(1, Ordering::SeqCst);
            show("https://teams.example.test/#pair=synthetic", "4b6db433");
            loop {
                if cancel.load(Ordering::SeqCst) {
                    return Err(crate::teams::PAIRING_CANCELLED.into());
                }
                if let Ok(result) = outcome.recv_timeout(std::time::Duration::from_millis(20)) {
                    return result;
                }
            }
        })
    }

    #[derive(Default)]
    struct Seen {
        feedback: RefCell<Vec<(String, bool)>>,
        connected: RefCell<usize>,
        opened: RefCell<Vec<String>>,
    }

    fn hooks(parent: &gtk::Window, seen: &Rc<Seen>) -> PairingHooks {
        let parent = parent.clone();
        let (feedback, connected, opened) = (Rc::clone(seen), Rc::clone(seen), Rc::clone(seen));
        PairingHooks {
            parent: Box::new(move || Some(parent.clone())),
            feedback: Box::new(move |message, error| {
                feedback.feedback.borrow_mut().push((message.into(), error))
            }),
            connected: Box::new(move || *connected.connected.borrow_mut() += 1),
            open_url: Box::new(move |url| opened.opened.borrow_mut().push(url.into())),
        }
    }

    fn pump_until(label: &str, done: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !done() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {label}"
            );
            while gtk::glib::MainContext::default().iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[allow(deprecated)]
    fn visible_dialogs(heading: &str) -> usize {
        gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<adw::MessageDialog>().ok())
            .filter(|d| d.is_visible() && d.heading().as_deref() == Some(heading))
            .count()
    }

    #[allow(deprecated)]
    fn respond(heading: &str, response: &str) {
        let dialog = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<adw::MessageDialog>().ok())
            .find(|d| d.is_visible() && d.heading().as_deref() == Some(heading))
            .unwrap_or_else(|| panic!("no visible dialog {heading}"));
        dialog.response(response);
    }

    const CONFIRM: &str = "Sign in to sync?";
    const APPROVE: &str = "Approve this device in your browser";
    const FAILED: &str = "Connection not completed";

    /// Confirm the origin and wait for the approval prompt.
    fn start(
        parent: &gtk::Window,
        seen: &Rc<Seen>,
    ) -> (Arc<AtomicUsize>, mpsc::Sender<Result<(), String>>) {
        let started = Arc::new(AtomicUsize::new(0));
        let (finish, outcome) = mpsc::channel();
        request(
            hooks(parent, seen),
            "https://teams.example.test",
            scripted(Arc::clone(&started), outcome),
        );
        assert_eq!(visible_dialogs(CONFIRM), 1);
        respond(CONFIRM, "connect");
        pump_until("the approval prompt", || visible_dialogs(APPROVE) == 1);
        (started, finish)
    }

    fn settle() {
        pump_until("the attempt to end", || !is_pending());
        for _ in 0..20 {
            while gtk::glib::MainContext::default().iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    #[allow(deprecated)]
    fn the_approval_prompt_follows_the_pairing_attempt() {
        adw::init().unwrap();
        let parent = gtk::Window::builder()
            .title("Toolport")
            .default_width(900)
            .default_height(700)
            .build();
        parent.present();

        // Approval succeeds: the prompt closes and Teams opens.
        let seen = Rc::new(Seen::default());
        let (started, finish) = start(&parent, &seen);
        assert_eq!(
            seen.opened.borrow().as_slice(),
            ["https://teams.example.test/#pair=synthetic"]
        );
        finish.send(Ok(())).unwrap();
        settle();
        assert_eq!(
            visible_dialogs(APPROVE),
            0,
            "the approval prompt stayed open"
        );
        assert_eq!(*seen.connected.borrow(), 1);
        assert_eq!(started.load(Ordering::SeqCst), 1);
        assert_eq!(
            seen.feedback.borrow().last().unwrap(),
            &("Toolport is signed in to sync.".to_string(), false)
        );

        // Pairing fails, or the request expires: a terminal result replaces the prompt.
        for error in [
            "server returned 502: bad gateway",
            "Connection request expired. Choose Connect Toolport again.",
        ] {
            let seen = Rc::new(Seen::default());
            let (_, finish) = start(&parent, &seen);
            finish.send(Err(error.into())).unwrap();
            settle();
            assert_eq!(visible_dialogs(APPROVE), 0, "{error}");
            assert_eq!(visible_dialogs(FAILED), 1, "{error}");
            assert_eq!(*seen.connected.borrow(), 0);
            assert_eq!(
                seen.feedback.borrow().last().unwrap(),
                &(error.to_string(), true)
            );
            respond(FAILED, "close");
            pump_until("the result to close", || visible_dialogs(FAILED) == 0);
        }

        // Hidden before approval: pairing continues, and a repeated link brings the
        // same prompt back without starting another attempt.
        let seen = Rc::new(Seen::default());
        let (started, finish) = start(&parent, &seen);
        respond(APPROVE, "hide");
        pump_until("the prompt to hide", || visible_dialogs(APPROVE) == 0);
        assert!(is_pending());
        let unused = Arc::new(AtomicUsize::new(0));
        let (_, never) = mpsc::channel();
        request(
            hooks(&parent, &seen),
            "https://teams.example.test",
            scripted(Arc::clone(&unused), never),
        );
        pump_until("the prompt to return", || visible_dialogs(APPROVE) == 1);
        assert_eq!(visible_dialogs(CONFIRM), 0, "a repeated link asked again");
        request(
            hooks(&parent, &seen),
            "https://teams.example.test",
            scripted(Arc::clone(&unused), mpsc::channel().1),
        );
        pump_until("one prompt", || visible_dialogs(APPROVE) == 1);
        respond(APPROVE, "hide");
        pump_until("the prompt to hide again", || visible_dialogs(APPROVE) == 0);
        finish.send(Ok(())).unwrap();
        settle();
        assert_eq!(
            visible_dialogs(APPROVE),
            0,
            "a hidden prompt came back after success"
        );
        assert_eq!(*seen.connected.borrow(), 1);
        assert_eq!(started.load(Ordering::SeqCst), 1);
        assert_eq!(
            unused.load(Ordering::SeqCst),
            0,
            "a repeated link started a second attempt"
        );

        // A repeated link while the origin confirmation is open shows that one.
        let seen = Rc::new(Seen::default());
        let (_, outcome) = mpsc::channel();
        request(
            hooks(&parent, &seen),
            "https://teams.example.test",
            scripted(Arc::clone(&unused), outcome),
        );
        request(
            hooks(&parent, &seen),
            "https://teams.example.test",
            scripted(Arc::clone(&unused), mpsc::channel().1),
        );
        assert_eq!(visible_dialogs(CONFIRM), 1);
        respond(CONFIRM, "cancel");
        settle();
        assert_eq!(visible_dialogs(CONFIRM), 0);
        assert_eq!(unused.load(Ordering::SeqCst), 0);

        // Cancelled from the prompt: pairing stops and nothing reports an error.
        let seen = Rc::new(Seen::default());
        let (_, _finish) = start(&parent, &seen);
        respond(APPROVE, "cancel");
        settle();
        assert_eq!(visible_dialogs(APPROVE), 0);
        assert_eq!(visible_dialogs(FAILED), 0);
        assert_eq!(*seen.connected.borrow(), 0);
        assert_eq!(
            seen.feedback.borrow().last().unwrap(),
            &(crate::teams::PAIRING_CANCELLED.to_string(), false)
        );

        // An unrelated dialog stays open through a whole attempt.
        let unrelated = adw::MessageDialog::new(Some(&parent), Some("Unrelated"), None);
        unrelated.add_response("close", "Close");
        unrelated.present();
        let seen = Rc::new(Seen::default());
        let (_, finish) = start(&parent, &seen);
        finish.send(Ok(())).unwrap();
        settle();
        assert_eq!(visible_dialogs("Unrelated"), 1);
        unrelated.close();

        if std::env::var_os("TOOLPORT_PAIRING_CAPTURE").is_some() {
            // Leave a pending prompt up for a screenshot, then let it succeed.
            let seen = Rc::new(Seen::default());
            let (_, finish) = start(&parent, &seen);
            let capture = std::env::var("TOOLPORT_PAIRING_CAPTURE").unwrap();
            std::fs::write(format!("{capture}.pending"), b"").unwrap();
            pump_until("the capture signal", || {
                std::path::Path::new(&format!("{capture}.approve")).exists()
            });
            finish.send(Ok(())).unwrap();
            settle();
            std::fs::write(format!("{capture}.done"), b"").unwrap();
            pump_until("the capture to finish", || {
                std::path::Path::new(&format!("{capture}.exit")).exists()
            });
        }
        parent.close();
    }
}
