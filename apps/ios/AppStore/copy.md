# App Store copy for PlayDrop (iOS)

Text to paste into App Store Connect, plus the privacy answers. Everything here must stay
true to what the iOS app actually does (Phase 4 gate "App Store copy is accurate", PLAN.md
anti-pattern 11). The limits that matter: the iPhone receives only while PlayDrop is open,
files queued on a computer arrive when you open the app, and nothing listens in the
background. Re-read this file against the build before every submission.

Field limits are noted in brackets; the counts below fit them.

## Name [30 characters]

PlayDrop: Local File Drop

## Subtitle [30 characters]

Send files to your computer

## Promotional text [170 characters]

Share photos, videos and files from your iPhone straight to your own computer or phone on
the same Wi-Fi. No account, no cloud, open source.

## Description [4,000 characters]

Editor note, not part of the text: add Windows to the platform list only once the Windows
build has actually shipped (it is not tested yet).

PlayDrop sends files between your iPhone and your own computers and phones over your Wi-Fi.
There is no account, no cloud and no internet in the middle: files go straight from one
device to the other, encrypted.

SEND IN ONE TAP
- Share from Photos, Files or any app and pick PlayDrop. A whole camera-roll selection or a
  multi-gigabyte video goes in one share.
- Send to your usual computer without choosing it every time.
- Send your clipboard text to your computer from the app or with a Shortcut.

RECEIVE WHILE OPEN
- While PlayDrop is open, your computer and phones can send files and clipboard text to
  your iPhone. Received files appear in the Files app under On My iPhone > PlayDrop.
- iOS does not let apps listen in the background, so PlayDrop does not pretend to. When the
  app is closed, your computer keeps what you send in a queue, and the files arrive the
  moment you open PlayDrop.

PAIR ONCE, SAFELY
- Pair by scanning a QR code on your computer or by typing its address, and check that both
  screens show the same code.
- Every connection is encrypted with TLS 1.3 and only paired devices are accepted.
- Interrupted transfers resume where they stopped instead of starting over.

FOR YOUR OWN DEVICES
- Works with PlayDrop on Linux and Android.
- Local network only: your files never leave your Wi-Fi.
- If devices cannot find each other, a diagnostics screen says why, for example when Local
  Network access is turned off for PlayDrop.

OPEN SOURCE
PlayDrop is free software under the GNU General Public License, version 3 or later. The
source code is public, and nothing in the app collects data about you.

What PlayDrop on iPhone does not do: it does not receive in the background, it does not
mirror notifications, and it does not work over the internet.

## Keywords [100 characters, comma separated, no spaces after commas]

file transfer,send files,wifi,local network,linux,desktop,android,clipboard,lan,share,open source,pc

Do not add other companies' product names (App Review rejects them in keywords).

## What's new in this version [4,000 characters]

Version 0.1.0 (build 1)

First release of PlayDrop for iPhone.
- Share photos, videos and files to your paired computer or phone in one tap.
- Receive files and clipboard text while the app is open.
- Files your computer queued for you arrive when you open the app.
- Pair with a QR code or an address; everything stays on your local network.

## Category and rating

- Primary category: Utilities. Secondary: Productivity.
- Age rating: 4+ (no objectionable content; the app shows only the user's own files).
- Price: free, no in-app purchases.

## URLs (fill in before submission)

- Support URL: the project's source repository page.
- Privacy policy URL: required by App Store Connect even for apps that collect nothing. A
  short page saying "PlayDrop collects no data; files and clipboard text travel only between
  your own paired devices on your local network" is enough. Not written yet.
- Marketing URL: optional.

## App Privacy ("nutrition label") answers

- **Do you or your third-party partners collect data from this app?** No.
  Result shown on the store: **Data Not Collected**.
- Reasoning, so the answer can be re-checked:
  - No account, no sign-in, no server run by us. The app talks only to the user's own
    paired devices on the local network.
  - No analytics, no advertising, no crash reporting SDK, no third-party SDKs at all. Apple's
    own opt-in crash reports are not counted as collection by the developer.
  - Files, clipboard text, device names and battery level are sent only to devices the user
    paired, directly over the local network, and never reach the developer. Apple's
    definition of "collect" is transmitting data off the device to the developer or partners
    in a way that allows access beyond what is needed to serve the request in real time;
    device-to-device transfers the user starts are not that.
  - Tracking: none. The privacy manifest (`PrivacyInfo.xcprivacy`) declares
    `NSPrivacyTracking` false and no tracking domains.

## App Review notes

PlayDrop sends files between the reviewer's own devices on one local network, so it needs a
second device to show a transfer. Suggested note:

> PlayDrop is an open-source local file transfer app (GPL-3.0-or-later). It needs a second
> device running PlayDrop on the same Wi-Fi (Linux and Android builds are on the project
> page). Pair by scanning the QR code from the other device's "Add device" screen.
> The app receives only while it is open; it uses no background modes. The Local Network
> permission is used to find and connect to the user's paired devices. The camera is used
> only to scan the pairing QR code. No account is needed and no data is collected.

## Export compliance

`ITSAppUsesNonExemptEncryption` is `false` (standard TLS only). See `apps/ios/README.md`
"Export compliance" for the reading behind it, which must be confirmed before submission.
