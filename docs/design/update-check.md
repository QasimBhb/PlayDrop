# Update check: design note (2026-11-12, Phase 16)

Reader: whoever builds or reviews the update check. This note is written before the code, because
it is the first time PlayDrop contacts a host outside the local network. The product has said "no
account and no cloud" since day one (`docs/product.md`), so every choice below is made to keep that
true in spirit: one small anonymous question, asked only when a person opens the app, that can be
switched off.

## 1. What the user gets

On opening PlayDrop, at most once a day, the app asks whether a newer release exists. If one does
it offers **Update** or **Later**.

| Install | What "Update" does |
|---|---|
| `.deb` (Pop!_OS, Ubuntu) | Downloads the new `.deb` (and the applet's, when the applet is installed), verifies it, installs it through `pkexec` (the system password prompt) and restarts the daemon |
| Windows installer | Downloads the new installer, verifies it, runs it (Windows asks for permission) and waits for it; the installer starts PlayDrop again |
| Android | Downloads the APK, verifies it, hands it to the system installer. Android itself refuses an APK signed with another key |
| Flatpak | Notice only, with the release page link. A Flatpak cannot replace itself |
| iPhone | Notice only, with the release page link. iOS installs go through SideStore, AltStore or xtool |
| Anything else (a `cargo build`, another distro) | Notice only |

"Later" hides the offer until the next day's check. Nothing is ever downloaded or installed without
the person pressing Update. While the download runs, the window's banner has **Cancel** (and
`playdrop update --install` cancels on Ctrl-C); once the files are with the operating system's
installer it can no longer be stopped from PlayDrop.

Only a release build checks. A build whose version is not a plain `major.minor.patch` (a
pre-release such as `0.11.0-rc.1`, or a test build) makes no request at all, offers nothing, and
says in Settings: "This is not a release build, so it does not check for updates." Reading such a
version as 0.0.0 would offer it every release, older ones included.

## 2. When a check runs

- Only when a UI opens: the desktop window, the COSMIC applet's popup, the Android app, the iPhone
  app. Never from a timer, never at login, never from the daemon or the Android service on their
  own. A computer whose window is never opened never asks.
- At most once per 24 hours per install, counted from the last check that got an answer. A failed
  check (no internet, GitHub down) may be tried again after one hour.
- Only when the Settings switch "Check for updates" is on. **Default: on** (the user's decision,
  2026-11-12). With it off, no request of any kind leaves the local network.
- Settings also has "Check now", which skips both waits (the 24 hours and the hour after a
  failure) but never the switch: a person pressing it wants an answer now.
- **A failed check is shown in Settings only**, beside "Last checked", never as a banner, so a
  computer that is offline is not nagged. An offer that verified earlier stays on screen through
  a later failed check.
- **The last signed answer is remembered** in `update.json` in the daemon's data dir (mode 0600),
  next to the times the rules above need and the version "Later" was said to. An offer then
  survives a restart of the daemon without asking GitHub again. The stored `SHA256SUMS` and its
  signature are verified against the pinned keys every time they are read, so the file is never
  trusted for content; it holds GitHub's file, two timestamps and a version, nothing about the
  device.

## 3. What is asked, and of whom

Two HTTPS GET requests to GitHub, the place the releases already live:

```
GET https://github.com/QasimBhb/PlayDrop/releases/latest/download/SHA256SUMS
GET https://github.com/QasimBhb/PlayDrop/releases/latest/download/SHA256SUMS.sig
```

GitHub answers each with a redirect to its file store. Nothing else is fetched for a check: no
JSON API, no release notes, no telemetry endpoint, no third party.

**What the request carries.** The URL above, which is the same for everyone, and
`User-Agent: PlayDrop`. Every header is a fixed value, the same for every install; none carries
a version, an operating system, a device name, a Device ID, an install ID, a cookie, a referrer
or a cache validator such as `If-None-Match` (a stored ETag would be an identifier). The exact
headers each platform's HTTP client sends:

| Platform | Headers |
|---|---|
| Desktop (`ureq`) | `User-Agent: PlayDrop`, `Host` |
| Android (`HttpURLConnection`) | the desktop's, plus `Accept-Encoding: identity` and `Connection: Keep-Alive` |
| iPhone (`URLSession`) | the desktop's, plus `Accept: */*`, `Accept-Language: en`, `Accept-Encoding: identity` and `Connection` |

GitHub sees what any web server sees: the IP address and the time. The Settings line says so:
"No name, ID or version is sent. GitHub sees your IP address, as with any download."

**Proxies.** The desktop daemon honours `HTTPS_PROXY` (or `https_proxy`) and `ALL_PROXY`, with
`NO_PROXY`, as `curl` does for an `https` URL; `HTTP_PROXY` alone is not used. Android and the
iPhone use the system proxy. A proxy sees what GitHub sees (with HTTPS through a proxy, the host
name and the time), nothing more.

**Why not the Releases API.** It answers with unsigned JSON, so the version in it could not be
trusted, and it is rate limited per address (60 an hour), which fails on a shared campus address.
The checksum file is tiny, is not rate limited that way, and is signed (section 4), so the version
number itself is authenticated.

**Redirects.** Followed at most 5 times, HTTPS only, and only to `github.com` or a host ending in
`.githubusercontent.com`. Anything else ends the check as failed.

**Limits.** `SHA256SUMS` at most 64 KiB, the signature at most 1 KiB, 15 s per request, redirects
included. A download is refused when larger than 512 MiB or when its length differs from what was
promised.

**Download time rule.** A download is large, so the 15 s is not its whole limit: connecting, the
answer's head and each read are each limited to 15 s (a stall ends it), the whole download (every
file of the update together) must end within 30 minutes, and at least 64 KiB must arrive in every
60 s (a server trickling a byte at a time would otherwise pass the per-read limit for hours). Any
of these ends the update as failed with "The download was too slow and was stopped." and removes
what was downloaded.

## 4. Signed checksums

The release run already writes `SHA256SUMS` (one `sha256sum` line per release file). From 0.11.0 it
also writes `SHA256SUMS.sig`: an Ed25519 signature over the exact bytes of `SHA256SUMS`, base64 on
one line.

- **Key.** A release key made for this purpose alone on 2026-11-12. The private half lives outside
  the repo (`~/.local/share/playdrop-release/update-signing-ed25519.pem`, mode 600) and as the
  Actions secret `PLAYDROP_UPDATE_SIGNING_KEY`. The public half is pinned in the code
  (`playdrop_core::update::TRUSTED_KEYS`):
  `3dc196f08c776f693ffbc69608aab9b3118602f58dfde914923871f954357ff8`.
  It is not the Android signing key and not a device key: one key, one job.
- **Rotation.** `TRUSTED_KEYS` is a list. A new key is added in one release and the old one removed
  in a later one; a build accepts a signature from any key on its list.
- **Verification order.** (1) the signature over `SHA256SUMS` verifies against a pinned key, or the
  check ends as "could not verify" and nothing is offered; (2) the file names in it give the
  version (`playdrop-<version>-linux-amd64.deb`), which must be strictly newer than the running
  one, or nothing is offered. Every `playdrop-` name must be exactly `playdrop-<version>-` plus
  one of `android.apk`, `ios-unsigned.ipa`, `linux-amd64.deb`, `linux-x86_64.flatpak`,
  `windows-x64-setup.exe` (and `playdrop-cosmic-applet-<version>-linux-amd64.deb`), all with one
  version, or the whole file is refused: `playdrop-0.11.0-rc.1-linux-amd64.deb` can never read
  as 0.11.0; (3) after "Update", the downloaded file's SHA-256 must equal the
  signed line for its exact name, or the file is deleted and the update refused with a message.
- **What the signature protects against.** A tampered download, a swapped file, a forged "newer
  version" pushed by someone who controls the network or a mirror, and a downgrade (step 2). TLS
  with the public web roots is still used, but the signature is what decides.
- **What it does not.** A stolen signing key or a compromised release run (out of scope with the
  rest of the supply chain, threat model section 5). Someone who can block GitHub can keep a
  device from learning of an update; that is a denial, not a compromise, and the app says when a
  check failed.
- **By hand.** Anyone can check a download without PlayDrop:
  `openssl pkeyutl -verify -pubin -inkey playdrop-update.pub -rawin -in SHA256SUMS -sigfile <(base64 -d SHA256SUMS.sig)`
  then `sha256sum -c SHA256SUMS --ignore-missing` (`docs/release/distribution.md`).

Releases up to 0.10.0 have no signature file. A build with the update check never offers them: it
only ever looks at "latest", and it refuses anything unsigned.

## 5. Where the code lives

- `playdrop_core::update` (pure, no network, no clock, no files): the pinned keys, signature and
  checksum-file parsing, version comparison, the asset name per install kind, the once-a-day rule,
  the redirect host rule and the URLs. One parser for every platform, fuzzed like the other
  parsers (SR-33). `playdrop-ffi` 0.6.0 binds it for Kotlin and Swift.
- Desktop: `playdropd` does the two requests and the download (`crates/playdropd/src/update/`),
  because it owns the settings and outlives the window. The window and the applet only ask over
  IPC ("a UI opened") and show what comes back. The daemon never starts a check on its own.
- Android and iPhone: the app does the requests with the platform HTTP client and hands the bytes
  to the core for every decision. The app passes its version exactly as built, suffix and all;
  `update_check_available` says beforehand whether it is a release, so a pre-release build never
  asks, and `update_offer` refuses such a version too.
- The HTTP client on desktop is `ureq` with rustls and the bundled Mozilla roots: no system
  library, the same on Linux and Windows, and inside the Flatpak.

## 6. Installing

- **`.deb`.** Files are downloaded to a fresh directory under the daemon's own data dir (mode
  700, files 600). Right before the install step the daemon asks `dpkg-query` again whether the
  COSMIC applet's package is installed (the answer cached when the offer was made is not used):
  if it is, the applet's new `.deb` goes along; if the question fails, the update stops with a
  plain sentence instead of reading the failure as "not installed", since the applet depends on
  `playdrop (= version)` and updating the main package alone would remove it. The files are then
  made read-only (0400 in a 0500 folder), hashed once more from an open descriptor, and installed
  by absolute path with

  ```
  pkexec /usr/bin/apt-get install --yes --no-install-recommends --no-remove \
      -o DPkg::Lock::Timeout=60 \
      -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold \
      /home/<user>/.local/share/PlayDrop/updates/<version>-<time>-<n>/playdrop-<version>-linux-amd64.deb \
      [.../playdrop-cosmic-applet-<version>-linux-amd64.deb]
  ```

  `pkexec` shows the system's own password prompt; PlayDrop never sees the password. `apt-get`
  resolves dependencies and refuses a broken package; `--no-remove` makes it refuse any plan that
  would remove a package rather than carry it out; the lock timeout waits up to a minute for a
  running unattended upgrade; the two `Dpkg::Options` keep a configuration file the user changed
  without asking. On failure or a dismissed prompt the old version keeps running and the window
  says what happened.
- **Restart after a `.deb` install.** Under the systemd user unit `playdropd.service` (D-Bus
  activation goes through it when the session has a user manager) the daemon asks systemd to
  restart the unit (`systemctl --user --no-block restart`): the unit is `Type=dbus`, and systemd
  would stop a new process started in place once the old bus name went away. Started any other
  way (XDG autostart, a terminal, the bus's own activation), the daemon shuts down cleanly and
  `exec`s `/usr/bin/playdropd` with its own arguments, keeping its PID; the new process takes the
  bus name afresh, so if something else started a daemon in the gap exactly one remains.
  Clients see `restarting`, then `shutting_down`, and follow the name back
  (`crates/playdropd/src/update/restart.rs`).
- **Windows.** The installer is downloaded to a fresh folder under
  `%LOCALAPPDATA%\PlayDrop\updates`, beside the data dir rather than in it: the data dir has a
  protected owner-only DACL (SR-28), and when an administrator's password is typed for a standard
  user ("over the shoulder" elevation) the elevated setup runs as that administrator and must
  read the file. The folder keeps the ACL it inherits from `%LOCALAPPDATA%` (the user, `SYSTEM`,
  Administrators) and nothing is added, so nobody wider than that can read it. The file is hashed
  once more, then started through the shell (`ShellExecuteExW`) with `/SILENT /RELAUNCH=1` when
  the update was started from the window, or `/SILENT /RELAUNCH=daemon` when it was not (the
  command line), so the installer starts again what was running. Inno Setup's loader starts the
  elevated setup, so Windows shows its permission prompt. The installer stops the daemon and the
  window before replacing them. The daemon waits for the loader to exit: a successful install
  stops this daemon first, so a daemon that sees the exit was not replaced, and it moves to
  `failed` (stage `install`, offer kept) with a sentence from Inno Setup's documented exit code
  (2 and 5 cancelled, 1 failed to start, which is how a declined permission prompt is expected to
  show, others failed) and removes the file.
- **Android.** The APK is downloaded to the app's cache, verified, and given to the system package
  installer (`REQUEST_INSTALL_PACKAGES`, the person confirms in Android's own dialog). An APK
  signed with another key than the installed app is refused by Android and the app says so: this
  is what a debug-key test install sees.
- **Cleanup.** A failed, cancelled or refused update removes its folder at once. A successful
  `.deb` install removes it before the restart. On Windows a successful install stops the daemon
  while the installer still runs from the file, so that folder is removed at the daemon's next
  start (every start clears `updates`; a file still locked by a running installer is left for the
  start after). Nothing downloaded is kept for later.
- **What remains (residuals).** The download is hashed while it streams and once more from an
  open descriptor right before the hand-over, with the files read-only and their folder 0500 on
  Linux. The system installer (`apt-get` as root, Inno's setup as an administrator) still opens
  the file by path afterwards, so another program running as the same user could, in the moment
  between the second hash and that open, restore write access (it owns the folder) and swap the
  file. That attacker already runs code as the user, which the threat model places out of scope
  (section 5); the steps above only shrink the window and make an accidental change visible. On
  Windows the file is not made read-only (the folder's inherited ACL lets the user change it);
  the second hash is the only narrowing there. Over-the-shoulder elevation relies on the
  inherited `%LOCALAPPDATA%` ACL granting Administrators: on a profile whose ACL was changed to
  remove them, the elevated setup cannot read the file, exits with an error and the daemon
  reports the install as failed.

## 7. Threat model additions

Boundary **B9, the update channel**: never trusted for content. It can only say "nothing new" or
hand over bytes that must pass section 4.

| # | Threat | Mitigation |
|---|---|---|
| T51 | The update check tells a third party who runs PlayDrop, on what, and when | Fixed URL, fixed User-Agent, no identifiers, only on opening a UI, once a day, switch (SR-45) |
| T52 | A network attacker, a hijacked mirror or a forged release serves a malicious or older build | Pinned Ed25519 signature over the checksums, strict newer-than rule, hash of the exact file name (SR-46) |
| T53 | The update path installs or elevates without the person | Nothing downloads before "Update"; installs go through the OS prompt (`pkexec`, UAC, Android's installer); no stored credentials (SR-47) |
| T54 | A hostile server feeds oversized or malformed answers, or redirects to another host | Size and time limits, redirect allow list, the parser is fuzzed and has no `unsafe` (SR-48) |

- **SR-45.** A check is made only when a UI opens or "Check now" is pressed, only with the switch
  on, at most once per 24 h (one hour after a failure). The request is the fixed URL with
  `User-Agent: PlayDrop` and carries no version, OS, name, ID, cookie or cache validator. *Test:*
  a stand-in server records the request head and the test compares it byte for byte; with the
  switch off the server sees nothing.
- **SR-46.** Nothing is offered unless `SHA256SUMS` carries a valid signature from a pinned key and
  names a strictly newer version. Nothing is installed unless the downloaded file's SHA-256 equals
  the signed line for its exact name. *Test:* a flipped bit in the sums, the signature or the
  file, an older version, and a file under another name are each refused.
- **SR-47.** Download starts only on the person's "Update". Installing always passes the OS's own
  consent (polkit, UAC, the Android installer). PlayDrop stores no password and runs nothing
  elevated itself. *Test:* a dismissed prompt leaves the old version running and reports it; a
  Windows installer that ends without replacing the daemon is reported from its exit code; the
  files are gone afterwards.
- **SR-48.** `SHA256SUMS` over 64 KiB, a signature over 1 KiB, a download over 512 MiB or of the
  wrong length, more than 5 redirects, a redirect off the allow list or to plain HTTP, a check
  request over 15 s, and a download that breaks the download time rule (section 3) each end the
  check or the update as failed. *Test:* each case against the
  stand-in server; the parser has a fuzz target.
- SR-32 extension: URLs and versions may be logged; nothing about the answer's headers is.

## 8. Testing without a real release

- A debug build of the daemon accepts `PLAYDROP_UPDATE_BASE` (another base URL, only a stand-in on
  this computer: `http://127.0.0.1:<port>` or `http://localhost:<port>`; any other value, an
  `https` host included, is ignored) and `PLAYDROP_UPDATE_TEST_KEY` (one more trusted key, hex).
  Release builds do not contain the code that reads them (`cfg(debug_assertions)`), so a shipped
  PlayDrop cannot be pointed elsewhere.
- The Android debug build type does the same through an intent extra and a debug-only network
  rule for `localhost`; the release build type has neither. The one-more-key entry point,
  `update_offer_with_test_key`, exists only in a `playdrop-ffi` built with the cargo feature
  `update-test-key`, which the Android debug build type turns on (its native library and its
  Kotlin bindings are built per build type); the release library has no such function, and the
  iPhone app never builds it.
- The stand-in is a local HTTP server with a throwaway key. It proves the request head, every
  refusal in SR-46 and SR-48, and the whole flow up to the OS prompt.

## 9. Not done, on purpose

- No automatic install, no background download, no "skip this version" memory beyond a day.
- No release notes shown inside the app: that would be a third request and unsigned text. The
  offer links to the release page.
- No delta updates, no staged rollout, no beta channel.
- No update of the Flatpak or the iPhone app from inside PlayDrop.
