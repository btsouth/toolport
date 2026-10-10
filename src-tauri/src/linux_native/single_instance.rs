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
    build_stamp: Option<u64>,
}

impl Executable {
    fn read(path: impl AsRef<std::path::Path>) -> Result<Self, String> {
        let metadata = std::fs::metadata(path.as_ref()).map_err(|error| error.to_string())?;
        let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
        let build_stamp = read_build_stamp(&bytes);
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            build_stamp,
        })
    }
}

fn needs_handover(current: &Executable, running: &Executable, uid: u32) -> Result<bool, String> {
    // A private session bus identifies the session; also verify its owner's UID.
    if uid != unsafe { libc::geteuid() } {
        return Err("The running Toolport belongs to another user.".into());
    }
    Ok(current.device != running.device || current.inode != running.inode).map(|different| {
        different
            && current
                .build_stamp
                .zip(running.build_stamp)
                .is_some_and(|(new, old)| new > old)
    })
}

fn read_build_stamp(bytes: &[u8]) -> Option<u64> {
    let marker = b"TOOLPORT_BUILD_STAMP:";
    bytes
        .windows(marker.len())
        .enumerate()
        .filter(|(_, w)| *w == marker)
        .filter_map(|(at, _)| {
            let digits: Vec<_> = bytes[at + marker.len()..]
                .iter()
                .copied()
                .take_while(u8::is_ascii_digit)
                .collect();
            std::str::from_utf8(&digits).ok()?.parse().ok()
        })
        .max()
}
pub(super) fn information(args: &[String]) -> Option<String> {
    if args.iter().any(|a| a == "--version" || a == "-V") {
        return Some(format!(
            "Toolport {}\n{}",
            env!("CARGO_PKG_VERSION"),
            env!("TOOLPORT_BUILD_STAMP")
        ));
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Some("Usage: toolport-gtk [--hidden] [toolport://URL]\n  --hidden    Start in the tray\n  --version   Print the version and exit\n  --help      Print this help and exit".into());
    }
    None
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
    let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
        .map_err(|error| error.to_string())?;
    loop {
        remaining(deadline)?;
        // Inspect before GApplication registration: registration synchronizes
        // remote actions, which can block on an unresponsive old executable.
        let reply = bus_call(&connection, "NameHasOwner", app_id, deadline)?;
        if reply.get::<(bool,)>() == Some((false,)) {
            let app = register_application(app_id)?;
            if !app.is_remote() {
                return Ok(app);
            }
            // A concurrent launcher won the name. Inspect that primary next.
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
            let app = register_application(app_id)?;
            if !app.is_remote()
                || bus_call(&connection, "GetNameOwner", app_id, deadline)?.get::<(String,)>()
                    == Some((owner,))
            {
                return Ok(app);
            }
            continue;
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
        // Let GApplication arbitrate again. A concurrent launch of this same
        // build will simply activate it.
    }
}

fn register_application(app_id: &str) -> Result<adw::Application, String> {
    let app = adw::Application::builder()
        .application_id(app_id)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    app.register(gio::Cancellable::NONE)
        .map_err(|error| error.to_string())?;
    Ok(app)
}

pub(super) fn show_failure(error: &str) {
    eprintln!("toolport: upgrade handover failed: {error}");
    let app = adw::Application::builder()
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_activate(|app| {
        let dialog = failure_dialog(app);
        let app = app.clone();
        dialog.connect_response(None, move |_, _| app.quit());
        dialog.present();
    });
    app.run_with_args(&["toolport"]);
}

#[allow(deprecated)]
fn failure_dialog(app: &adw::Application) -> adw::MessageDialog {
    // A standalone modal MessageDialog advertises an xdg dialog to the
    // compositor without mapping a tiled application window behind it.
    let dialog = adw::MessageDialog::new(
        None::<&gtk::Window>,
        Some("Toolport is still running"),
        Some("The previous version did not close. Quit it from its tray menu, then open Toolport again."),
    );
    dialog.set_application(Some(app));
    dialog.set_title(Some("Toolport is still running"));
    dialog.set_modal(true);
    dialog.set_resizable(false);
    dialog.add_response("close", "Close");
    dialog.set_close_response("close");
    dialog.set_default_response(Some("close"));
    dialog
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    #[allow(deprecated)]
    fn failure_is_a_standalone_modal_alert() {
        adw::init().unwrap();
        let app = adw::Application::builder()
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gio::Cancellable::NONE).unwrap();
        let dialog = failure_dialog(&app);
        assert!(dialog.is_modal());
        assert!(!dialog.is_resizable());
        assert!(dialog.transient_for().is_none());
        assert_eq!(
            dialog.heading().as_deref(),
            Some("Toolport is still running")
        );
        assert_eq!(dialog.body(), "The previous version did not close. Quit it from its tray menu, then open Toolport again.");
        assert_eq!(dialog.response_label("close"), "Close");
        assert_eq!(dialog.close_response(), "close");
        assert_eq!(app.windows().len(), 1);
        let responded = std::rc::Rc::new(std::cell::Cell::new(false));
        let seen = responded.clone();
        dialog.connect_response(None, move |_, response| seen.set(response == "close"));
        dialog.response("close");
        assert!(responded.get());
    }

    #[test]
    fn same_executable_activates() {
        let image = Executable {
            device: 1,
            inode: 2,
            build_stamp: Some(1),
        };
        assert_eq!(
            needs_handover(&image, &image, unsafe { libc::geteuid() }),
            Ok(false)
        );
    }

    #[test]
    fn newer_build_hands_over_even_at_same_path_and_version() {
        let current = Executable {
            device: 1,
            inode: 3,
            build_stamp: Some(2),
        };
        let deleted = Executable {
            device: 1,
            inode: 2,
            build_stamp: Some(1),
        };
        assert_eq!(
            needs_handover(&current, &deleted, unsafe { libc::geteuid() }),
            Ok(true)
        );
        let other_device = Executable {
            device: 2,
            inode: 3,
            build_stamp: Some(2),
        };
        assert_eq!(
            needs_handover(&current, &other_device, unsafe { libc::geteuid() }),
            Ok(false)
        );
    }

    #[test]
    fn help_version_and_older_or_unknown_builds_never_handover() {
        for flag in ["--help", "-h", "--version", "-V"] {
            assert!(information(&["toolport-gtk".into(), flag.into()]).is_some());
        }
        assert!(information(&["toolport-gtk".into(), "--hidden".into()]).is_none());
        assert_eq!(
            read_build_stamp(b"text TOOLPORT_BUILD_STAMP:123 end"),
            Some(123)
        );
        let current = Executable {
            device: 1,
            inode: 3,
            build_stamp: Some(2),
        };
        for stamp in [None, Some(2), Some(3)] {
            let running = Executable {
                device: 1,
                inode: 2,
                build_stamp: stamp,
            };
            assert_eq!(
                needs_handover(&current, &running, unsafe { libc::geteuid() }),
                Ok(false)
            );
        }
    }
    #[test]
    fn another_user_is_never_retired() {
        let current = Executable {
            device: 1,
            inode: 3,
            build_stamp: Some(2),
        };
        let old = Executable {
            device: 1,
            inode: 2,
            build_stamp: Some(1),
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
        let temp = std::env::temp_dir().join(format!(
            "toolport-shell-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&temp).unwrap();
        let path = temp.join("shell");
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
        std::fs::remove_dir_all(temp).unwrap();
    }
}
