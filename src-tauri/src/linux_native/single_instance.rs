//! Retire a replaced GTK executable before starting any primary-only services.

use adw::prelude::*;
use gtk::{gio, glib};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::time::{Duration, Instant};

const HANDOVER_LIMIT: Duration = Duration::from_secs(5);

// A small named ELF section lets later launches inspect the build without
// loading or scanning the executable. Keep the section referenced at runtime.
const fn stamp_bytes() -> [u8; 128] {
    let text = env!("TOOLPORT_BUILD_STAMP").as_bytes();
    let mut bytes = [0; 128];
    let mut i = 0;
    while i < text.len() {
        bytes[i] = text[i];
        i += 1;
    }
    bytes
}
#[used]
#[link_section = ".toolport_build_stamp"]
static BUILD_STAMP_BYTES: [u8; 128] = stamp_bytes();
fn current_build_stamp() -> Option<u64> {
    read_build_stamp(std::hint::black_box(&BUILD_STAMP_BYTES))
}
fn elf_build_stamp(
    file: &mut (impl std::io::Read + std::io::Seek),
) -> std::io::Result<Option<u64>> {
    use std::io::SeekFrom;
    let mut header = [0; 64];
    file.read_exact(&mut header)?;
    if &header[..6] != b"\x7fELF\x02\x01" {
        return Ok(None);
    }
    let u16_at = |b: &[u8], i| u16::from_le_bytes(b[i..i + 2].try_into().unwrap());
    let u32_at = |b: &[u8], i| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    let u64_at = |b: &[u8], i| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
    let offset = u64_at(&header, 40);
    let size = u16_at(&header, 58) as u64;
    let count = u16_at(&header, 60) as u64;
    let names = u16_at(&header, 62) as u64;
    if size < 64 || count == 0 || names >= count {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(offset + names * size))?;
    let mut section = [0; 64];
    file.read_exact(&mut section)?;
    let name_offset = u64_at(&section, 24);
    let name_size = u64_at(&section, 32);
    if name_size > 65536 {
        return Ok(None);
    }
    let mut strings = vec![0; name_size as usize];
    file.seek(SeekFrom::Start(name_offset))?;
    file.read_exact(&mut strings)?;
    for i in 0..count {
        file.seek(SeekFrom::Start(offset + i * size))?;
        file.read_exact(&mut section)?;
        let at = u32_at(&section, 0) as usize;
        if strings.get(at..).and_then(|s| s.split(|b| *b == 0).next())
            != Some(b".toolport_build_stamp".as_slice())
        {
            continue;
        }
        let at = u64_at(&section, 24);
        let size = u64_at(&section, 32);
        if size > 128 {
            return Ok(None);
        }
        let mut bytes = vec![0; size as usize];
        file.seek(SeekFrom::Start(at))?;
        file.read_exact(&mut bytes)?;
        return Ok(read_build_stamp(&bytes));
    }
    Ok(None)
}
#[derive(Debug, PartialEq, Eq)]
struct Executable {
    device: u64,
    inode: u64,
    build_stamp: Option<u64>,
}

impl Executable {
    fn read(path: impl AsRef<std::path::Path>) -> Result<Self, String> {
        let metadata = std::fs::metadata(path.as_ref()).map_err(|error| error.to_string())?;
        let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
        let build_stamp = match elf_build_stamp(&mut file).ok().flatten() {
            Some(stamp) => Some(stamp),
            None => {
                // Compatibility with older builds that have no named section.
                std::io::Seek::rewind(&mut file).map_err(|e| e.to_string())?;
                stream_build_stamp(file).map_err(|e| e.to_string())?
            }
        };
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            build_stamp,
        })
    }
}

// Bounded memory, including markers that straddle a read boundary. The
// executable may have been deleted; the pinned /proc image remains readable.
fn stream_build_stamp(mut reader: impl std::io::Read) -> std::io::Result<Option<u64>> {
    let mut chunk = [0u8; 64 * 1024];
    let mut tail = Vec::new();
    let mut stamp = None;
    loop {
        let n = reader.read(&mut chunk)?;
        if n == 0 {
            return Ok(stamp.max(read_build_stamp(&tail)));
        }
        tail.extend_from_slice(&chunk[..n]);
        // Keep the final marker plus its maximum numeric width for the next
        // read, instead of accepting a number truncated at this boundary.
        let complete = tail.len().saturating_sub(64);
        stamp = stamp.max(read_build_stamp(&tail[..complete]));
        let start = complete.saturating_sub(64);
        tail.drain(..start);
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
            String::from_utf8_lossy(std::hint::black_box(&BUILD_STAMP_BYTES))
                .trim_end_matches('\0')
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
    let metadata = std::fs::metadata("/proc/self/exe").map_err(|e| e.to_string())?;
    let current = Executable {
        device: metadata.dev(),
        inode: metadata.ino(),
        build_stamp: current_build_stamp(),
    };
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
        let path = format!("/proc/{pid}/exe");
        let metadata = std::fs::metadata(&path).map_err(|e| e.to_string())?;
        let running = if current.device == metadata.dev() && current.inode == metadata.ino() {
            Executable {
                device: metadata.dev(),
                inode: metadata.ino(),
                build_stamp: current.build_stamp,
            }
        } else {
            Executable::read(&path)?
        };
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
    fn elf_stamp_reads_only_section_metadata_and_stamp() {
        let mut bytes = vec![0; 512];
        bytes[..6].copy_from_slice(b"\x7fELF\x02\x01");
        bytes[40..48].copy_from_slice(&128u64.to_le_bytes());
        bytes[58..60].copy_from_slice(&64u16.to_le_bytes());
        bytes[60..62].copy_from_slice(&2u16.to_le_bytes());
        bytes[62..64].copy_from_slice(&0u16.to_le_bytes());
        let names = b"\0.toolport_build_stamp\0";
        bytes[152..160].copy_from_slice(&300u64.to_le_bytes());
        bytes[160..168].copy_from_slice(&(names.len() as u64).to_le_bytes());
        bytes[300..300 + names.len()].copy_from_slice(names);
        bytes[192..196].copy_from_slice(&1u32.to_le_bytes());
        let stamp = b"TOOLPORT_BUILD_STAMP:12345";
        bytes[216..224].copy_from_slice(&400u64.to_le_bytes());
        bytes[224..232].copy_from_slice(&(stamp.len() as u64).to_le_bytes());
        bytes[400..400 + stamp.len()].copy_from_slice(stamp);
        assert_eq!(
            elf_build_stamp(&mut std::io::Cursor::new(bytes)).unwrap(),
            Some(12345)
        );
    }

    #[test]
    fn build_stamp_stream_handles_chunk_boundaries() {
        for offset in 65500..65540 {
            let mut bytes = vec![b'x'; offset];
            bytes.extend_from_slice(b"TOOLPORT_BUILD_STAMP:1791610372123456789 end");
            assert_eq!(
                stream_build_stamp(std::io::Cursor::new(bytes)).unwrap(),
                Some(1791610372123456789)
            );
        }
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
        std::fs::write(&path, b"TOOLPORT_BUILD_STAMP:1").unwrap();
        let old = std::fs::File::open(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"TOOLPORT_BUILD_STAMP:2").unwrap();
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
