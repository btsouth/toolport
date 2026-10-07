use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::LazyLock;
use std::time::Duration;

use adw::prelude::*;
use image::GenericImageView;
use ksni::blocking::TrayMethods;

/// Written by the approval page on every poll, read by the tray timer. The
/// count must be visible with the window hidden - that is the whole point of a
/// tray - so it lives here rather than in any page state.
static PENDING_APPROVALS: AtomicUsize = AtomicUsize::new(0);

pub(super) fn set_pending(count: usize) {
    PENDING_APPROVALS.store(count, Ordering::Relaxed);
}

enum Command {
    Show,
    ShowApprovals,
    Quit,
}

pub(super) struct ToolportTray {
    commands: mpsc::Sender<Command>,
    pending: usize,
}

impl ksni::Tray for ToolportTray {
    fn id(&self) -> String {
        "toolport".into()
    }

    fn title(&self) -> String {
        match self.pending {
            0 => "Toolport".into(),
            1 => "Toolport - 1 request awaiting action".into(),
            n => format!("Toolport - {n} requests awaiting action"),
        }
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.title(),
            ..Default::default()
        }
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        static ICON: LazyLock<ksni::Icon> = LazyLock::new(|| {
            let image = image::load_from_memory_with_format(
                include_bytes!("../../icons/32x32.png"),
                image::ImageFormat::Png,
            )
            .expect("the bundled Toolport tray icon is a valid PNG");
            let (width, height) = image.dimensions();
            let mut data = image.into_rgba8().into_vec();
            for pixel in data.chunks_exact_mut(4) {
                pixel.rotate_right(1);
            }
            ksni::Icon {
                width: width as i32,
                height: height as i32,
                data,
            }
        });
        vec![ICON.clone()]
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.commands.send(Command::Show);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{MenuItem, StandardItem};

        vec![
            StandardItem {
                label: "Open Toolport".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.commands.send(Command::Show);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: format!("Pending approvals ({})", self.pending),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.commands.send(Command::ShowApprovals);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit Toolport".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.commands.send(Command::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Whether a StatusNotifierWatcher actually owns its bus name right now. The
/// tray itself is spawned with `assume_sni_available` so the icon appears when
/// a host shows up later, but hidden launches must not trust that assumption:
/// on a desktop with no SNI host (stock GNOME) a hidden window with no tray
/// icon is unreachable.
pub(super) fn sni_watcher_present() -> bool {
    let Ok(connection) =
        gtk::gio::bus_get_sync(gtk::gio::BusType::Session, gtk::gio::Cancellable::NONE)
    else {
        return false;
    };
    connection
        .call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            Some(&("org.kde.StatusNotifierWatcher",).into()),
            Some(gtk::glib::VariantTy::new("(b)").expect("static variant type")),
            gtk::gio::DBusCallFlags::NONE,
            1000,
            gtk::gio::Cancellable::NONE,
        )
        .ok()
        .and_then(|reply| reply.child_value(0).get::<bool>())
        .unwrap_or(false)
}

/// How long a hidden launch waits for a tray host before showing the window.
/// At login the host (the Omarchy shell, Waybar) often starts in the same
/// second as Toolport and registers its watcher a moment later.
pub(super) const HOST_GRACE: Duration = Duration::from_secs(15);

/// A hidden launch found no tray host yet. Keep the window hidden while the host
/// may still be starting, and present it if none appears within `grace` so the
/// app never sits running with no way to reach it.
pub(super) fn present_unless_host_appears(app: &adw::Application, grace: Duration) {
    let poll = Duration::from_millis(500);
    let mut remaining = grace.as_millis() / poll.as_millis();
    let app = app.clone();
    gtk::glib::timeout_add_local(poll, move || {
        if sni_watcher_present() {
            return gtk::glib::ControlFlow::Break;
        }
        remaining = remaining.saturating_sub(1);
        if remaining > 0 {
            return gtk::glib::ControlFlow::Continue;
        }
        if let Some(window) = app.windows().into_iter().next() {
            window.present();
        }
        gtk::glib::ControlFlow::Break
    });
}

pub(super) fn start(app: &adw::Application) -> Option<ksni::blocking::Handle<ToolportTray>> {
    let (sender, receiver) = mpsc::channel();
    let handle = ToolportTray {
        commands: sender,
        pending: 0,
    }
    .assume_sni_available(true)
    .spawn()
    .ok()?;
    let app = app.clone();
    let handle_for_updates = handle.clone();
    let mut shown_pending = 0usize;
    gtk::glib::timeout_add_local(Duration::from_millis(100), move || {
        for command in receiver.try_iter() {
            match command {
                Command::Show => app.activate(),
                Command::ShowApprovals => {
                    app.activate();
                    if let Some(action) = app.lookup_action("show-servers") {
                        action.activate(None);
                    }
                }
                Command::Quit => app.quit(),
            }
        }
        let pending = PENDING_APPROVALS.load(Ordering::Relaxed);
        if pending != shown_pending {
            shown_pending = pending;
            let _ = handle_for_updates.update(move |tray| tray.pending = pending);
        }
        gtk::glib::ControlFlow::Continue
    });
    Some(handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_uses_the_embedded_icon_without_a_theme_lookup() {
        let (commands, _receiver) = mpsc::channel();
        let tray = ToolportTray {
            commands,
            pending: 0,
        };
        assert!(ksni::Tray::icon_name(&tray).is_empty());
        let icons = ksni::Tray::icon_pixmap(&tray);
        assert_eq!(icons.len(), 1);
        assert_eq!((icons[0].width, icons[0].height), (32, 32));
        assert_eq!(icons[0].data.len(), 32 * 32 * 4);
    }

    /// The tray menu is drawn by the host's icon theme, not by Toolport's CSS.
    /// Omarchy's Yaru-sage has no `window-new` or `security-high`, so named
    /// icons rendered as fallback squares next to a loud red exit glyph. The
    /// menu stays text-only so it cannot break on whatever theme is active.
    #[test]
    fn tray_menu_items_do_not_name_theme_icons() {
        let (commands, _receiver) = mpsc::channel();
        let tray = ToolportTray {
            commands,
            pending: 0,
        };
        for item in ksni::Tray::menu(&tray) {
            if let ksni::MenuItem::Standard(item) = item {
                assert!(
                    item.icon_name.is_empty(),
                    "{} names a theme icon",
                    item.label
                );
            }
        }
    }
}
