# Third-party code

PlayDrop is licensed GPL-3.0-or-later (see `LICENSE`). Code reused from other projects is listed
here. Every reused file keeps its original copyright header and carries an SPDX tag naming its
original licence. GPL-3.0-or-later can absorb both GPL-2.0-or-later and Apache-2.0 code, which is
why this licence was chosen.

Rules:
- Add a row before merging any reused or closely adapted file.
- Keep the original copyright header intact. Add our own line below it if we changed the file.
- Apache-2.0 files also need their NOTICE text preserved if the upstream project ships one.
- Studying a project's protocol documents does not need a row. Copying or translating its code does.

| Path in PlayDrop | Origin | Upstream path | Licence | What was reused |
|---|---|---|---|---|
| (none yet) | | | | |

Candidate sources named in `PLAN.md`:
- KDE Connect (`kdeconnect-kde`, `kdeconnect-android`): GPL-2.0-or-later. Plugin logic for MPRIS,
  mousepad input, battery.
- LocalSend (`localsend/localsend`): Apache-2.0. Rust HTTP sender tuning, Android background
  service and share-sheet handling.
- Not cleared: GSConnect, Valent, cosmic-utils/kdeconnect. Confirm their licences before reuse.
