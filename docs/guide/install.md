# Install PlayDrop

Every file is on the [v0.1.0 release page](https://github.com/QasimBhb/PlayDrop/releases/tag/v0.1.0)
(newer versions are listed under [Releases](https://github.com/QasimBhb/PlayDrop/releases)).
PlayDrop is only on GitHub Releases. It is not in Google Play, the App Store or the Microsoft Store.

Each release has a `SHA256SUMS` file. To check your download, put it in the same folder as the
file and run `sha256sum -c SHA256SUMS --ignore-missing` (Linux).

Install PlayDrop on every device you want to link. The first pairing is in [pairing.md](pairing.md).

## Pop!_OS (COSMIC) and Ubuntu (GNOME)

Pick one.

**.deb** (`playdrop-<version>-linux-amd64.deb`):

```sh
sudo apt install ./playdrop-<version>-linux-amd64.deb
```

Use `apt`, not `dpkg -i`, so the packages PlayDrop needs come along. Then log out and in, or run
`playdropd` once, to start the background service. It starts at login after that.

**Flatpak** (`playdrop-<version>-linux-x86_64.flatpak`):

```sh
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install --user ./playdrop-<version>-linux-x86_64.flatpak
flatpak run org.playdrop.PlayDrop
```

The bundle needs a runtime from Flathub, which is why the first command adds Flathub. A bundle
installed from a file does not update itself. Install the next release's bundle the same way.

Pop!_OS 24.04 with COSMIC is the tested desktop. Ubuntu GNOME should work but has never been run.
On GNOME, copying does not sync by itself (see [clipboard.md](clipboard.md)).

## Windows 10 and 11

Run `playdrop-<version>-windows-x64-setup.exe`.

- It asks for administrator rights because it adds two firewall rules named `PlayDrop`. They are
  limited to `playdropd.exe` and open port 42187 for transfers (TCP) and discovery (UDP).
- The installer is not code signed. Windows SmartScreen shows "Windows protected your PC".
  Click **More info**, then **Run anyway**.
- The daemon starts when you sign in.

Status: GitHub's Windows runner builds and tests the Windows version, but nobody has run it on a
real Windows machine yet. Expect rough edges and please report them.

## Android

1. On the phone, download `playdrop-<version>-android.apk` from the release page and open it.
2. Android asks once to allow installs from that app (your browser or file manager). Allow it.
3. Open PlayDrop and follow the setup, including the notification permission and the
   battery setting. They keep the connection alive with the screen off.

The APK needs Android 8 or newer. It was tested on Android 16. Android 14 and 15 have not been
tested.

**Updates with Obtainium.** [Obtainium](https://github.com/ImranR98/Obtainium) installs new
releases for you: install it, tap **Add App**, paste `https://github.com/QasimBhb/PlayDrop`, and
keep the defaults. Updates also install over the old version by hand, because every release is
signed with the same key.

## iPhone

Apple only allows apps outside the App Store when you sign them yourself, so the release has an
unsigned file, `playdrop-<version>-ios-unsigned.ipa`, for a sideloading tool.

1. Install [SideStore](https://sidestore.io) or [AltStore](https://altstore.io) by following
   their own guides.
2. Download the `.ipa` on the phone and open it with that tool. It signs the app with your
   Apple ID and installs it.
3. Let the tool refresh PlayDrop at least every 7 days. After that the app stops opening until
   it is refreshed.

What this means in practice:

- The iPhone app receives only while it is open on screen. iOS does not allow more.
- A sideloaded copy cannot look for devices by itself. Pair by typing the computer's address or
  scanning its QR code. Paired devices reconnect on their own.
- The share sheet extension needs an App Group. Without one it installs but says PlayDrop's
  shared folder is missing. Sending from inside the app, receiving, the clipboard and Find my
  phone still work. This is not fixed in 0.2.0.

## Update and remove

- Linux: install the new `.deb` or `.flatpak` the same way. Remove with
  `sudo apt remove playdrop` or `flatpak uninstall --user org.playdrop.PlayDrop`.
- Windows: run the new installer over the old one. Remove it from Settings, Apps. Your pairings
  stay in `%LOCALAPPDATA%\PlayDrop`. Delete that folder to forget every paired device.
- Android and iPhone: install the new file over the old one. Uninstalling the app removes its
  pairings.
