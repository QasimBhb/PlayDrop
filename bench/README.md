# bench/

Scripts that produce the throughput notebook (`docs/release/throughput.md`) the same way
every time. Nothing here writes into the repo: datasets live under `/tmp/pd-bench`,
downloaded or built tools under `/tmp/pd-bench-tools`.

| File | Measures | Needs |
|---|---|---|
| `make-dataset.sh` | Builds the five datasets and prints counts, bytes and manifest checksums. Seeded, so the bytes are identical on every machine. | python3, an ext4 directory |
| `loopback.sh` | PlayDrop HTTP/2 and raw framing against iperf3 on 127.0.0.1. Protocol and crypto cost only, no radio. | `cargo build --release -p playdrop-cli`, iperf3 |
| `iperf3-phone.sh` | The Wi-Fi ceiling: desktop to phone and phone to desktop, 1 and 4 streams. | adb, iperf3, an arm64 Android iperf3 (the script header says how to build it) |
| `desktop-to-phone.sh` | PlayDrop end to end, desktop to phone, per dataset, then checks file count and sha256 of every file on the phone. | `playdropd` running and paired, adb, python3-gi |
| `pd_ipc.py` | Helper for the script above: passes files to the daemon as file descriptors (`SendFiles`) and polls `transfers`. | python3-gi |
| `incumbents.md` | Manual steps to time LocalSend and KDE Connect on the same datasets. | the user |

## Order of a full run

1. `bench/make-dataset.sh` (about 10 s; skips datasets that already verify).
2. Note the link: `nmcli -f IN-USE,SSID,CHAN,RATE,SIGNAL dev wifi | grep '^\*'` or
   `iw dev <iface> link`, and `adb shell dumpsys wifi | grep -i "link speed\|frequency"`.
   The scripts print the phone side themselves.
3. `bench/iperf3-phone.sh`, with nothing else using the Wi-Fi.
4. Start the daemon, then `bench/desktop-to-phone.sh` (the start line is in `CLAUDE.md`,
   "Commands": `HOME=/tmp/pd-home target/release/playdropd --data-dir /tmp/pd-a --receive-dir /tmp/pd-a-in --port 42187 --name alpha --no-tray --no-autostart`).
5. `bench/loopback.sh` on a quiet machine (the figures fall a lot when other builds run).
6. `bench/incumbents.md` by hand.
7. Copy the tables into `docs/release/throughput.md` with the date and the link figures.

## Datasets

| Name | Content | Why |
|---|---|---|
| `big-1g` | one 1 GiB file | the headline single-file number, compared with iperf3 (floor 50%, stretch 80%) |
| `small-1000` | 1,000 files of exactly 100 KiB | the file-count collapse LocalSend reports |
| `mixed-5000` | 5,000 files of 4 to 64 KiB | the "thousands of small files" case |
| `medium-100` | 100 files of 1.0 to 2.3 MiB, about the same bytes as `mixed-5000` | with `mixed-5000`, the "no drop between 100 and 5,000 files" check (same bytes, 50 times the files) |
| `tree` | 160 files in a 4-level folder tree plus empty folders | folder structure survives; Android keeps no empty folders |

## Reading the numbers

- `desktop-to-phone.sh` wall time starts just before `SendFiles` and ends at the first
  100 ms poll that sees the transfer finished. It includes manifest building, BLAKE3 on
  both sides and the phone's Downloads writes. MiB/s is file bytes over that time.
- The phone folder is `/sdcard/Download/PlayDrop/<dataset>/` (the app's Downloads sink,
  `DownloadsSink.kt`). The script deletes the dataset folder before each run.
- A row without "yes ... all sums match" is a failure, whatever its speed.
- Do not run `iperf3-phone.sh` and `desktop-to-phone.sh` at the same time; they share
  the one radio.
