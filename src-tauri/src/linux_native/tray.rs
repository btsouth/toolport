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

    fn icon_name(&self) -> String {
        "toolport".into()
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
                icon_name: "window-new".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.commands.send(Command::Show);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: format!("Pending approvals ({})", self.pending),
                icon_name: "security-high".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.commands.send(Command::ShowApprovals);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit Toolport".into(),
                icon_name: "application-exit".into(),
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
