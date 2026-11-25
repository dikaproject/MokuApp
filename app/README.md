# Moku

Moku is a "dynamic island" for Windows: a small glass pill at the top of the screen that
shows what is going on (music, timers, notifications, battery, calls, permission requests
from AI tools) and opens into a panel with tabs when you hover or click it. A separate
settings app (with a first-run onboarding) edits its configuration, and `mokuctl` lets
scripts and tools talk to it.

Moku is desktop-only for now. The UI is Indonesian by default, with English available.

## Install

1. Download `moku-setup.exe` from the latest GitHub release.
2. Run it. It installs for your account only (no admin rights) into
   `%LOCALAPPDATA%\Programs\Moku`, adds a Start Menu shortcut and an entry in Apps & Features,
   and can start Moku with Windows.

**SmartScreen:** the installer is not code-signed yet, so Windows SmartScreen may say
"Windows protected your PC" the first time. Choose "More info" → "Run anyway". To check the
file first, compare its SHA-256 with `moku-setup.exe.sha256` from the same release:
`(Get-FileHash .\moku-setup.exe -Algorithm SHA256).Hash`.

- Silent install: `moku-setup.exe /S` (add `/noautostart`, `/nolaunch` as needed).
- Uninstall: Apps & Features → Moku, or `moku-setup.exe --uninstall` (the window asks whether
  to delete your data in `%APPDATA%\Moku`; silent `--uninstall /S` keeps it unless `/deletedata`.
  Deleting data removes Moku's own files only (see "Configuration and data"), never other things
  that happen to be in that folder, and only in a folder carrying the `.moku-data` marker).
- Portable: `Moku-<version>-portable.zip` has the same three exes; unzip anywhere and run
  `moku.exe` (no shortcut, no updater install; the update pill opens the release page instead).
- Updates: Moku checks GitHub once a day and shows "Moku x.y.z tersedia" with "Perbarui". The
  downloaded installer is only run when its SHA-256 matches the release's `.sha256` **and** the
  release's `moku-setup.exe.sig` is a valid ECDSA P-256 signature of that digest by the key built
  into Moku; a release without a signature is never offered. Turn it off in Settings → Tentang →
  "Periksa pembaruan otomatis" (the switch is greyed out in a build that has no repository or
  signing key set: "Belum aktif di build ini").

## Requirements (building from source)

- Windows 10 1903 or later, or Windows 11 (x64)
- Rust with the MSVC toolchain. `rust-toolchain.toml` pins the version (1.100.0); rustup installs
  it by itself the first time you run `cargo` in the checkout.
- Visual Studio Build Tools (the "Desktop development with C++" workload, for `link.exe`
  and the Windows SDK)

## Build and run

```powershell
cd app
$env:CARGO_TARGET_DIR = "<repo>\.build\main"   # optional; keeps build output out of the repo
cargo build --release --locked   # moku.exe, moku-settings.exe, mokuctl.exe
cargo test --workspace --locked
& "$env:CARGO_TARGET_DIR\release\moku.exe"
```

- Keep the checkout and `CARGO_TARGET_DIR` at **short paths** (like `<repo>` = `C:\src\moku`). Very long
  paths make `link.exe` fail with `LNK1104` while linking build scripts.
- Stop a running `moku.exe` before rebuilding (tray → Keluar, `Ctrl+Alt+Shift+Q`, or
  `Stop-Process -Name moku`), or the linker cannot replace the exe.
- The three exes must stay in the same folder: the island starts `moku-settings.exe` from
  next to itself.
- On the first run the island opens the settings app, which shows the onboarding.
- `cargo build` also builds `moku-setup.exe`, but without the binaries inside (it refuses to
  install). The real installer comes from `app\tools\release.ps1` (see "Releasing").

## Features

The panel's tabs, in order:

| Tab | What it does |
|---|---|
| Musik | Current media session (any player that uses Windows media controls): art, title, timeline, play/pause/next/prev, synced lyrics from LRCLIB, live bars in the pill. Volume pill on volume changes. |
| Hari ini | Date and week strip, today's agenda from your iCal (`.ics`) calendars (Google "secret address", Outlook "publish", any `.ics`; repeating events, time zones and Meet / Zoom / Teams "Gabung" buttons), a local to-do list you add to, edit and delete in the panel (with a due time that raises a reminder: see "To-do with a time"), Indonesian public holidays and cuti bersama (with a "Besok libur" pill the evening before), weather from Open-Meteo for the configured city, rain heads-up, small chips for an exchange rate and the air quality, and the latest BMKG earthquake and weather warnings (credit "Sumber: BMKG"). |
| Sehat | Drink-water and eye-break reminders, prayer times (Kemenag RI or Malaysia JAKIM angles), the Hijri date (also in the Hari ini header; Kemenag's published month starts from 1 Muharram 1447 to 1 Muharram 1449 H, embedded as data with their sources in `features/hijri.rs`, the tabular calendar outside that range, and a -2..+2 day correction on top for regional differences), and the Ramadan pack: imsak and buka puasa countdown ring in the island (prominent in the last 30 minutes), "Selamat berbuka" at Maghrib, opt-in puasa sunnah reminders (Senin-Kamis, Ayyamul Bidh, Arafah, Tasu'a and Asyura) and an optional adzan sound at prayer time (a generated chime or your own file; your media is paused first and only what Moku paused is resumed; no recording ships with Moku). Off by default. |
| Kontrol | Six tiles, three and three: Wi-Fi, Bluetooth (names a connected headset and its battery, tinted when on or connected), Jangan ganggu, Mode gelap, Tetap menyala, Mode rapat; volume and brightness sliders, output device, and the per-app volume mixer (chip "Mixer per aplikasi": a slider and a mute button for every app that is playing sound). |
| Shelf | Drop files on the island to park them (or press "Shelf" on a finished download); drag them back out. Click a file to **send it to your phone by QR code** or **convert it** (images, audio, video); "Terima dari HP" receives files and text from the phone (see "Kirim ke HP"). Recent Shelf files also show up in the launcher. |
| Timer | Timer presets, focus/pomodoro with breaks (can hold notifications), stopwatch. |
| Clipboard | History of text and images, click to copy back, pin, auto-expire; skips password managers and ignored apps. **Smart actions**: a colour gets HEX / RGB / HSL buttons, a link gets "QR" (a QR code in the panel) and "Buka", a sum shows its result, minified JSON gets "Rapikan", an Indonesian mobile number gets "WhatsApp", and a one-time code (OTP) is held in memory only and dropped after 60 s. |
| Tangkap | Area (Windows screen clip), window and full-screen screenshots to `Pictures\Moku` and the clipboard, and **Teks (OCR)**: drag a region of the screen and its text is copied (`Ctrl+Alt+T`, or `ocr` in the launcher). |
| Monitor | CPU, GPU, RAM, disk and network with sparklines. Off by default. |
| Notifikasi | Notifications sent through `mokuctl`, do-not-disturb, AI permission cards, Claude Code sessions. |

Pills outside the panel: battery low / charging (plugging in shows a battery that fills up to
the current level, 2.5 s), Bluetooth headset connected with its battery, Caps Lock, internet lost
/ back, camera and microphone in use, meeting detected (Zoom, Teams, Meet) with a mute button,
progress bars, "Rapat 5 menit lagi" before a calendar meeting (with "Gabung" when it has a Meet,
Zoom or Teams link), and browser downloads (name, size, speed; then "Buka", "Folder" and "Shelf"
when done; "Buka" is never offered for programs and scripts).
Two activities at once show as a pill plus a small bubble. Claude Code sessions (with the
hook installed): "Bekerja…" with a running time, "Butuh input" when Claude waits for you,
"Selesai · 3m 12s" when a turn ends, one pill per session.

### To-do with a time

Type into "+ Tambah to-do" in the Hari ini tab (click it, type, Enter; Esc cancels), or use
`mokuctl todo add "…"` or the launcher's `todo …`: all three read the same words, in Indonesian and
English. A time in the words becomes a reminder: at that time a pill appears (priority 85) with
**Selesai** and **Tunda 10 m**, and the to-do shows the time under its text (red when overdue).
Edit with the pencil or a double-click, delete with ×.

| You type | The to-do is due |
|---|---|
| `rapat jam 3`, `rapat pukul 15.30`, `rapat 15:30` | the next 15:00 / 15:30 |
| `kirim laporan besok 09:00`, `besok jam 9`, `lusa` | tomorrow 09:00 / the day after tomorrow 09:00 |
| `senin depan`, `jumat`, `tanggal 28`, `28 desember` | that day at 09:00 |
| `telepon dokter nanti malam`, `besok pagi` | tonight 20:00 / tomorrow 08:00 |
| `30 menit lagi`, `dalam 2 jam`, `setengah jam lagi` | that long from now |
| `tomorrow 9am`, `at 3pm`, `next friday`, `in 2 hours`, `tonight` | the same, in English |
| `malam ini jam 12`, `besok jam 12 malam`, `besok 24:00` | 00:00 tomorrow / 00:00 the day after tomorrow / the same (a night hour 0-5 with a day named, and `24:00`, belong to the next day) |
| `rapat jam 3 sore WITA` | 15:00 WITA, converted to your PC's zone (in WIB: 14:00); `WIB` is UTC+7, `WITA` +8, `WIT` +9 |

A bare hour such as `jam 3` is ambiguous (03:00 or 15:00): it means the next time that clock shows,
ignoring 00:00–05:59 unless it is night now; with a day named (`besok jam 3`) 7–12 is the morning and
1–6 the afternoon. A day alone is 09:00. A time with a leading zero (`03:00`) or an hour above 12 is a
24-hour time exactly. The full rules are in the header of `moku/src/features/today/due.rs`, and its
tests are the table of examples. Words that are not times (`jam tangan`, `3 jam`, `versi 1.20`) stay text.

## Polite island

Settings → Island (or `[island]` in `config.toml`) decides where the island lives and when it
gets out of the way:

| Setting | What it does |
|---|---|
| Monitor (`monitor`) | `primary`, `cursor` (the island follows the pointer to another monitor after 300 ms there), or one monitor by device name (`\.\DISPLAY2`; the primary one when it is not connected). One island per monitor (`all`) is not implemented. |
| Letak + Geser halus (`anchor`, `offset_x`) | Left / centre / right along the top edge plus a nudge in DIP. The island opens its panel from its resting centre, so it cannot get closer to a side than half a panel. |
| Minggir saat kursor lewat (`move_aside`) | `off` (default), `fade` or `slide`: when the pointer heads for the area under the island (the browser's address bar, a tab) and is not aiming at it, the island fades to 30 % or shifts sideways, and clicks go through to what is underneath. It comes back after the pointer has left, or when you rest the pointer on it. |
| Sembunyi sampai tepi atas (`hide_until_edge`) | The island appears when the pointer rests on the top edge near it (a band of 6 px when another monitor is above), or when something new arrives, and hides again afterwards. It never hides what you may need to answer: the panel or a sheet, a pill with a button (a timer, a reminder), or any notification-priority activity (a notification, an AI card, a tsunami warning, a finished timer). |
| Sembunyi di depan aplikasi ini (`hide_apps`) | Process names (`chrome.exe`); the island hides while one of them is in front, like a full-screen app (a tsunami warning and a sheet you opened with the launcher, OCR or Tanya AI hotkey still show it). "Aplikasi aktif" adds the next app you switch to. |
| Abaikan musik dari aplikasi ini (`media_ignore_apps`) | Media sessions of these apps (`Spotify.exe`, or part of their source id) never show the music pill. |
| Gerak (`appearance.animations`) | Normal, Kurangi (no overshoot, shorter, fades; away moves only fade) or Mati. Windows' "Animation effects" off always wins. |

Move aside and hide-until-edge need the pointer's position: while one of them (or "follow the
cursor") is on, the island reads it with `GetCursorPos` every 100-200 ms (every 400 ms once the
pointer has rested, also while the island is aside or hidden, about every 30 ms while it is near the island)
and whether the left button is held, to tell a click from a rest: a pointer that moves with the button down
is a drag and never moves the island aside. While the island is not on screen (paused, a full-screen app, an
app of `hide_apps`) the timer is not armed at all. "Follow the cursor" does not move the island while the panel
or a sheet is open or a text field has the keyboard. Nothing is recorded, logged or sent, no hook is installed,
and nothing is ever sent to another window. With all of them off (the default) nothing is polled.


## Kirim ke HP (phone transfer) and the Shelf converter

**Send a Shelf file to your phone, or receive files and text from it, over the same Wi-Fi, with no
app on the phone.** In the Shelf tab, click a file and press "Kirim ke HP", or press "Terima dari HP".
The island shows a QR code; scan it with the phone's camera and a small web page opens. Settings →
Fitur → "Kirim ke HP" turns the feature off.

- **How it works.** Moku starts a temporary HTTP server on **one** address: the IPv4 of your real
  Wi-Fi / LAN adapter (the one with a default gateway; virtual adapters, Hyper-V, WSL, VMware, VPNs
  and tunnels are skipped; "Ganti" switches when there are several), on a random port. It exists only
  while the transfer is open: at most 10 minutes, and a minute after a transfer finished. When no
  transfer is open nothing listens and nothing runs.
- **The link** carries a random 128-bit token (compared in constant time). Only the **first device**
  that opens it is accepted; another IP is refused before anything is read. A wrong or missing token,
  an unknown path or another `Host` name is answered like any unknown address. There are no directory
  listings and no file is served by path: a download is "file number n of the list you offered", an
  upload "file number n of the list the phone announced".
- **You decide.** Nothing is sent or stored before you press **Terima** on the island: "HP
  192.168.1.23 ingin mengirim 2 file". "Tolak" (or no answer for 90 s) ends the session. A warning
  is shown when a program or script is among the files (Moku never runs received files).
- **Received files** are streamed to `Downloads\Moku` (never replacing a file: `name (2).ext`), up
  to 2 GB each, under a sanitised name (no path, no `:` streams, no reserved device names, no
  direction-override characters), written as `.mokupart` and renamed only when complete; an
  interrupted upload leaves nothing. They appear on the Shelf. A **text** from the phone appears as a
  pill with "Salin"; it is copied only when you press it, and never logged.
- **A Public network is refused.** Windows asks "Public or Private?" for every new network. Moku
  reads that category (`INetworkListManager`) and, when the only usable network is a **Public** one (a
  cafe, a hotel), shows "Jaringan publik: kirim ke HP dimatikan demi keamanan" with **Batal** and
  **Tetap lanjutkan**. The second starts this one session on that network; nothing is remembered, so
  the next session asks again. A Private or domain network, or one Windows reports no category for, is
  used without asking, and a Private cable is used instead of a Public Wi-Fi.
- **Received files carry the Mark-of-the-Web** (`Zone.Identifier`, `ZoneId=3`, on NTFS), so Windows treats
  them like a browser download: SmartScreen and Office's Protected View apply. "Folder" only opens the
  folder; a program or script from the Shelf or the launcher is shown in Explorer, never started.
- **It is plain HTTP.** The traffic on your Wi-Fi is not encrypted; the card says so. Do not use it
  on public Wi-Fi (Moku refuses it unless you insist, above). The server also refuses requests that lack our own origin and `X-Moku` header,
  caps request headers at 8 KB, cuts off slow clients (10 s for the request head, 30 s without a
  byte for a body or a download) and serves at most 8 connections at once.
- **Windows Firewall.** The first time Moku listens, Windows asks whether to allow it on your
  network. Choose **Private networks**. Moku never changes the Firewall itself. If you block it, the
  phone can not connect; the card suggests checking this after 20 s.
- **Developer switches** (need `MOKU_DEBUG=1`): `MOKU_PHONE_BIND=127.0.0.1` listens on loopback instead
  of the LAN adapter (only a loopback address is accepted, never `0.0.0.0` or a LAN address),
  `MOKU_PHONE_DIR` replaces the receive folder, and the island writes the session address to
  `phone-debug.txt` in the data folder for the test client (`tools\phone-test-client.ps1`, which
  refuses any host that is not loopback). `phone.sample:<waiting|send|connected|card|card-risky|
  progress|done|text|done-pill|public>` draws the UI with sample data.

**Ubah ke… (converter).** Click a Shelf file and press "Ubah ke…":

| From | To |
|---|---|
| images (PNG, JPG, BMP, TIFF, GIF, WebP, HEIC, AVIF) | JPG, PNG, WebP (WebP only when Windows has a WebP *encoder*) |
| audio (MP3, WAV, M4A, AAC, WMA, FLAC, OGG, Opus) | MP3, M4A |
| video (MP4, MOV, MKV, AVI, WMV, WebM, M4V) | MP4, M4A, MP3 |

Images go through WIC in bands (EXIF orientation is applied, transparency becomes white for JPG),
audio and video through `Windows.Media.Transcoding.MediaTranscoder`; the tile shows a progress ring
(click it to cancel). The result is `name (Moku).ext` next to the original, `name (Moku 2).ext` when
that exists: an existing file is never replaced, and a failed or cancelled conversion removes only the
file it created. **HEIC / AVIF / WebP decoding needs the free Microsoft Store extensions** (HEIF Image
Extensions, AV1, WebP Image Extensions): without them the island says so and offers "Buka Store";
nothing is installed by Moku.

## Launcher

`Alt+Space` (configurable in Settings → Pintasan → Peluncur, or `[launcher] hotkey` in `config.toml`)
turns the island into a search field with up to seven results; Esc, a click elsewhere or the hotkey
again closes it, ↑/↓ select, Enter runs, `Alt+1`–`Alt+7` run a row directly.

| You type | You get |
|---|---|
| a few letters (`vsc` finds "Visual Studio Code") | apps from the Start Menu and packaged (Store) apps; fuzzy and acronym matching, ranked by what you pick |
| `12*3`, `200 x 10%`, `(4+5)^2` | the result; Enter copies it |
| `todo beli kopi` | adds a to-do |
| `timer 10m`, `timer 1h30m` | starts a timer |
| `> pengaturan` | a Moku settings page |
| nothing | what you picked recently (apps first); recent Shelf files only after them |

Recent Shelf files can be opened from the launcher, except **programs and scripts** (`.exe`, `.ps1`,
`.lnk`, `.msi`, `.iso` …): those are only shown in Explorer, never started from the island (the same list
the downloads pill uses, `moku/src/safety.rs`). Nothing is typed into other windows: the launcher only
receives the keys sent to its own window while it is open, and gives the foreground back when it
closes. If the hotkey is taken by another program the island says so once ("Pintasan Alt+Space dipakai
aplikasi lain") and keeps your setting; it also gives the hotkey back while the island is paused or a
full-screen app is in front. An app of `hide_apps` in front does **not** take the hotkeys away: the
launcher, OCR and Tanya AI still open, and the island shows itself for as long as their sheet is open.

How the app list is made: the island itself never loads the shell's heavy libraries for it. When the
launcher first opens, Moku starts itself once more as a short-lived helper
(`moku.exe --launcher-index <file>`, no window, below normal priority), which resolves the Start Menu
shortcuts, lists `shell:AppsFolder`, extracts 48 px icons and writes a compact cache
(`launcher-index.bin` in the data folder), then exits; the island reads only that file. The first time
(no cache yet) the list says "Mengindeks…" for a few seconds, later it shows the previous cache at
once; the helper runs again at most every 10 minutes while the launcher is in use. The mixer's and the
Shelf files' icons come from the same helper (`moku.exe --icons`). The parsed list and the icons are
dropped two minutes after the launcher was last closed.

**Caps Lock and your keyboard.** The Caps Lock pill (on by default; turn it off in the
settings app under Notifikasi → Pil sistem → Caps Lock) uses Raw Input, which delivers every
key event system-wide to the island's hidden window. The island only compares each event's
virtual-key code with `VK_CAPITAL` and drops everything else at once: no other key is stored,
logged or sent anywhere.

**Bluetooth headset battery.** When a paired Bluetooth headset or earbuds connect, the island
shows its name and battery level (and warns at 20, 10 and 5 %); the Kontrol tab's Bluetooth tile
shows the same. Windows has no documented API for the battery of a classic Bluetooth headset, so
Moku reads what Windows itself shows in Settings: the level Windows stores on the device (device
property `{104EA319-6EE2-4701-BD47-8DDBF425BBE5}` 2, read with `CM_Get_DevNode_PropertyW`). The key
is not in Microsoft's headers; it is documented by the community (for example the xm4battery
project) and verified here on a real headset. Consequences: a device that does not report a level
(many cheap headsets) is shown without a percentage; the level is the last one Windows heard, so it
can be a few minutes old; only audio devices (class "Audio/Video") are looked at, and
Bluetooth LE-only audio devices are not covered. Nothing is sent anywhere and nothing is paired or
connected by Moku. Switches: settings app, Fitur, "Daya & audio".

## Tanya AI (Ask AI)

Select text in any program and press `Ctrl+Alt+A` (or type `? your question` in the launcher): the
island becomes a card with **Terjemahkan** (Indonesian and English into each other), **Ringkas**,
**Jelaskan**, **Perbaiki tulisan**, a question field, and an answer that streams in. Then **Salin**,
**Ganti teks** (pastes the answer over the selection, only when you click it) and **Ingat ini**
(a fact the island remembers). It is off until you turn it on in Settings → Tanya AI and give it a
provider: OpenAI, OpenRouter, Groq, Gemini (OpenAI endpoint), DeepSeek, Ollama or LM Studio, or any
other OpenAI-compatible base URL. Ollama and LM Studio run on your PC and need no key.

**Privacy, in one place.** Your memory stays on this PC, encrypted for your Windows account. When you
ask, the selected text and the context you allow are sent to the provider you chose; every card says
"Dikirim ke <host>" and shows what goes along as chips you can remove for that request, and "Lihat
yang dikirim" shows the exact payload. From there, privacy depends on your AI provider (some free
plans use data to improve their products). Moku has no server and sends nothing to us; use Ollama or
LM Studio to keep everything on this PC.

- **What is sent:** the selected text, date and time, time zone, app language and the city *name*;
  opt in (Settings → Memori & Privasi) to to-do titles, today's calendar titles and times, timer state
  and a weather summary; and the facts you told it to remember (unless you pause the memory). For a
  provider on `localhost`, "Kirim semua konteks" turns all of these on at once.
- **Never sent:** clipboard history, notification bodies, OCR or screen content, OTPs, coding-agent
  sessions, a precise location. OTPs, 16-digit numbers (cards, NIK) and token-like strings are
  replaced by a marker such as `[disamarkan: kode OTP]` before sending (switch off in Memori & Privasi).
- **API key:** Windows Credential Manager (`Moku/ai/<host>`, one key per provider host, so switching
  provider never sends the old key to the new host), never `config.toml` or a log.
- **Memory:** `ai-memory.bin`, JSON encrypted with AES-256-GCM (Windows CNG); the key is random per
  save and wrapped by DPAPI for your Windows user. A file that cannot be opened (damaged, changed,
  another Windows user) is reported and left untouched, never overwritten. Settings → Memori &
  Privasi lists, edits and deletes facts, pauses the memory, exports JSON to Downloads and deletes
  everything. The chat history is off by default (7 or 30 days when on) and is never sent to a
  provider.
- **Selected text:** read with UI Automation. If that finds nothing and the fallback is on, Moku
  presses Ctrl+C in the program in front, only right after your own hotkey press, reads what it
  copied and puts your clipboard back exactly as it was (the fallback is skipped when the clipboard
  cannot be saved without loss, and Moku never presses keys in a terminal, where Ctrl+C means
  "stop"). Password fields are never read. **Moku's own clipboard history ignores the change**: a hold
  on the history (`features/clipboard/hold.rs`) covers the copy and the restore, so the selection is not
  recorded (also not when it is a one-time code). **What remains** is any *other* clipboard watcher:
  Windows' clipboard history (Win+V), cloud clipboard and third-party managers see the selection copy for
  the fraction of a second before your clipboard is back, and Moku can not stop them. Turn the fallback
  off in Settings → Tanya AI if that matters (UI Automation alone never touches the clipboard). "Ganti
  teks" (paste the answer over the selection) borrows the clipboard the same way, with the same hold. Asking UI Automation for the selection can make a
  Chromium-based browser switch its accessibility support on for the rest of that browser session
  (as screen readers do), which can make its pages a little slower.
- **Network:** only the base URL you configured (HTTPS, or HTTP for localhost). Redirects are not
  followed, so the key never travels to another host. Nothing you typed or an AI answered is logged.

## mokuctl

```
mokuctl ping
mokuctl notify --title "Build selesai" --body "205 tests passed" --icon check
mokuctl progress --title "Mengunduh" --value 42% --id dl      # --value 1 finishes it
mokuctl ask --title "Run tests?" --command "cargo test" --timeout 60
                       # prints approve | deny | <option> | timeout; exit 0 / 1 / 2
mokuctl todo add "Kirim laporan"
mokuctl session --id s1 --project toko-online --state working   # agent status pill
mokuctl claude-hook    # Claude Code hook: reads the hook JSON on stdin, prints nothing
mokuctl pipe-name      # the pipe this mokuctl talks to
mokuctl debug open-tab <id> | close | state | click <action> | screenshot-ready
```

`mokuctl debug` is for tests: a release island refuses every debug op except `state` unless
it was started with `MOKU_DEBUG=1` in its environment, so other programs cannot click
buttons in the island (for example approve a permission card).

Exit code 3 means Moku is not running (or IPC is turned off in settings). `mokuctl --help`
lists every option. The pipe is per user (`\\.\pipe\moku-<your SID>`, opened by you only), and
`mokuctl` refuses to talk to a pipe whose server is not run by you. `mokuctl pipe-name` prints
the name; `MOKU_DATA_DIR` (a test island with its own data folder) and `MOKU_PIPE` change it.

### Claude Code

To let Claude Code ask for permission through the island and show its live status, add hooks to
`~/.claude/settings.json` (all of it, with every event, is in
[docs/claude-code-hook.md](docs/claude-code-hook.md)). The status events call `mokuctl.exe claude-hook`
**directly** (native, about 10 ms, it never slows Claude Code down); only the permission card
(`PreToolUse`) goes through the PowerShell script, which needs `mokuctl.exe` too:

```json
{
  "hooks": {
    "PreToolUse": [
      { "matcher": "Bash|PowerShell|Edit|MultiEdit|Write",
        "hooks": [ { "type": "command", "command": "powershell.exe", "timeout": 180,
                     "args": ["-NoProfile", "-ExecutionPolicy", "Bypass",
                              "-File", "C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\tools\\claude-hook.ps1"] } ] }
    ],
    "UserPromptSubmit": [
      { "hooks": [ { "type": "command", "command": "C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\mokuctl.exe", "args": ["claude-hook"], "timeout": 5 } ] }
    ],
    "Notification": [
      { "hooks": [ { "type": "command", "command": "C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\mokuctl.exe", "args": ["claude-hook"], "timeout": 5 } ] }
    ],
    "Stop": [
      { "hooks": [ { "type": "command", "command": "C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\mokuctl.exe", "args": ["claude-hook"], "timeout": 5 } ] }
    ]
  }
}
```

(Add `SessionStart`, `PostToolUse` (`"async": true`), `StopFailure`, `SubagentStop` and `SessionEnd` the same way
for the full set of pills.)

### Codex CLI, Gemini CLI and "always allow"

`mokuctl agent-hook --agent claude|codex|gemini` is the hook command of every supported coding agent: a pill
per session for all three, and a permission card for Claude Code and OpenAI Codex CLI (Gemini CLI's hooks can
deny but not approve, so it is status only). With **Aturan "Selalu izinkan perintah ini"** switched on in
Settings → Notifikasi (it is **off by default**), a shell-command card offers a button that saves the **exact
command** shown on the card. A later ask for that same command, character for character (spaces between words
do not count), from the same agent, is approved without a card. Nothing else is: `npm test` does not allow
`npm test --script-shell=./x.cmd`, `git grep -O calc` or `npm test && rm -rf /`, and no rule can start with a
shell, an interpreter, a downloader, or a program that runs other programs (`find`, `make`, `sed`, `awk`, `tar`,
`env`, `xargs`, `start`, `explorer`, `forfiles`, `rundll32`, `mshta`, `msiexec` ...). A rule that was added to
`agent-rules.json` without Moku writing it (a program or an agent edited the file) does not act: Moku shows
"Aturan izin diubah di luar Moku" and waits until you confirm it in Settings. Moku never writes to an agent's
own settings. Configs, doc links and limits: [docs/agents.md](docs/agents.md).

### Windows notifications on the island

Settings → Notifikasi → "Notifikasi Windows di island" (off by default) shows the notifications of the apps you
list (WhatsApp, Telegram, Discord, Outlook, Teams, Signal by default) as a pill, with **Salin kode** for a
one-time code. Nothing is saved or logged, the text is hidden during meetings, and Moku never opens Windows'
permission prompt. Whether Windows delivers notifications to an unpackaged exe is documented, with the fallback
(a sparse identity package built by `release.ps1 -SparsePackage`), in [docs/notifications.md](docs/notifications.md).

## Hotkeys

| Hotkey | Default | Setting |
|---|---|---|
| Open / close the panel | `Alt+M` | `[general] hotkey_toggle` |
| Quit | `Ctrl+Alt+Shift+Q` | `[general] hotkey_quit` |
| Launcher | `Alt+Space` | `[launcher] hotkey` |
| Screen text (OCR) | `Ctrl+Alt+T` | `[ocr] hotkey` |
| Tanya AI | `Ctrl+Alt+A` | `[ai] hotkey` |

Every one is a global `RegisterHotKey` shortcut, and every failure looks the same: when another program already
owns the combination, the island shows "Pintasan Ctrl+Alt+T dipakai aplikasi lain" once and the shortcut
stays off until you change it in Settings → Pintasan. **One combination belongs to one feature**: the settings
app refuses a key that another feature uses ("Sudah dipakai peluncur"), and if a hand-edited `config.toml`
gives two features the same key, the later one is skipped with a pill ("... sudah dipakai ...: OCR
dilewati"). A switched-off feature holds no key. The defaults were chosen to be free on most PCs
(`Win+Shift+T` belongs to Windows, `Ctrl+Alt+Space` is taken by several apps); they were checked with
`RegisterHotKey` on the machine this was built on, and `Ctrl+Shift+Space` is the alternative for Tanya AI when
`Ctrl+Alt+A` is taken. The hotkeys are let go while the island is paused from the tray or a full-screen app is in
front, **not** while an app of `hide_apps` is: pressing the launcher, OCR or Tanya AI hotkey there opens its
sheet and shows the island for as long as it is open.

## Configuration and data

- Config: `%APPDATA%\Moku\config.toml`. The island watches it and applies changes at once
  (hot reload); the settings app saves there too. Invalid values fall back to their
  defaults and are logged; a half-written file is ignored until it is valid again.
- Data folder `%APPDATA%\Moku\` (every file below is in `moku_core::paths::DATA_FILES`, which "Hapus
  semua data" and the uninstaller use as the list of what is Moku's):
  - `config.toml`: all settings.
  - `todo.json` (the to-dos; `todo.corrupt.json` is a damaged one kept aside), `water.json` (drink counter).
  - `launcher.json` (what you picked in the launcher), `launcher-index.bin` (the helper's app list with icons).
  - `capture.json` and `capture-thumb.png` (the last screenshot), `update.json` (when the updater last looked),
    `meeting-mute.flag` (the microphone repair note).
  - `agent-rules.json` (the "Selalu izinkan perintah ini" rules of the coding-agent hub) and `agent-rules.trusted.json`
    (Moku's own copy of what it wrote, so a rule added to the first file by another program waits for your confirmation).
  - `holiday-eve.txt` (the holiday the "Besok libur" pill was last shown for), `bmkg-seen.json` (the
    earthquake times and warning ids already shown, so a restart does not repeat them).
  - `ai-memory.bin` (the Tanya AI memory, encrypted) and `ai-memory.lock` (its write lock).
  - `phone-debug.txt` (only in a `MOKU_DEBUG=1` run: the address of the open phone-transfer session, for
    `tools\phone-test-client.ps1`; received files go to `Downloads\Moku`, never here).
  - Folders: `clipboard\` (history and images), `shelf\` (the index), `logs\moku.log` (rotated at 1 MB).
  - `.moku-data`, an empty marker that says "this is a Moku data folder". Nothing is written next to the exe.
- `MOKU_DATA_DIR` points both apps at another folder (tests, portable use). It also gives that
  island its own pipe (see "mokuctl"), so a scratch island never talks to your real one.
- Settings → "Hapus semua data" first tells a running island to drop the history, shelf, to-do
  and notification data it holds in memory (IPC `ClearData`), then deletes Moku's own files in the
  data folder (the list above, **not** whatever else happens to be in that folder) except `config.toml` (and the microphone-restore marker `meeting-mute.flag`).
  With IPC turned off the island cannot be told: restart it after wiping.
- Network, Tanya AI: only the base URL you set in Settings → Tanya AI (an OpenAI-compatible
  `<base>/chat/completions`, and `<base>/models` for the "Tes koneksi" button), only while Tanya AI
  is on and only when you ask. HTTPS, or HTTP for localhost; redirects are not followed; the API key
  is read from Windows Credential Manager and sent only in that request's `Authorization` header.
  No telemetry.
- Network: Open-Meteo (weather, geocoding) and LRCLIB (lyrics), only while those
  features are on, and GitHub for updates: at most once a day
  `GET https://api.github.com/repos/<repo>/releases/latest`, plus the installer download from
  `github.com` (redirected to `objects.githubusercontent.com` /
  `release-assets.githubusercontent.com`) when you press "Perbarui": the installer, its `.sha256`
  and its `.sig` signature. The updater talks to no other host, only over HTTPS, and only while
  `update.check` is on (Settings → Tentang) and the build has a real repository
  (`moku_core::release::REPO`) and the signing key's public half
  (`moku_core::release::UPDATE_PUBLIC_KEY`) set. No telemetry, no accounts. The launcher's helper
  process makes no network calls.
- Network, Hari ini (v0.3). Each request goes out only while its switch is on (Settings → Fitur), over
  HTTPS, to the one host named here; no account, no key, no cookie, and nothing about you in it except what
  is in the address (the city's coordinates for the air quality):
  - `https://calendar.google.com/calendar/ical/id.indonesian%23holiday%40group.v.calendar.google.com/public/basic.ics`:
    the public Google calendar of Indonesian holidays, when the island starts and every 12 hours, while
    `today.holidays` is on (on by default only when the Windows language is Indonesian).
  - `https://data.bmkg.go.id/DataMKG/TEWS/autogempa.json` and `.../gempadirasakan.json`: BMKG earthquakes,
    every 2 minutes while `bmkg.quake` is on (on by default only for Indonesian Windows; two requests per
    poll, far under BMKG's 60 per minute).
  - `https://www.bmkg.go.id/alerts/nowcast/id`: BMKG weather warnings (RSS), every 10 minutes while
    `bmkg.nowcast` is on (off by default).
  - `https://api.frankfurter.dev/v1/latest?base=USD&symbols=IDR`: the exchange rate of your pair, every 6 hours
    while `glance.rate` is on (off by default).
  - `https://air-quality-api.open-meteo.com/v1/air-quality?latitude=…&longitude=…&current=us_aqi,pm2_5`: the
    air quality at the coordinates of the Hari ini city, every hour while `glance.aqi` is on (off by default).
  - The city is turned into coordinates once with Open-Meteo's geocoding (the same call the weather makes),
    also when only the quake distance or the air quality needs it.
  Nothing is logged but error text.
- Network, calendar: the iCal (`.ics`) addresses you paste in Settings → Fitur → Kalender,
  and nothing else, every 15 minutes, only while the calendar is on (it is off by default and
  needs an address). Only `https:` / `webcal:` addresses and local files on a drive letter are
  accepted, never plain `http:` and never a network (`\\server\share`) path, which would make Windows
  offer your credentials to that host. The address is a secret (anyone with it can read your calendar): it is stored
  as you typed it in `config.toml`, shown masked in Settings, and never logged; event titles
  are never logged either.
- OCR (`Ctrl+Alt+T`; settings → Pintasan):
  everything happens on this PC and no host is contacted. The monitor under the cursor is frozen
  **in memory** for the moment you drag a region, only that region goes to Windows' own
  `Windows.Media.Ocr` engine, and the picture is freed as soon as the overlay closes. Nothing of the
  screen is saved, the recognised text is never logged or stored (the card keeps it in memory for a few
  minutes). It needs an OCR language of Windows (Settings → Time & language → Language & region →
  a language → Language options → Optical character recognition); Moku never installs one and
  shows how to add one when none is found. Indonesian (`id-ID`) is preferred, then English
  (`en-US`), then your profile languages; the `[ocr] language` setting overrides that.
- Smart clipboard actions: the detectors are local and pure (no host is contacted). "Buka" and
  "WhatsApp" hand a `https://` address (`https://wa.me/62…` for a phone number) to your browser only when you
  press them. A one-time code is recognised **before** the clipboard history writes anything: **any text that
  is only digits, 4 to 8 of them, from any app** (so a PIN or a short number is hidden too: a code that leaked
  is worse than a number that is only shown as dots), a message of up to 1 KB such as "Kode OTP Anda 482913", or
  a code written in two groups (`123-456`) copied from Phone Link, WhatsApp, Telegram or Signal. It is never
  saved to a file or to the index, its row shows dots, it is copied back without Windows' own history, and it is
  dropped after 60 s (`[clipboard] otp_guard`, on by default).
- Network, phone transfer (v0.3): while you have a "Kirim ke HP" / "Terima dari HP" transfer open
  (and only then), Moku **listens** with a temporary HTTP server on your Wi-Fi / LAN address, on
  a random port, for at most 10 minutes (see "Kirim ke HP"). It makes no outgoing connection for it.
  Plain HTTP, one device, one-time 128-bit token, your approval first. With Settings → Fitur →
  "Kirim ke HP" off, or no transfer open, nothing listens. Windows asks about its Firewall the first
  time; choose Private networks.
- Downloads: the Downloads folder is watched for file names only (`.crdownload`, `.part`,
  `.partial` and their renames). No file is ever opened or read, no name is logged, and
  nothing leaves the machine.
- Developer switch for the downloads watcher: with `MOKU_DEBUG=1`, `MOKU_DOWNLOADS_DIR` points
  it at a scratch folder instead of your Downloads folder (ignored without `MOKU_DEBUG=1`).
  The calendar can read a local file (`file:///C:/path/cal.ics`) instead of a web address.
- Developer switch: `MOKU_EXPORT_DIR` makes "Ekspor JSON" (Settings → Memori & Privasi) write there
  instead of Downloads, and not open Explorer; tests use it.
- Developer switches: `MOKU_LAUNCHER_SAMPLE=1` (with `MOKU_DEBUG=1`) makes the launcher's helper list
  invented apps instead of your Start Menu (screenshots); `MOKU_DEBUG=1` enables `mokuctl debug` (except `state`, which always
  works) and the sample/debug actions used by tests (`mokuctl debug click media.sample` …); `MOKU_AUTOSTART_KEY` makes the settings app use a
  test registry key instead of the real `Run` key. `MOKU_INSTALL_ROOT` and `MOKU_REG_ROOT` move
  everything `moku-setup` writes (program folder and shortcut under the given folder; uninstall
  entry and Run value under `HKCU\<MOKU_REG_ROOT>`); its window then shows "MODE UJI".

## Known limitations

- No real backdrop blur on the development machine (the host backdrop brush renders
  black there); the glass is a tint by default. `moku.exe --blur` keeps the blur path.
- Idle memory is above the 35 MB working-set target. Release build, island visible, 30 s idle
  (`prototype\measure.ps1`): about **40 MB working set / 14 MB private** with music turned off
  (the same as v0.1's final build) and about **51 MB / 19 MB** while a media session (GSMTC) is
  active. Most of it is shared system DLL pages (WinRT media, WinHTTP/TLS, Direct2D/WARP). Moku
  frees what it can (drawing caches after the panel closes, the Kontrol tab's WMI/radio objects,
  the idle WinHTTP session) but never trims its working set artificially: the launcher used to
  call `EmptyWorkingSet` after indexing, which only hid the number, and no longer does.
- The launcher's app list and icons come from a helper process (see "Launcher"), so indexing a real
  Start Menu (209 apps, about 5 s, 2 MB cache file) adds no shell libraries to the island (checked
  by listing its loaded DLLs before and after: the only new one was `textinputframework.dll`, the
  IME / text-input framework). Using the launcher still costs memory in the island, measured right
  after closing it: about +3 MB working set with a media session (+5 MB with music off, where
  private memory is +3.5 MB), from the cache buffer, the sheet's drawing caches and the icons. The
  parsed list and the icons are dropped two minutes after the launcher closes; private memory is
  then back to its idle level (15 MB with music on), the working set stays about 3 MB above idle
  (pages of Direct2D's caches the OS has not reclaimed).
- The Caps Lock pill was not verified with a real key press, and file drag-and-drop and
  screen capture were not driven live in automated tests (they were tested through their
  debug paths and unit tests).
- GPU overlay tools that inject into every Direct3D process (NVIDIA's performance overlay,
  RivaTuner/MSI Afterburner) can draw their FPS counter on top of the island, because the
  island renders with a Direct3D (WARP) device. Exclude `moku.exe` in the overlay tool
  (for RTSS: add an application profile for `moku.exe` with "Application detection level"
  set to None).
- `--gpu` (hardware rendering) does not recover from a lost GPU device.

## Belum ada / Not yet

What is really missing (everything else in this file exists):

- **Moku Link**, the phone app and every phone feature (calls, SMS, phone battery, remote): needs
  the Android app and a protocol. The "HP" tab is a placeholder.
- **Windows toast mirroring is unproven on a plain install**: it is written, but Windows may require a package
  identity (MSIX or a sparse package) before it hands notifications to `moku.exe`; the island says "butuh
  versi Store/MSIX" when it does (see [docs/notifications.md](docs/notifications.md)).
- **OTP codes from the phone** (codes in mirrored Windows notifications are handled).
- **Screen recording**: needs Windows.Graphics.Capture plus an encoder. Screenshots exist.
- **Real backdrop blur**: the glass is a tint (see "Known limitations").
- **A light island theme.**
- **Code signing** of the installer and the exes (see "SmartScreen" above). The updater's own
  ECDSA signature check is a different thing: it proves a release came from the maintainer's key,
  Windows does not know that key.

## Releasing

- The version is set once, in `app/Cargo.toml` (`[workspace.package] version`); every exe and
  the installer take it from there. The licence is GPL-3.0 (`license = "GPL-3.0-only"`, `LICENSE`
  and `NOTICE` at the repo root, both shipped in the installer and the portable zip).
- **Two constants make a build releasable**, both in `app/moku-core/src/release.rs`:
  `REPO` (the GitHub `owner/name`, `dikaproject/MokuApp`; a fork sets its own) and
  `UPDATE_PUBLIC_KEY` (the maintainer's key; a fork makes its own). While either is the
  placeholder (`"OWNER/moku"` / `None`) the updater stays off (logged once at
  start, and Settings → Tentang says "Belum aktif di build ini"), and `app\tools\release.ps1`
  **refuses to package a release** (`-Dev` builds an unofficial package anyway).
- **The signing key**: `powershell -File app\tools\release-key.ps1 new -Out <a path outside the repo>`
  makes an ECDSA P-256 key pair (.NET ECDsaCng), saves the private key there (never inside the
  repository; back it up: without it nobody can sign an update) and prints the
  `UPDATE_PUBLIC_KEY` line to paste into `release.rs`. `release.ps1` signs with the key file named by
  `$env:MOKU_SIGNING_KEY` and writes `moku-setup.exe.sig`; `release-key.ps1 sign <file>` and `verify <file>`
  do the same by hand. For GitHub Actions put the key file's text in the repository secret
  `MOKU_SIGNING_KEY`.
- To publish: add a `## [x.y.z]` section to `CHANGELOG.md`, bump the version, commit, then push
  a tag `vx.y.z`. `.github/workflows/release.yml` runs the tests, builds, packages with
  `app/tools/release.ps1` and creates the GitHub Release with `moku-setup.exe`,
  `moku-setup.exe.sha256`, `moku-setup.exe.sig`, the portable zip and the changelog section as notes.
  A tag with a pre-release part (`v0.3.0-rc.1`) becomes a pre-release, which the updater ignores.
- Locally: `powershell -File app\tools\release.ps1` writes the same files to
  `<target dir>\dist` (it installs and uploads nothing).
- CI: `.github/workflows/ci.yml` runs `cargo test --workspace --locked` and a release build with
  `RUSTFLAGS=-D warnings` on every pull request and every push to `main` (Windows), with read-only
  permissions; the actions are pinned to commit SHAs and the Rust version to `rust-toolchain.toml`.
  Only the release job of `release.yml` may write (to create the release).

## License

Moku is free software under the [GNU GPL version 3](../LICENSE) (c) 2026 Moku contributors, with
the additional terms in [`NOTICE`](../NOTICE) (GPL section 7): keep the credit to the official
repository, give a modified version its own name and icon, and the Moku logo is not licensed for
modified versions. The installer and the portable zip include `LICENSE` and `NOTICE` next to the
exes. Slint (settings window, installer) is used under the GPL-3.0 too.
