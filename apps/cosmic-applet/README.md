# PlayDrop panel applet for COSMIC

A native COSMIC panel entry for PlayDrop. Click the drop icon in the panel to see:

- whether the PlayDrop daemon (`playdropd`) is running, with a "Start PlayDrop" button when it
  is not;
- your paired devices, online or offline, with battery when the device reports it;
- for each online device, "Send clipboard" (one click), "Send files..." and "Send folder..."
  (open the file chooser);
- transfers in progress and the last few finished ones, updating live;
- "Remote input from ..." with a Stop button while a device controls this desktop;
- "Open PlayDrop" to open the full window.

The panel icon is hollow and struck through while the daemon is not running.

The applet is optional. The daemon's tray icon keeps working without it. It is a thin client:
it talks to `playdropd` over D-Bus only (`docs/ipc.md`) and never opens a network socket.

## Build

It is its own Cargo workspace with its own `Cargo.lock`, because libcosmic comes from git and
must stay out of the main lock file. libcosmic is pinned to the revision cosmic-applets 1.9.0
uses.

```sh
cd apps/cosmic-applet
cargo build --release --locked
cargo test --locked
```

On a drive mounted through ntfs-3g, set `CARGO_TARGET_DIR` to a folder on a Linux file system
(for example `~/.cache/playdrop-applet-target`); linking on NTFS stalls.

Build needs `libxkbcommon-dev` and `pkg-config`. At run time the binary links only `libc`,
`libgcc_s` and `libxkbcommon` (Wayland is spoken in pure Rust); the file chooser needs
`xdg-desktop-portal`. Both panel icons are built into the binary.

## Try it from `~/.local`

```sh
install -Dm755 target/release/playdrop-cosmic-applet ~/.local/bin/playdrop-cosmic-applet
install -Dm644 data/org.playdrop.PlayDrop.CosmicApplet.desktop \
  ~/.local/share/applications/org.playdrop.PlayDrop.CosmicApplet.desktop
install -Dm644 data/icons/hicolor/symbolic/apps/org.playdrop.PlayDrop-symbolic.svg \
  ~/.local/share/icons/hicolor/symbolic/apps/org.playdrop.PlayDrop-symbolic.svg
```

`~/.local/bin` must be on the `PATH` the panel sees (log out and in after adding it). The
applet finds `playdrop-desktop` and `playdropd` next to itself or on `PATH`.

Then open COSMIC Settings, go to Desktop, Panel (or Dock), Applets, choose "Add applet", and
pick PlayDrop.

To run it outside the panel for a quick look, start `playdrop-cosmic-applet` from a terminal
in the COSMIC session. With `PLAYDROP_APPLET_OPEN_POPUP=1` it opens its popup as soon as it
has reached (or failed to reach) the daemon, which helps when no pointer is at hand.

## Remove it

Remove PlayDrop from COSMIC Settings, Desktop, Panel, Applets, then delete the files:

```sh
rm ~/.local/bin/playdrop-cosmic-applet \
   ~/.local/share/applications/org.playdrop.PlayDrop.CosmicApplet.desktop
```

Keep the icon if the PlayDrop package installed it too.

## Notes

- The applet never starts the daemon by itself. When the daemon is not running it waits for
  it to appear on the session bus (started at login, from the window, the command line or
  `systemctl --user`) and connects at once. Only "Start PlayDrop" starts it: through D-Bus
  activation when the service file is installed, otherwise by running `playdropd`.
- "Send clipboard" asks the daemon to read the clipboard itself and send it, so text and
  images both work. A clip marked by a password manager is held back and the popup asks first
  ("Send" or "Cancel"); on "Send" the daemon sends it marked sensitive, so the phone keeps it
  out of its history. With an older daemon that does not know the request, or one that cannot
  read the clipboard on this session, the applet reads the text itself through the Wayland
  data-control protocol (text only, at most 1 MiB) with the same ask-first rule.
- Devices show their battery as soon as the list loads (the daemon reports it with each
  device). Transfer rows show what is being moved ("notes.txt", "Photos (50 files)", "a.pdf
  and Photos"); names are cut to fit and come from the other device, so they are only shown.
- "Send files..." and "Send folder..." use the desktop's file chooser. Only files on this
  computer can be sent; anything else chosen is left out and the popup says so.
- Every request to the daemon gives up after 15 seconds and says so, so a stuck daemon never
  leaves the popup waiting.
- Logs go to standard error. `RUST_LOG=playdrop_cosmic_applet=debug` shows more. File names,
  clipboard text and full Device IDs are never logged at info level.
