use std::path::Path;
use std::process::Command;

pub(super) const RELEASE_PAGE: &str = "https://github.com/btsouth/toolport/releases";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PackageType {
    Deb,
    Rpm,
    Pacman,
}

fn advice(package: Option<PackageType>) -> &'static str {
    match package {
        Some(PackageType::Deb) => {
            "Download the new .deb from the release page, then run sudo apt install ./<file>.deb."
        }
        Some(PackageType::Rpm) => {
            "Download the new .rpm from the release page, then run sudo dnf install ./<file>.rpm."
        }
        Some(PackageType::Pacman) => "Update Toolport with sudo pacman -Syu.",
        None => {
            "Open the release page for the latest Toolport package and installation instructions."
        }
    }
}

// Query ownership of this executable, not the distribution or available CLIs:
// a Debian system can have rpm installed and a development build is not packaged.
fn installed_type(
    executable: &Path,
    mut query: impl FnMut(PackageType, &Path) -> Option<String>,
) -> Option<PackageType> {
    for package in [PackageType::Deb, PackageType::Rpm, PackageType::Pacman] {
        let Some(output) = query(package, executable) else {
            continue;
        };
        let owned = match package {
            PackageType::Deb => output.lines().any(|line| {
                let Some((owner, path)) = line.split_once(": ") else {
                    return false;
                };
                owner.split(':').next() == Some("toolport") && Path::new(path) == executable
            }),
            PackageType::Rpm | PackageType::Pacman => output.trim() == "toolport",
        };
        if owned {
            return Some(package);
        }
    }
    None
}

pub(super) fn update_advice() -> &'static str {
    let package = std::env::current_exe().ok().and_then(|executable| {
        installed_type(&executable, |package, executable| {
            let (command, args): (&str, &[&str]) = match package {
                PackageType::Deb => ("dpkg-query", &["--search"]),
                PackageType::Rpm => ("rpm", &["-qf", "--qf", "%{NAME}"]),
                PackageType::Pacman => ("pacman", &["-Qqo"]),
            };
            let output = Command::new(command)
                .args(args)
                .arg(executable)
                .output()
                .ok()?;
            if !output.status.success() {
                return None;
            }
            String::from_utf8(output.stdout).ok()
        })
    });
    advice(package)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advice_uses_downloaded_files_for_deb_and_rpm_and_the_pacman_repo() {
        assert!(advice(Some(PackageType::Deb)).contains("sudo apt install ./<file>.deb"));
        assert!(advice(Some(PackageType::Rpm)).contains("sudo dnf install ./<file>.rpm"));
        assert!(advice(Some(PackageType::Pacman)).contains("sudo pacman -Syu"));
        for package in [PackageType::Deb, PackageType::Rpm] {
            assert!(advice(Some(package)).contains("Download the new"));
            assert!(advice(Some(package)).contains("release page"));
        }
        assert!(advice(None).contains("release page"));
    }

    #[test]
    fn detection_requires_toolport_ownership_of_the_running_executable() {
        let executable = Path::new("/usr/bin/toolport-gtk");
        for expected in [PackageType::Deb, PackageType::Rpm, PackageType::Pacman] {
            let detected = installed_type(executable, |package, path| {
                assert_eq!(path, executable);
                if package != expected {
                    return None;
                }
                Some(match package {
                    PackageType::Deb => "toolport:amd64: /usr/bin/toolport-gtk\n".into(),
                    _ => "toolport\n".into(),
                })
            });
            assert_eq!(detected, Some(expected));
        }
        assert_eq!(installed_type(executable, |_, _| None), None);
        assert_eq!(
            installed_type(executable, |package, _| Some(match package {
                PackageType::Deb => "toolport: /usr/bin/another-binary\n".into(),
                _ => "another-package\n".into(),
            })),
            None
        );
        assert_eq!(
            installed_type(executable, |_, _| Some(
                "another-package: /usr/bin/toolport-gtk\n".into()
            )),
            None
        );
    }
}
