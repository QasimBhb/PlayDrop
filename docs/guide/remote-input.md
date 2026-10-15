# Remote input and media control

A paired phone can act as the computer's trackpad, keyboard and presentation remote. It can
also show what the computer is playing and control it.

## Remote input is off until you turn it on

Controlling a computer is the most sensitive thing PlayDrop does. So:

- The permission is **off** for every new pairing.
- You turn it on **on the computer**, for one device, in the PlayDrop window (the Input switch for that device). The phone cannot ask for it or turn it on itself.
- If the computer cannot do remote input, the switch is disabled and says why.

On Pop!_OS COSMIC and GNOME, the first time a phone uses input, the system shows a consent dialog
for screen and input control. Someone at the computer must approve it. After that it is
remembered until you revoke it.

## Using it

On the phone, open the device and choose Trackpad, Keyboard or Presenter.

- **Trackpad.** Drag with one finger to move. Tap to left-click, tap with two fingers to
  right-click. Hold, then drag, to drag an item. Drag with two fingers to scroll.
- **Keyboard.** Type on the phone and the text goes to the computer.
- **Presenter.** Slide controls for a talk.

The trackpad on an Android phone over Wi-Fi was tried on COSMIC and showed no visible lag. No
number was measured. The iPhone screens and Windows have not been tried by a person.

## While it runs

- The computer shows "Remote input from <phone>" in its window and tray, with a **Stop** button.
- **Kill switch.** Stop in the tray or window ends every input session at once and releases any
  key or button that was held down.
- **Locked desktop.** If the computer locks, the session ends. A locked computer takes no remote
  input.
- **Idle.** A session with no input for 10 minutes ends.
- Keys are capped at 60 a second.
- The phone is told why the session ended.
- Turning the permission off in the window also stops it.

## Media control

With the media permission (on by default), the phone shows what the computer is playing and has
play, pause, next, previous and volume. On Linux this works with players that follow MPRIS. On
Windows it uses the system media controls and is untested by a person. A refused command names
the device and the permission to turn on.
