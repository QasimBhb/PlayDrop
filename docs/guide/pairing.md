# Pair two devices

Pairing introduces two devices once. After that they reconnect by themselves, also after the
network or the address changes. There is no account.

PlayDrop trusts a device by its key, not by its name or its address. Two devices with the same
name are still two different devices.

## With the six digits

1. Open **Add device** in PlayDrop on both devices. On the desktop it is in the window (and in
   the tray menu). On the phone it is on the devices screen.
2. Wait a few seconds. Each device should show the other as a candidate. If nothing shows up,
   go to [cant-find-your-device.md](cant-find-your-device.md).
3. Pick the other device. Both screens show the same six digits.
4. Compare them. If they match, accept on both. If they differ, say no: someone else may be
   on the network.
5. The device now appears in the list on both sides.

## With a QR code

Use this when the devices cannot see each other on the network, or you would rather not compare.

1. Open **Add device** on one device and show its QR code.
2. Scan it with the other device. The code carries the first device's key, its addresses and a
   one-time secret, so you do not compare digits.
3. Accept on both if asked.

The QR code works once and expires. Treat it like a password while it is on screen.

## Or type an address

Open **Add device** on the computer and note its address. On the phone, type it into **Add
device**, then follow the same steps. The iPhone build from the releases needs this or the QR
code, because it cannot look for devices by itself.

## What a new pairing allows

By default a paired device may:

- send you files, which land without a question (auto-accept, see [sending.md](sending.md));
- send and receive clipboard text;
- ping you and find your phone;
- control media (play, pause, volume) on the computer.

It may not control your computer's mouse and keyboard. That is off until you turn it on, on
the computer (see [remote-input.md](remote-input.md)). These permissions are per device and can
only be changed on the device they apply to, never from the other side.

## Forget a device

Open the device in the list and unpair it. The device must pair again to come back.
