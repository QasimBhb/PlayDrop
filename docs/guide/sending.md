# Send files

Files and whole folders go straight between your devices over the local network. Nothing goes
through the internet or a server.

## From an Android phone

- **Share sheet.** In any app, tap Share and choose PlayDrop. The files go at once to your
  default device, with no picker. The picker appears only if you ask for it.
- **From the app.** Open PlayDrop, pick a device, choose files or a folder.
- **Notification.** The PlayDrop notification has a "Send to" action for the default device and a
  "Send clipboard" action (see [clipboard.md](clipboard.md)).

## From an iPhone

- **Share extension.** Tap Share and choose PlayDrop. This needs the app to have an App Group.
  A sideloaded copy usually has none, and the extension then says the shared folder is missing.
  If so, send from inside the app.
- **From the app.** Open PlayDrop, pick a device, choose files or photos.

## From a Linux or Windows computer

- **Window.** Drag files or folders onto a device in the PlayDrop window.
- **Command line.** `playdrop send ~/Pictures/trip --to pixel`. See [cli.md](cli.md).

## Resume

If the connection drops, for example when Wi-Fi blinks, the transfer carries on from where it
stopped and checks the data as it goes. If the sending app or daemon itself is restarted, start
the send again: resuming across a restart of the sender is not part of version 0.1 or 0.2.

Resume is tested between two computers. Killing a transfer to a phone over Wi-Fi and checking
the result has not been done yet.

## Auto-accept

When you pair, the other device gets permission to send you files without a question. A
notification then tells you what arrived, with Open and Show in folder.

- Auto-accept belongs to one paired device. It is never based on a name, an address or a network.
- Turn it off per device in that device's settings. Then each send waits for your answer.
  An unanswered offer expires after 10 minutes.

## Where files land

| Device | Folder |
|---|---|
| Linux and Windows | `~/Downloads/PlayDrop`, the PlayDrop folder inside your Downloads folder |
| Android | `Download/PlayDrop` in the phone's storage. On Android 8 and 9 files go to PlayDrop's own folder instead. |
| iPhone | PlayDrop's folder in the Files app (On My iPhone, PlayDrop) |

Folders arrive as folders. Empty folders are recreated on Android 11 and newer.

## Many small files to an Android phone

Android records every new file in Downloads one at a time, which makes thousands of small files
slow if they land there directly. Version 0.2.0 changes this: PlayDrop first puts many small
files in its own folder on the phone, then moves them into Downloads in the background. The
notification shows the progress. Version 0.1.0 does not do this and arrives at about 2 MiB/s
with 5,000 small files.
