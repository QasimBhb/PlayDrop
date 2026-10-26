# Troubleshooting

First open **Diagnostics** in the desktop window, or in the phone app's settings. On the computer
you can also run `playdrop diagnose` ([cli.md](cli.md)). Both name the layer that failed.

For devices that do not show up, read [cant-find-your-device.md](cant-find-your-device.md).

## The daemon is not running

The daemon (`playdropd`) is the part that listens for your phone. The window and the command line
only talk to it. If the window says it cannot reach the daemon, or `playdrop` exits with code 3:

- Start it by opening the PlayDrop window, which asks the system to start it, or run `playdropd`
  in a terminal.
- With the `.deb` the daemon starts at login. If it did not, log out and in once. A systemd user
  service is included but not enabled; see `packaging/deb/README.md`.
- With the Flatpak: `flatpak run --command=playdropd org.playdrop.PlayDrop`. Starting at login
  from a Flatpak goes through a system permission. If the system refuses, PlayDrop turns the
  setting off and says so in a notification and in Diagnostics.
- On Windows the installer sets the daemon to start when you sign in.

## Port 42187 is in use

PlayDrop listens on port 42187. If another program, or a second copy of the daemon, holds it, the
daemon stops and says "port 42187 is already in use by another program (or another PlayDrop
daemon)".

- Check for a second copy: `pgrep -a playdropd`. Stop one of them.
- Find the other program: `sudo ss -ltnp | grep 42187` (Linux).

That is different from a firewall blocking the port; see the third layer in
[cant-find-your-device.md](cant-find-your-device.md).

## KDE Connect and LocalSend can stay installed

PlayDrop never uses the ports of KDE Connect (1714 to 1764) or LocalSend (53317 and its multicast
address). Both can run beside PlayDrop. This is checked by tests, and the phone used for testing
had the official KDE Connect app installed during every transfer without trouble. Diagnostics
treats KDE Connect or GSConnect holding port 1716 as fine.

Running PlayDrop and KDE Connect on the same desktop at once has not been tried by a person.

## "Clipboard not sent"

The message names the reason. "No connected device has clipboard sharing turned on" means every
device that accepts your clipboard is offline right now, or you switched sending off for it. Open
**Devices** to see which are online and what each may receive. "It matches the last clip sent or
received" is not a problem: the other device already has that text. The three reasons are
explained in [clipboard.md](clipboard.md).

## A send failed with "resumes when ... is back"

The other device could not be reached for ten minutes (Wi-Fi dropped, the phone went to sleep
on a network that isolates it, the daemon there was down). Nothing is lost: the receiving side
keeps what it verified for 24 hours, and the sender keeps its side of the transfer.

- From a computer, do nothing: the send continues by itself when the device reconnects. If you
  would rather drop it, forget the failed entry: `playdrop transfers` shows it as
  `failed (resumes)` and `playdrop cancel <id>` forgets it (the window and the phone apps have a
  Forget button on that row).
- From a phone, share the same files to the same device again; the transfer continues from
  where it stopped.
- Sending the same files again from any device within 24 hours also continues it.

## Logs

PlayDrop's logs never contain keys, pairing secrets or the contents of what you send. File names
are logged only at debug level.

- Linux, when the daemon runs as a systemd user service (including D-Bus activation on a system
  with systemd): `journalctl --user -u playdropd`.
- To see more, run the daemon in a terminal with a log filter:

  ```sh
  playdropd --log playdropd=debug,playdrop_core=info
  ```

  (`RUST_LOG` works too.) For the Flatpak, start it with
  `flatpak run --command=playdropd org.playdrop.PlayDrop --log playdropd=debug,playdrop_core=info`.
- Android: connect the phone to a computer and run `adb logcat -s playdrop:*`.

Stop any running daemon before starting a second one in a terminal.

## The diagnostics report

There is no "export" button. To share a report, copy the text from the Diagnostics page, or run:

```sh
playdrop diagnose --json
```

It names the layers and their status. It does not contain file contents, clipboard text or keys.
Read it once before you post it in an issue, in case a device name or address is private.

## Reporting a problem

Open an issue at <https://github.com/QasimBhb/PlayDrop/issues>. Say which operating systems the
two devices run, how you installed PlayDrop, and what Diagnostics says. What has and has not been
tested is in `docs/release/test-matrix.md`.
