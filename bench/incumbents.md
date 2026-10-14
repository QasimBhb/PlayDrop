# Timing LocalSend and KDE Connect on the same datasets

Manual steps, because neither app has a scriptable send. Use the datasets from
`bench/make-dataset.sh` (`/tmp/pd-bench/<name>`), the same Wi-Fi, the phone in the same
place and the same screen state as the PlayDrop runs. Do each run three times and write
down all three. Record failures and stalls too: "gave up at 40%" is a result.

Direction to measure first: desktop to phone, because that is what
`bench/desktop-to-phone.sh` measures. Phone to desktop is a second table if you want it.

## Common timing method

Stopwatch error would swamp the small-file runs, so use file times instead.

1. Put both clocks on one scale. In a desktop terminal:
   `d=$(date +%s.%N); p=$(adb shell date +%s.%N | tr -d '\r'); echo "$p - $d" | bc`
   Call the result `offset` (phone minus desktop, seconds). It is usually under 1 s.
2. Just before you tap Send (or press Enter on the command), run
   `date +%s.%N` on the desktop and note it as `t0`. Pressing the key and running
   `date` in another terminal window is accurate to about a second, which is fine for
   runs over 20 s. For shorter runs, use a screen recording of both screens and read the
   frame times.
3. When the receiving app says done, find the newest file time on the phone:
   `adb shell 'find <folder> -type f -printf "%T@\n" 2>/dev/null | sort -n | tail -1'`
   If `-printf` is missing on the phone's `find`, use
   `adb shell 'ls -lt --full-time <folder> | head -2'`.
   Subtract: `elapsed = t_last_file - offset - t0`.
4. MiB/s = total MiB of the dataset (`bench/make-dataset.sh` prints it) over `elapsed`.
5. Check the count: `adb shell 'find <folder> -type f | wc -l'` must equal the dataset's
   file count (`1`, `1000`, `5000`, `100`, `160`). Record the count you got.

The PlayDrop figure is wall time from the call to the transfer being marked complete,
which includes a little more bookkeeping at the end than "last file written". Say so in
the notebook; the difference is under a second.

## LocalSend

Desktop: already installed as the Flatpak `org.localsend.localsend_app`
(`flatpak run org.localsend.localsend_app`).

Phone: not installed. Install from one of:
- Google Play ("LocalSend"), or
- F-Droid ("LocalSend"), or
- the signed APK on the project's GitHub Releases page
  (https://github.com/localsend/localsend/releases, file `LocalSend-<version>-android-arm-v8.apk`),
  then `adb install -r <apk>`.

Steps (desktop to phone):
1. Open LocalSend on both. Both must show "Quick Save" or accept the incoming request;
   turn Quick Save on for Favorites or All on the phone so no tap is needed during the
   run, and note which you used.
2. In the desktop app: Send, Files (or Folder for `tree`, `small-1000`, `mixed-5000`,
   `medium-100`), choose the dataset, pick the phone, note `t0` as you click.
3. Wait for "Finished". Record the elapsed time and the received file count as above.
   LocalSend saves to its own folder (Settings, Destination); use that path in step 3 of
   the common method.
4. Delete the received files from the phone before the next run.
5. Note the LocalSend version (About) and that PlayDrop was not running on either device
   (stop `playdropd` and force-stop the PlayDrop app), or say it was; either is fair, but
   say which.

LocalSend uses port 53317 and PlayDrop never binds it, so both can stay installed.

## KDE Connect

Phone: already installed (`org.kde.kdeconnect_tp`). Open it once and allow the storage
permission it asks for.

Desktop: `sudo apt install kdeconnect`. Installing it and running it beside PlayDrop is
also the coexistence check from PLAN.md Phase 6: start `playdropd` first, then
`kdeconnect-app` (or `kdeconnectd`), and confirm both come up. Neither may report a port
conflict (KDE Connect uses TCP/UDP 1714-1764, PlayDrop 42187). Record this as its own
line in `docs/release/test-matrix.md`.

Steps (desktop to phone):
1. Pair them in `kdeconnect-app` and accept on the phone.
2. Find the device id: `kdeconnect-cli -a --id-only`.
3. Send: `kdeconnect-cli -d <id> --share <file> [--share <file> ...]`. KDE Connect has no
   folder send; for the folder datasets pass every file (for `tree` that is 160 `--share`
   arguments, e.g. `args=$(find /tmp/pd-bench/tree -type f -printf '--share %p ' ); kdeconnect-cli -d <id> $args`),
   and say in the notebook that folder structure is flattened.
4. Note `t0` just before pressing Enter. KDE Connect on the phone shows a notification
   per file; use the last file's time as in the common method. Received files land in the
   folder set in the phone app (Settings, Download path).
5. If it stalls or the phone app is killed, record how far it got. 5,000 files may not be
   practical; run `small-1000` and stop the 5,000 case at 15 minutes with "not finished".
6. Note the versions (`kdeconnect-cli --version`, and the phone app's version).

## Where the results go

`docs/release/throughput.md`, tables "LocalSend" and "KDE Connect": one cell per dataset
with MiB/s, the received count, and the three raw run times in the notes.
