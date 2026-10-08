//! Retire a replaced GTK executable before starting any primary-only services.

use adw::prelude::*;
use gtk::{gio, glib};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::time::{Duration, Instant};

const HANDOVER_LIMIT: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq, Eq)]
struct Executable {
    device: u64,
    inode: u64,
}

impl Executable {
    fn read(path: impl AsRef<std::path::Path>) -> Result<Self, String> {
        let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

fn needs_handover(current: &Executable, running: &Executable, uid: u32) -> Result<bool, String> {
    // A private session bus identifies the session; also verify its owner's UID.
    if uid != unsafe { libc::geteuid() } {
        return Err("The running Toolport belongs to another user.".into());
    }
    Ok(current != running)
}

fn remaining(deadline: Instant) -> Result<i32, String> {
    let remaining = deadline
        .saturating_duration_since(Instant::now())
        .as_millis();
    if remaining == 0 {
        Err("The running Toolport did not close within five seconds.".into())
    } else {
        Ok(remaining.min(i32::MAX as u128) as i32)
    }
}

fn bus_call(
    connection: &gio::DBusConnection,
    method: &str,
    name: &str,
    deadline: Instant,
) -> Result<glib::Variant, String> {
    connection
        .call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            method,
            Some(&(name,).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            remaining(deadline)?.min(1000),
            gio::Cancellable::NONE,
        )
        .map_err(|error| error.to_string())
}

pub(super) fn register(app_id: &str) -> Result<adw::Application, String> {
    let deadline = Instant::now() + HANDOVER_LIMIT;
    let current = Executable::read("/proc/self/exe")?;
    loop {
        remaining(deadline)?;
        let app = adw::Application::builder()
            .application_id(app_id)
            .flags(gio::ApplicationFlags::HANDLES_OPEN)
            .build();
        app.register(gio::Cancellable::NONE)
            .map_err(|error| error.to_string())?;
        if !app.is_remote() {
            return Ok(app);
        }
        let connection = app.dbus_connection().ok_or("No session bus connection.")?;
        // The old primary may have exited between registration and inspection.
        let reply = bus_call(&connection, "NameHasOwner", app_id, deadline)?;
        if reply.get::<(bool,)>() == Some((false,)) {
            continue;
        }
        let owner = bus_call(&connection, "GetNameOwner", app_id, deadline)?
            .get::<(String,)>()
            .ok_or("Invalid application owner reply.")?
            .0;
        let pid = bus_call(&connection, "GetConnectionUnixProcessID", &owner, deadline)?
            .get::<(u32,)>()
            .ok_or("Invalid application PID reply.")?
            .0;
        let uid = bus_call(&connection, "GetConnectionUnixUser", &owner, deadline)?
            .get::<(u32,)>()
            .ok_or("Invalid application user reply.")?
            .0;
        // Pin this process before inspecting its image, so PID reuse cannot make
        // us wait for an unrelated process. No process ever receives a signal.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if fd < 0 {
            return Err(format!(
                "Could not track the running Toolport: {}",
                std::io::Error::last_os_error()
            ));
        }
        let process = unsafe { OwnedFd::from_raw_fd(fd as i32) };
        let running = Executable::read(format!("/proc/{pid}/exe"))?;
        if !needs_handover(&current, &running, uid)? {
            return Ok(app);
        }
        eprintln!("toolport: asking replaced shell {pid} to quit before upgrade startup");
        // Preview.1 already exports this action. Address its unique bus name,
        // never a replacement that wins registration while this launch waits.
        connection
            .call_sync(
                Some(&owner),
                &format!("/{}", app_id.replace('.', "/")),
                "org.gtk.Actions",
                "Activate",
                Some(
                    &(
                        "quit",
                        Vec::<glib::Variant>::new(),
                        std::collections::HashMap::<String, glib::Variant>::new(),
                    )
                        .to_variant(),
                ),
                None,
                gio::DBusCallFlags::NONE,
                remaining(deadline)?.min(1000),
                gio::Cancellable::NONE,
            )
            .map_err(|error| format!("Could not ask the running Toolport to close: {error}"))?;
        // GApplication releases its name before post-run tray/bridge cleanup.
        // Wait for process exit as well, so no old tray item survives takeover.
        loop {
            let mut poll = libc::pollfd {
                fd: process.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let result = unsafe { libc::poll(&mut poll, 1, remaining(deadline)?) };
            if result > 0 && poll.revents & libc::POLLIN != 0 {
                break;
            }
            if result < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            }
            return Err("The running Toolport did not close within five seconds.".into());
        }
        let owner_alive = bus_call(&connection, "NameHasOwner", &owner, deadline)?;
        if owner_alive.get::<(bool,)>() != Some((false,)) {
            return Err("The old Toolport still owns its session bus connection.".into());
        }
        // Drop the registered remote application and let GApplication arbitrate
        // again. A concurrent launch of this same build will simply activate it.
    }
}

pub(super) fn show_failure(error: &str) {
    eprintln!("toolport: upgrade handover failed: {error}");
    let app = adw::Application::builder()
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let error = error.to_owned();
    app.connect_activate(move |app| {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Toolport could not start")
            .default_width(460)
            .default_height(180)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
        content.set_margin_top(24);
        content.set_margin_bottom(24);
        content.set_margin_start(24);
        content.set_margin_end(24);
        content.append(&gtk::Label::builder()
            .label(format!("{error}\n\nQuit the running Toolport from its tray menu, then launch Toolport again."))
            .wrap(true)
            .build());
        let close = gtk::Button::with_label("Close");
        let app = app.clone();
        close.connect_clicked(move |_| app.quit());
        content.append(&close);
        window.set_content(Some(&content));
        window.present();
    });
    app.run_with_args(&["toolport"]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_executable_activates() {
        let image = Executable {
            device: 1,
            inode: 2,
        };
        assert_eq!(
            needs_handover(&image, &image, unsafe { libc::geteuid() }),
            Ok(false)
        );
    }

    #[test]
    fn replaced_executable_hands_over_even_at_same_path_and_version() {
        let current = Executable {
            device: 1,
            inode: 3,
        };
        let deleted = Executable {
            device: 1,
            inode: 2,
        };
        assert_eq!(
            needs_handover(&current, &deleted, unsafe { libc::geteuid() }),
            Ok(true)
        );
        let other_device = Executable {
            device: 2,
            inode: 3,
        };
        assert_eq!(
            needs_handover(&current, &other_device, unsafe { libc::geteuid() }),
            Ok(true)
        );
    }

    #[test]
    fn another_user_is_never_retired() {
        let current = Executable {
            device: 1,
            inode: 3,
        };
        let old = Executable {
            device: 1,
            inode: 2,
        };
        assert!(
            needs_handover(&current, &old, unsafe { libc::geteuid() }.wrapping_add(1)).is_err()
        );
    }

    #[test]
    fn handover_deadline_fails_closed() {
        assert!(remaining(Instant::now()).is_err());
        assert!(remaining(Instant::now() + HANDOVER_LIMIT).is_ok());
    }

    #[test]
    fn deleted_image_remains_identifiable() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("shell");
        std::fs::write(&path, b"old").unwrap();
        let old = std::fs::File::open(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"new").unwrap();
        let deleted = Executable::read(format!("/proc/self/fd/{}", old.as_raw_fd())).unwrap();
        assert!(
            needs_handover(&Executable::read(&path).unwrap(), &deleted, unsafe {
                libc::geteuid()
            })
            .unwrap()
        );
    }
}
