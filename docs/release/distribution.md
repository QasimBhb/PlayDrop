# Distribution

PlayDrop ships through **GitHub Releases only**: no Play Store, no App Store, no Microsoft
Store. Every release carries the same set of files, built by `.github/workflows/release.yml`
on GitHub's runners, plus a `SHA256SUMS` file to check them against.

## Making a release

1. Bump `version` under `[workspace.package]` in the root `Cargo.toml` (for example
   `0.1.0` to `0.2.0`). That one number is the version of everything: the Android
   `versionName` and `versionCode` (major * 10000 + minor * 100 + patch), the `.deb`, the
   Windows installer, the Flatpak and the iOS `MARKETING_VERSION` (CI passes it to
   `xcodebuild`; `apps/ios/project.yml` still holds `0.1.0` for local builds). Add a
   `<release>` entry for the new version at the top of `<releases>` in
   `packaging/linux/org.playdrop.PlayDrop.metainfo.xml`.
2. If `Cargo.lock` changed, run `packaging/flatpak/update-sources.sh` and commit the new
   `cargo-sources.json` with it.
3. Commit, then tag and push the tag:
   ```sh
   git tag v0.2.0
   git push origin main
   git push origin v0.2.0
   ```
4. CI builds every asset in parallel, then creates the release with all of them and
   `SHA256SUMS`. A tag that does not match the version in `Cargo.toml` stops the run before
   anything is built. A version with a hyphen (`0.2.0-rc1`) is published as a pre-release.

To test the pipeline without publishing, run the **Release** workflow by hand (Actions tab,
"Run workflow"): every job builds and uploads its file as a workflow artifact, and no
release is created.

## The assets

| File | For | Notes |
|---|---|---|
| `playdrop-<ver>-android.apk` | Android 8 (API 26) and newer | Signed with the PlayDrop release key; arm64 phones and x86_64 emulators in one APK |
| `playdrop-<ver>-linux-amd64.deb` | Pop!_OS 24.04 (COSMIC), Ubuntu 24.04 (GNOME) and newer | Native package, daemon starts at login |
| `playdrop-<ver>-linux-x86_64.flatpak` | Any distro with Flatpak | Sandboxed, single-file bundle |
| `playdrop-<ver>-windows-x64-setup.exe` | Windows 10 and 11, x64 | Inno Setup installer, not code signed |
| `playdrop-<ver>-ios-unsigned.ipa` | iPhone, iOS 17 and newer | Unsigned, for SideStore or AltStore to sign and install |
| `SHA256SUMS` | Everyone | `sha256sum -c SHA256SUMS --ignore-missing` in the download folder |

## Android

### Installing

Download `playdrop-<ver>-android.apk` on the phone and open it. Android asks once to allow
installs from that app (the browser or file manager). Updates install over the old version
as long as they are signed with the same key, which every release is.

### Automatic updates with Obtainium

[Obtainium](https://github.com/ImranR98/Obtainium) watches GitHub releases and installs new
APKs:

1. Install Obtainium (from its GitHub releases or F-Droid).
2. Tap **Add App** and paste `https://github.com/QasimBhb/PlayDrop`.
3. Keep the defaults. Each release has exactly one `.apk`, so no asset filter is needed.
   Turn on "Include prereleases" only if you want release candidates.

### The release key

All release APKs are signed with one key. Android only installs an update over an existing
app when the signing key matches, so **losing this key means every user has to uninstall
PlayDrop (losing its pairings) to move to a build signed with a new key**. Anyone who gets
the key can publish updates that install over PlayDrop on users' phones.

Where it lives on the development machine:

| Path | What |
|---|---|
| `~/.local/share/playdrop-release/playdrop-release.jks` | The keystore (alias `playdrop`) |
| `~/.local/share/playdrop-release/keystore.properties.backup` | Copy of the passwords file |
| `apps/android/keystore.properties` | Points Gradle at the keystore; gitignored, never committed |

`apps/android/.gitignore` ignores `keystore.properties`, `*.jks` and `*.keystore`.

**Back it up offline now**, for example both files from `~/.local/share/playdrop-release/`
on an encrypted USB stick kept somewhere else, plus a password manager entry with the
passwords. A copy only in GitHub secrets is not a backup: secrets cannot be read back out.

Local release build (uses `keystore.properties`):

```sh
cd apps/android && ./gradlew :app:assembleRelease
```

### Putting the key into GitHub secrets

The release workflow reads four repository secrets and fails the Android job if any is
missing, so it never ships a debug-signed APK. From the repo root, with `gh` logged in
(these commands pipe the values straight from the files, so nothing is printed):

```sh
P=apps/android/keystore.properties &&
test -s "$P" &&
base64 -w0 ~/.local/share/playdrop-release/playdrop-release.jks \
    | gh secret set PLAYDROP_KEYSTORE_BASE64 &&
sed -n 's/^storePassword=//p' "$P" | tr -d '\r\n' \
    | gh secret set PLAYDROP_KEYSTORE_PASSWORD &&
sed -n 's/^keyAlias=//p' "$P" | tr -d '\r\n' \
    | gh secret set PLAYDROP_KEY_ALIAS &&
sed -n 's/^keyPassword=//p' "$P" | tr -d '\r\n' \
    | gh secret set PLAYDROP_KEY_PASSWORD &&
gh secret list
```

The `test -s` guard matters: `gh secret set` accepts an empty stdin without complaint, so
running this from the wrong folder would store three empty secrets and the Android job
would only fail later with a signing error. Done on 2026-10-14 for `QasimBhb/PlayDrop`.

In CI the keystore is decoded to the runner's temporary folder, used by Gradle through the
`PLAYDROP_KEYSTORE_*` environment variables, and deleted at the end of the job. After the
build, the job checks the APK's signature and fails if it carries the Android debug
certificate.

## Linux

**.deb** (Pop!_OS, Ubuntu, Debian-based):

```sh
sudo apt install ./playdrop-<ver>-linux-amd64.deb
```

Use `apt`, not `dpkg -i`, so dependencies come along. Log out and in, or run `playdropd`
once, to start the background service. Details, autostart choices and the firewall:
`packaging/deb/README.md`.

**Flatpak**:

```sh
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install --user ./playdrop-<ver>-linux-x86_64.flatpak
flatpak run org.playdrop.PlayDrop
```

The bundle needs the `org.freedesktop.Platform//25.08` runtime from Flathub, hence the
remote. A bundle installed from a file does not update itself; install the next release's
bundle the same way. Permissions and sandbox notes: `packaging/flatpak/README.md`.

## Windows

Run `playdrop-<ver>-windows-x64-setup.exe`. It needs administrator rights (it adds a
firewall rule limited to `playdropd.exe`). The installer is **not code signed**, so
Microsoft Defender SmartScreen shows "Windows protected your PC" for a new download: click
**More info**, then **Run anyway**. The warning fades as a signed file gains reputation;
removing it for good needs a code signing certificate, which costs money every year and is
not set up. What the installer does: `packaging/windows/README.md`.

The Windows build is produced and tested on GitHub's Windows runner
(`.github/workflows/windows.yml`); no physical Windows machine has run it yet.

Two more workflows check the code on every push, and neither publishes anything:
`.github/workflows/linux.yml` formats, lints, builds and tests the Rust workspace on Ubuntu, and
`.github/workflows/android.yml` regenerates the UniFFI bindings, compiles the Kotlin and runs
the Android JUnit tests. Both prove the code builds and the unit tests pass; neither can prove
anything about a device or a desktop session.

## iPhone

### Why there is no free way to distribute

- The App Store and TestFlight both need the paid Apple Developer Program (99 USD a year).
- Signing with a free Apple ID works only for the owner's own devices: the provisioning
  profile expires after 7 days, at most 3 sideloaded apps can be active at once, and some
  capabilities (App Groups on some tools, the multicast entitlement always) are not
  available.
- So the release carries an **unsigned** `.ipa`. Each user signs it with their own Apple ID
  through a sideloading tool, and re-signs it every 7 days.

### For the project owner: xtool from Linux

The owner builds and installs directly from this Linux machine with xtool, signed with a
free personal team (never an employer's team): `apps/ios/scripts/xtool-dev.sh`, details in
`apps/ios/README.md` under "Building on Linux with xtool". Re-run it within 7 days to keep
the app working.

### For everyone else: SideStore or AltStore

1. Install [SideStore](https://sidestore.io) (refreshes apps on the phone itself) or
   [AltStore](https://altstore.io) (refreshes through AltServer on a computer on the same
   network) by following their own guides.
2. Download `playdrop-<ver>-ios-unsigned.ipa` on the phone, open it with SideStore or
   AltStore (or use **+** in its My Apps tab). It signs the app with your Apple ID and
   installs it.
3. Let the tool refresh PlayDrop before the 7 days run out, or it stops opening until you do.

Limits of a sideloaded build:

- **No multicast entitlement** (Apple grants it only to a paid team, per app). Devices do
  not appear by themselves; pair by typing the computer's address or scanning its QR code.
  Paired devices reconnect by their last-known address.
- **Share extension and App Group.** The unsigned build carries no entitlements, so whether
  the share sheet works depends on the tool registering an App Group for your Apple ID.
  Without one, the share extension installs but reports that PlayDrop's shared folder is
  missing; sending from inside the app, receiving, clipboard, the outbox pull and Find my
  phone still work.
- PlayDrop receives only while it is open on screen (iOS suspends it in the background),
  which is the same in every build.

## How KDE Connect and LocalSend compare

- **KDE Connect** publishes its Android app on F-Droid and Google Play, the iOS app on the
  App Store under KDE e.V.'s paid developer account, and the desktop app through Linux
  distributions, Flathub and the Microsoft Store, with Windows and macOS builds on KDE's own
  servers too. A non-profit with a developer account and a build infrastructure makes the
  stores affordable.
- **LocalSend** uses the stores (Google Play, App Store, Microsoft Store, F-Droid, Flathub
  and others) and also attaches APKs and desktop installers to its GitHub releases.
- **PlayDrop** starts with GitHub Releases alone: no store fees, no review queues, and one
  pipeline that builds everything from a tag. The cost is sideloading on Android (a one-time
  "allow installs" prompt, Obtainium for updates), a SmartScreen warning on Windows, and the
  7-day re-signing on iPhone.

### F-Droid later

F-Droid is the most natural next step for Android, because it is how many people who want
an open-source LAN app already install them. It would require:

- F-Droid builds the APK itself from a tagged source release, using a build recipe in the
  `fdroiddata` repository. That recipe must install the Rust toolchain, the NDK and
  `cargo-ndk`, and run the same Gradle task.
- No prebuilt binaries in the build: every dependency must be built from source or come from
  a trusted Maven repository, and F-Droid's scanner rejects bundled `.so`, `.jar` or `.aar`
  files it cannot account for. The JNA dependency (which ships a prebuilt native library)
  needs checking against their rules.
- Only free software dependencies and no proprietary services (PlayDrop already meets this).
- By default F-Droid signs with its own key, so an F-Droid install cannot update over a
  GitHub install and the other way round. To let users switch freely, the build must be
  reproducible: F-Droid then rebuilds the APK, confirms it matches ours byte for byte apart
  from the signature, and publishes the copy signed with the PlayDrop release key.
