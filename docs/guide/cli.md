# The command line

`playdrop` is a command line for the PlayDrop daemon on Linux and Windows. Use it from a terminal
or a script. The commands below all talk to the running daemon (`playdropd`), so the daemon must
be running. They show the same devices, pairings and transfers as the window.

```
playdrop send <PATH>... [--to <peer>] [--no-wait] [--json]
playdrop peers [--json]
playdrop status [--json]
playdrop transfers [--json]
playdrop clip (<TEXT> | --stdin) [--to <peer>]
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

It waits for the transfer to finish and prints progress to standard error. With `--no-wait` it
starts the transfer and returns at once. Check on it with `playdrop transfers`.

**`peers`** lists paired devices, whether each is online, and its short ID.

**`status`** shows the daemon's state, the same as the window's status.

**`transfers`** lists current and recent transfers with their progress.

**`clip`** sends text to a device's clipboard. `playdrop clip "hello"` sends that text.
`playdrop clip --stdin` reads the text from standard input, for example
`pwd | playdrop clip --stdin`. The same grants apply as in the window: the device must accept
clipboard text from you.

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
| 1 | It failed (a transfer failed or was refused, the device went away) |
| 2 | Wrong usage, or an unknown or ambiguous device |
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
