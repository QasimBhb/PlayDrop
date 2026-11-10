# The command line

`playdrop` is a command line for the PlayDrop daemon on Linux and Windows. Use it from a terminal
or a script. The commands below all talk to the running daemon (`playdropd`), so the daemon must
be running. They show the same devices, pairings and transfers as the window.

## Where the command comes from

Every desktop download since 0.8.0 includes it.

- **.deb:** `playdrop` is on your `PATH` after the install.
- **Windows:** the installer puts `playdrop.exe` in `C:\Program Files\PlayDrop\bin` and, unless you
  clear the "Add the playdrop command to PATH" box, adds that folder to `PATH`. Open a new terminal
  after installing.
- **Flatpak:** Flatpak does not put commands on your `PATH`, and the app only sees the files you
  hand it. Add this to `~/.bashrc` or `~/.zshrc` once:

  ```sh
  playdrop() { flatpak run --file-forwarding --command=playdrop org.playdrop.PlayDrop "$@"; }
  ```

  Then every command below works as written, with one difference: put files and folders between
  `@@` marks, so Flatpak passes them into the app.

  ```sh
  playdrop peers
  playdrop send @@ ~/Pictures/trip @@ --to pixel
  playdrop send @@ report.pdf notes.txt @@
  playdrop outbox add @@ notes.txt @@ --to iphone
  ```

  Without the marks the command says the file does not exist and reminds you of them. Files in
  `~/Downloads/PlayDrop` need no marks. Each marked file stays shared with PlayDrop until you
  remove it (`flatpak documents org.playdrop.PlayDrop` lists them,
  `flatpak document-unexport --doc-id <ID>` removes one). Whether that sharing lasts through a
  restart of the computer has not been checked; if a stopped send cannot find its file after
  one, send it again.

```
playdrop send <PATH>... [--to <peer>] [--no-wait] [--json]
playdrop peers [--json]
playdrop status [--json]
playdrop transfers [--json]
playdrop cancel <ID>
playdrop clip [<TEXT> | --stdin] [--to <peer>] [--sensitive]
playdrop ping [--to <peer>] [MESSAGE]
playdrop outbox [--json]
playdrop outbox add <PATH>... --to <peer>
playdrop diagnose [--json]
```

## Choosing a device

`<peer>` is the start of a device's short ID, or its label (the name shown in the list). Run
`playdrop peers` to see both.

When exactly one paired device is online, you can leave out `--to` and PlayDrop uses that one.
With none or several online, a command that needs a device stops with exit code 2 and says why.
An unknown or ambiguous name also exits with 2.

## Commands

**`send`** sends files and folders.

```sh
playdrop send ~/Pictures/trip --to pixel
playdrop send report.pdf notes.txt
```

It waits for the transfer to finish and prints progress to standard error: bytes, rate and, for several files, how many are done (`1,204 of 5,000 files`). The line is cut to the width of your terminal. With `--no-wait` it
starts the transfer and returns at once. Check on it with `playdrop transfers`.

**`peers`** lists paired devices, whether each is online, its short ID and, for a device that
is online and reports one, its battery (`64%`, `64% charging` or `9% low`).

**`status`** shows the daemon's state, the same as the window's status.

**`transfers`** lists current and recent transfers with their progress. The NAMES column says
what is being sent: up to three top-level names (a file, or the folder it is in), so a folder
of 5,000 files reads as its folder's name. `and more` follows when there are more than
three. For a transfer of many
files that is moving, the FILES column shows how many are done, like `1204/5000`. A failed send
that PlayDrop keeps so it can resume shows `failed (resumes)` in the STATE column; `--json`
gives it as `"resumable": true`.

**`cancel`** cancels a transfer, or forgets a failed send that is waiting to resume. `ID` is the
first column of `playdrop transfers`; the start of it is enough when it is unique. A send that
could not reach the other device shows as `failed (resumes)`: it continues by itself when the
device is back or when you send the same files again, and `playdrop cancel <ID>` stops that and
tells the other device. A transfer that has already ended (complete, cancelled, or failed with
nothing to resume) is left as it is: the command says so, for example `already complete`, and
exits 0. An ID that matches no transfer exits 2.

**`clip`** sends text to a device's clipboard. `playdrop clip "hello"` sends that text.
`playdrop clip --stdin` reads the text from standard input, for example
`pwd | playdrop clip --stdin`. With neither, `playdrop clip --to phone` sends what is on this
computer's clipboard now: text, or an image when clipboard images are on in Settings. The same
grants apply as in the window: the device must accept clipboard text from you. When nothing is
sent it says why: the clip matches the last clip sent or received (exit code 0, nothing to do),
nothing could receive it, the device cannot take it, or the clipboard is empty (exit code 1).

A clip a password manager marked as secret is not sent from the clipboard unless you add
`--sensitive`: the command says so and exits 1. With `--sensitive` the clip always goes marked
sensitive, whether or not a password manager marked it, so the device keeps it out of its
clipboard history; with text or `--stdin`, `--sensitive` marks that text the same way. On a
desktop where PlayDrop cannot read the clipboard in the background (GNOME, or the Flatpak on
COSMIC), and with a daemon older than this command, `playdrop clip` without text says so and
exits 1: give the text or use `--stdin` there.

**`ping`** pings a device, which makes it show a notification. An optional message is shown with
it.

**`outbox`** lists files queued for a device to pick up, and **`outbox add`** queues files for a
device. This is for devices that cannot receive on their own, like an iPhone: it fetches the
queue when you open the app.

**`diagnose`** runs the same check as the Diagnostics page and prints it: whether the daemon is
listening, which network interfaces are usable, and whether the multicast, port or firewall layer
is the problem. See [cant-find-your-device.md](cant-find-your-device.md).

## JSON

`--json` prints the daemon's own answer as JSON on standard output, one result for the command,
so a script can read it. The shape is the one described in `docs/ipc.md`. Without `--json` the
output is plain text for people. Progress and messages always go to standard error.

```sh
playdrop peers --json
```

## Exit codes

| Code | Meaning |
|---|---|
| 0 | It worked |
| 1 | It failed (a transfer failed or was refused, the device went away, the daemon could not cancel) |
| 2 | Wrong usage, or an unknown or ambiguous device or transfer ID |
| 3 | The daemon is not running |

A script can tell "start the daemon" from "fix the command":

```sh
playdrop send big.iso --to pixel
case $? in
  0) echo sent ;;
  3) echo "start playdropd first" ;;
  *) echo "send failed" ;;
esac
```

## Two other subcommands

These are for developers, not for daily use.

- `playdrop host` runs a standalone host in your terminal, with its own data folder, without the
  daemon. It is how two hosts are tried against each other on one machine.
- `playdrop bench` benchmarks transfer speed between two hosts.
