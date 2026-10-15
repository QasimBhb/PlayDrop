# Clipboard sync

Copy on one device, paste on another. Text goes over the same encrypted link as files. Nothing
is stored anywhere else.

How it works depends on what the operating system allows.

| Device | How the clipboard moves |
|---|---|
| Pop!_OS COSMIC | Automatic. Copying sends to your paired phones within a moment. |
| Windows | Automatic (written and tested by CI only, not yet run by a person). |
| Ubuntu GNOME | By button: the window has a send button. GNOME gives no way to watch the clipboard. |
| Android | Notification action. Tap **Send clipboard** in the PlayDrop notification. Android 10 and newer stop background apps from reading the clipboard, so this tap is how you send. |
| iPhone | By button, and a Shortcut called "Send Clipboard". iOS has no background clipboard access. |

PlayDrop hides a way that the system cannot support, and says why, instead of showing a switch
that does nothing.

## Per-device permissions

For each paired device you can allow receiving and sending separately. Both are on after
pairing. Change them in that device's settings on the device you are using. Nobody can change
them from the other side.

## The sensitive-clip rule

Password managers mark what they copy as secret. PlayDrop respects that mark.

- A clip marked secret is **not** sent by automatic sync. You see a short note that it was not
  sent because it is marked sensitive.
- If you send it by hand (the button or the notification action), PlayDrop asks first. If you
  go ahead, it travels marked sensitive.
- A clip that arrives marked sensitive is kept out of notifications. On Windows it is kept out
  of clipboard history and cloud sync. On Android it is hidden from previews. On the iPhone it
  stays local to the device and is removed from the pasteboard after a short time.
- If a password manager sets no mark, PlayDrop cannot know. Turn off sending for that device or
  turn the clipboard off while you work with passwords.

## Limits

- Text longer than 1 MiB is not synced.
- Images are sent with the clipboard where the system gives them to PlayDrop. An iPhone image
  goes to every device that accepts clipboards, not just one.
- A clip that came from another device is not sent back again.
