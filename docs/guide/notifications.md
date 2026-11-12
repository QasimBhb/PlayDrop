# Phone notifications on the computer

PlayDrop can show your Android phone's notifications on your computer, from the apps you pick.
A message arrives on the phone, and a notification with the app name and the phone's name pops
up on the computer too.

- When the notification goes away on the phone (you opened it, swiped it, or the app removed
  it), the copy on the computer closes as well.
- When you close the copy on the computer (on Linux), the notification is dismissed on the
  phone too. On Windows the copy is a plain notification for now: closing it does not touch
  the phone.
- You cannot reply or press the notification's buttons from the computer, and icons and
  pictures are not shown.
- It works from Android only. An iPhone does not let other apps read its notifications.

## Off until you turn it on, three times

Nothing is mirrored until all three of these are on. All three start off, and all three are on
the phone, because the notifications are the phone's.

1. **Notification access.** Android asks you to give PlayDrop "Notification access" in the
   system settings. The PlayDrop app takes you there and says why the feature is off until
   you do.
2. **The device switch.** In the PlayDrop app, open the computer and turn on mirroring for it.
   Pairing never turns this on, and the computer cannot turn it on for you.
3. **The app list.** Tick the apps whose notifications may go to that computer. The list
   starts empty, and a newly installed app is not on it until you tick it.

## What is never sent

- Notifications from apps you did not tick, and from PlayDrop itself.
- Ongoing notifications (music players, downloads, navigation, apps running in the
  background) and group summaries.
- Notifications an app marked secret, and anything from a work profile or a secure folder.
- One-time codes that Android itself hides from other apps arrive hidden.
- When an app marks a notification private, only its public version goes, or just the app
  name and "New notification" when it has none.

On the computer, at most 20 notifications from one phone are shown at a time; a new one closes
the oldest. PlayDrop never writes the app names, titles, text or the notification keys into its
logs or the diagnostics report.

## Turning it off

- **On the computer:** open the PlayDrop window, choose the phone on the Devices page, and turn
  off **Show this device's notifications**. Every copy from that phone closes at once and new
  ones are not shown. Turning off notifications in PlayDrop's settings hides them too.
- **On the phone:** turn off the device switch, untick apps, or take back Notification access
  in Android's settings. The phone stops sending at once.
- Unpairing, or the phone going offline, closes every copy on the computer.
- **Windows exception:** Windows cannot yet close the copies PlayDrop shows. On Windows,
  mirrored notifications stay in the notification centre until you clear them yourself, even
  after the phone removes them, after unpairing, and after the permission is turned off. No
  new ones arrive once it is off.

From a terminal running `playdrop host`, the same switch is `grants <peer> -notificationsIn`.
