# Toolport on Arch and Omarchy

Toolport ships as `toolport` in its own signed pacman repository, so updates
arrive with `pacman -Syu` like the rest of your system. It runs on Arch, Manjaro,
EndeavourOS, and Omarchy. The package needs GTK 4.10+ and libadwaita 1.4, which
current Arch systems have.

Desktop integration (theme, tray, launch at login) is covered on the
[Omarchy page](https://toolport.app/omarchy).

## Install

On an Arch host the one-line installer adds the repository, trusts the signing
key, and installs the package:

```sh
curl -fsSL https://toolport.app/install.sh | bash
```

To do the same steps by hand, run all four commands:

```sh
curl -fsSL https://repo.toolport.app/toolport.gpg | sudo pacman-key --add -
sudo pacman-key --lsign-key A16BFA2E1014BD6BD718CC6E6621247E3FFA6AA7

printf '\n[toolport]\nServer = https://repo.toolport.app/$arch\n' | sudo tee -a /etc/pacman.conf
sudo pacman -Syu toolport
```

The first two import the repository's public key and mark that exact fingerprint
as trusted. Trusting the pinned fingerprint, rather than whatever
`repo.toolport.app` serves, is what stops a swapped key from being accepted: if
the key on the server is ever not this one, `--lsign-key` fails on purpose. To
inspect it yourself:

```sh
pacman-key --finger A16BFA2E1014BD6BD718CC6E6621247E3FFA6AA7
```

`pacman -Syu` then picks up the new repository and installs `toolport`, which
provides `toolport-gtk`, `toolport-gateway`, and a desktop entry. Launch it from
your application menu, or run `toolport-gtk`.

## Update

```sh
sudo pacman -Syu
```

There is no Toolport-specific update command, and the app never self-updates.
Whatever `pacman -Syu` installs is what you run; `pacman -Q toolport` prints the
installed version.

## Remove

```sh
sudo pacman -R toolport
```

Then delete the `[toolport]` block from `/etc/pacman.conf`, and drop the key if
you want:

```sh
sudo pacman-key --delete A16BFA2E1014BD6BD718CC6E6621247E3FFA6AA7
```

## Notes

- The package conflicts with the AUR `toolport-bin` and replaces
  `toolport-native-preview`, so only one Toolport runs at a time. Both Linux
  builds read the same `~/.config/Toolport` and only one process can hold the
  approval broker, so pacman swaps an existing `toolport-bin` install in place
  rather than installing beside it.
- The `.deb` and the AppImage are unchanged for Ubuntu 22.04, Debian 12, and any
  distribution without GTK 4.10 and libadwaita 1.4.
