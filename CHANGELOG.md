# Changelog

All notable changes to Moku. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and versions follow [Semantic Versioning](https://semver.org/). The release workflow publishes
the section of the tagged version as the GitHub release notes.

## [0.3.0] - 2026-11-25

First public release, at https://github.com/dikaproject/MokuApp. Moku is free software under the
GNU GPL version 3 with the additional terms in `NOTICE` (keep the credit to the official repository;
a modified version uses its own name and icon). It includes everything from 0.1.0 and 0.2.0 plus
seven feature tracks: the polite island (O), screen OCR and smart clipboard actions (L), Tanya AI (M),
Ibadah and Ramadan (N), Hari ini (P), the coding-agent hub and Windows notifications (Q), and phone
transfer with the Shelf converter (R).

### Added

- **Polite island** (track O): Settings → Island chooses the monitor (`primary`, `cursor`, or one by
  device name), the anchor (left, centre, right) with a nudge, and when the island gets out of the way:
  it fades or slides aside when the pointer heads for the area under it, hides until the pointer rests
  on the top edge, hides while an app you listed is in front, and ignores the music of apps you listed.
  "Gerak" has a reduce-motion level (no overshoot, shorter, fades only) besides Normal and Mati. The
  pointer is read only while one of these is on; nothing is recorded or sent.
- **Ibadah and Ramadan** (track N, Settings → Fitur): the Hijri date (also in the Hari ini header, with a
  -2..+2 day correction), prayer times by Kemenag RI or Malaysia JAKIM angles, the imsak and buka puasa
  countdown ring in the island (prominent in the last 30 minutes), "Selamat berbuka" at Maghrib, opt-in
  puasa sunnah reminders (Senin-Kamis, Ayyamul Bidh, Arafah, Tasu'a, Asyura) and an optional adzan sound
  at prayer time (a generated chime or your own file; media Moku paused is the only media it resumes;
  no recording ships with Moku). Off by default.
- **Screen OCR** (track L): press `Ctrl+Alt+T` (or type `ocr` in the launcher, or use the
  "Teks (OCR)" tile in Tangkap), drag a region of the screen, and its text is copied; a pill says
  "Teks disalin · 42 kata" with "Lihat", a card of selectable lines. It uses Windows' own OCR
  engine, so nothing leaves the PC and nothing of the screen is saved. With no OCR language
  installed the pill explains how to add one.
- **Smart clipboard actions** (track L): a colour offers the other notations (HEX / RGB / HSL), a link
  offers a QR code and "Buka", a sum shows its result, minified JSON gets "Rapikan", an Indonesian mobile
  number gets "WhatsApp". A strong match also shows a short pill (off in Settings → Pintasan).
- **One-time codes (OTP)** (track L) are recognised before anything is written: never saved to disk,
  shown as dots, copied back without Windows' clipboard history, and dropped from the history after 60 s.
- **Tanya AI** (track M; Ask AI, off until you turn it on in Settings → Tanya AI): select text in any
  program and press `Ctrl+Alt+A` (or type `? question` in the launcher) to translate (Indonesian and
  English into each other), summarize, explain, fix the writing of, or ask about it, with the answer
  streaming into the island; then copy it, paste it over the selection ("Ganti teks", only on your
  click) or have Moku remember a fact. Works with any OpenAI-compatible provider (presets for OpenAI,
  OpenRouter, Groq, Gemini, DeepSeek, Ollama and LM Studio). Every card shows which host it is sent to
  and what goes along (removable chips, "Lihat yang dikirim"); OTPs, 16-digit numbers and tokens are
  hidden first; clipboard history, notification bodies, screen text, OTPs, agent sessions and precise
  location are never sent. The API key lives in Windows Credential Manager (one per provider host).
  The memory (facts, and an optional 7- or 30-day history) is a local file encrypted with AES-256-GCM,
  its key wrapped by DPAPI; Settings → Memori & Privasi lists, edits, deletes, exports and pauses it.
  After a question is sent, privacy depends on the AI provider you chose; use Ollama or LM Studio to
  keep everything on the PC. The settings app has two new pages (Tanya AI, Memori & Privasi) and the
  first-run guide a sixth step about it.
- **To-do with a time** (track P, Hari ini): a "+ Tambah to-do" field in the panel, edit (pencil or
  double-click) and delete (×). A due time in the words, in Indonesian or English (`rapat jam 3`, `kirim
  laporan besok 09:00`, `nanti malam`, `senin depan`, `30 menit lagi`, `tomorrow 9am`, `in 2 hours`),
  raises a reminder pill at that time with **Selesai** and **Tunda 10 m**. `mokuctl todo add` and the
  launcher's `todo` read the same words. The ambiguous `jam 3` means the next 03:00 or 15:00 (rules in
  the README). Tanya AI can include the open to-dos with their due times, when you allow it.
- **Libur nasional and cuti bersama** (track P) from the public Google holiday calendar: a badge in Hari
  ini on a holiday, the next one listed, and a "Besok libur" pill from 17:00 the evening before. On by
  default when the Windows language is Indonesian.
- **BMKG alerts** (track P): earthquakes (every 2 minutes) raise a pill when they are within your radius
  (300 km of the Hari ini city by default) or M 6 or more anywhere, with magnitude, place, distance, time
  and BMKG's tsunami line in red; each quake is shown once. Weather warnings (nowcast RSS, every 10
  minutes, off by default) for your province. Credit "Sumber: BMKG".
- **Glance chips** (track P) in Hari ini: an exchange rate (Frankfurter, default USD → IDR, every 6
  hours) and the air quality (Open-Meteo US AQI, hourly) with a pill when it goes above 150. Both off by
  default. Settings → Fitur has the switches; `README.md` "Network" lists every host.
- **Coding-agent hub** (track Q): `mokuctl agent-hook --agent claude|codex|gemini` shows a pill per
  session for Claude Code, OpenAI Codex CLI and Gemini CLI, and a permission card for Claude Code and
  Codex (Gemini CLI's hooks can deny but not approve, so it is status only). With the opt-in switch
  "Aturan Selalu izinkan perintah ini" (off by default), a shell-command card offers **Selalu izinkan
  perintah ini**: the exact command becomes a rule (`agent-rules.json`), listed and removable in Settings →
  Notifikasi, that approves that same command, and only that one, from the same agent without a card. Moku
  never writes to an agent's own settings (`docs/agents.md`).
- **Windows notifications on the island** (track Q, Settings → Notifikasi, off by default): the
  notifications of the apps you list (WhatsApp, Telegram, Discord, Outlook, Teams, Signal by default) as a
  pill, with **Salin kode** for a one-time code. Nothing is saved or logged, the text is hidden during
  meetings, and Moku never opens Windows' permission prompt (`docs/notifications.md`).
- **Kirim ke HP** (track R): send a Shelf file to your phone, or receive files and text from it, over the
  same Wi-Fi by scanning a QR code; no app on the phone. A temporary HTTP server listens on your real
  Wi-Fi / LAN address (virtual adapters and VPNs are skipped) only while a transfer is open, with a
  one-time 128-bit token, one device only, and nothing moves before you press "Terima" on the island.
  Received files are streamed to `Downloads\Moku` (2 GB each, never replacing a file) and land on the
  Shelf; a text from the phone shows as a pill with "Salin". Plain HTTP on your network: use it on
  trusted Wi-Fi only. Windows Firewall asks the first time; choose Private networks. Settings →
  Fitur turns it off. `tools\phone-test-client.ps1` is a loopback-only test client.
- **Ubah ke…** (track R) on Shelf files: images (WIC) to JPG / PNG / WebP, audio and video
  (MediaTranscoder) to MP3 / M4A / MP4, with a progress ring on the tile. The result is
  `name (Moku).ext` next to the original and never replaces a file. HEIC / AVIF / WebP decoding needs
  the free Store extensions; the island says so when they are missing.
- A generic QR widget in the UI kit (`ui::qr`, tracks L and R): byte mode, error correction M, versions
  1-10, written from the specification and checked against 17 matrices from an independent encoder and
  a decoder that reads every version and mask back. One encoder serves the clipboard's QR card
  (`qr(text)`) and the phone-transfer card (`qr_image`).

### Changed

- **Licence:** GPL-3.0 with `NOTICE` (section 7 terms) instead of MIT, before anything was
  published. Settings → Tentang names the licence and the official repository.
- **Updater:** on. `REPO` is `dikaproject/MokuApp` and `UPDATE_PUBLIC_KEY` holds the release key.
- The settings app now has 12 pages (Tanya AI and Memori & Privasi were added) and a 6-step first-run
  guide; the launcher's `>` list of settings pages follows.
- The launcher gained `ocr` (as the whole query) and `? question` (Tanya AI). An empty `>` lists all 12 settings
  pages and scrolls (arrow keys, wheel).
- `features/claude` became `features/agents` (the coding-agent hub).
- `net.rs` has a generic `request()` (any method, headers, a body, the response piece by piece with a
  cancel token, redirects off, loopback never through the proxy) besides `get` / `get_text`. It is also
  compiled into the settings app for the "Tes koneksi" button.
- New files in the data folder: `agent-rules.json`, `holiday-eve.txt`, `bmkg-seen.json`, `ai-memory.bin`
  with its `.lock`, and `phone-debug.txt` (debug runs only). "Hapus semua data" removes them with the
  rest; the README lists every one.

### Fixed

- Hover to open: the input window started at the pill's top, 8 DIP below the screen's edge, so a pointer pushed against
  the top edge (the natural way to reach a top island) was over the app behind and nothing opened until the
  pointer came down a little. The input window now reaches up to the monitor's top edge over the width of the pill
  (and of the panel and the split bubble), nothing wider, on every monitor, scale and DPI; a click up there counts
  as a click on the pill. It is also put back where it belongs if the system moved it.
- `net.rs`: a response body is read as it arrives, and an empty URL no longer reaches
  `WinHttpCrackUrl`.
- **Review round of 2026-11-25** (all still unreleased):
  - Hijri: Kemenag's published month starts (1 Muharram 1447 to 1 Muharram 1449 H, with their sources) are
    data, so 1 Ramadan 1447 = 19 Feb 2026, 1 Syawal 1447 = 21 Mar 2026, 1 Muharram 1448 = 16 Jun 2026 and
    25 Nov 2026 = 15 Jumadil Akhir 1448 need no correction; the tabular calendar is used outside that range, and
    the -2..+2 day correction still applies on top.
  - A tsunami pill is its own activity: a later earthquake can no longer replace it, and it stays visible over
    a full-screen app and over `hide_apps` until closed. "tidak berpotensi tsunami" is no warning.
  - Polite island: the pointer timer backs off while the island is away too and is not armed while it is
    hidden (re-armed on show); hide-until-edge keeps any pill with a button and any notification-priority
    activity; follow-the-cursor never moves an open panel, sheet or focused field; a move with the left button
    down (a drag) never moves the island aside; the edge reveal band is 6 px when a monitor is above.
  - Hotkeys: OCR is `Ctrl+Alt+T` and Tanya AI `Ctrl+Alt+A` (the old defaults are taken on many PCs); every hotkey
    (panel, quit, launcher, OCR, AI) shows the same "Pintasan ... dipakai aplikasi lain" pill when it cannot
    be registered; one combination belongs to one feature (the settings refuse it, the island skips the later one
    with a pill); `hide_apps` no longer takes the launcher, OCR and Tanya AI hotkeys away, and a sheet they open
    shows the island while it is open.
  - Codes: any text that is only 4-8 digits is held in memory like an OTP whatever app copied it, OTP messages up
    to 1 KB are handled, and a code in a mirrored notification is masked with dots in the pill and the panel.
  - Hari ini: the two columns are measured against the 360 DIP body and give way (the "next day off" line, then
    agenda rows with "+N lainnya", then to-do rows) instead of being clipped; the to-do header reads "To-do hari ini".
  - To-do times: `malam ini jam 12`, `besok jam 12 malam` and `24:00` are midnight of the next day when a day is
    named, and `WIB` / `WITA` / `WIT` are converted to the PC's time zone.
  - Tanya AI's Ctrl+C fallback and "Ganti teks" put a hold on Moku's own clipboard history (README names the
    remaining Win+V / cloud-clipboard race).
  - Launcher: `ocr reader` is a search, a disabled OCR leaves no dead row.
  - OCR overlay: a half-made bitmap pair no longer leaks, the foreground request is retried, and Esc closes the
    overlay even when it never got the keyboard.
  - Adzan resumes only the media sessions Moku paused (not a sibling session the user had paused); the
    reminder and holiday timers are armed only for something that is due; a failed geocoding is remembered.
  - `*.localhost` is no longer "local" (Tanya AI needs HTTPS for it); `net::get` has a total timeout of three
    times its read timeout.
  - Tests that used the real Credential Manager or DPAPI are `#[ignore]`d (`MOKU_TEST_SYSTEM=1 cargo test -- --ignored`
    runs them), the autostart test no longer races on a shared registry key, and the tests that slept to wait for
    another thread poll for the event with a deadline.
  - The settings sidebar lists Tanya AI and Memori & Privasi before Tentang and fits 12 entries at the smallest
    window; the privacy note is shown in one language; the HP placeholder tab is gone from the island;
    redaction markers read `[disamarkan: kode OTP]`; sample data no longer uses a personal name.
  - Version 0.3.0.

### Security

- Tanya AI sends only to the one base URL you configured (HTTPS, or HTTP for localhost), never follows a
  redirect with your key, and keeps the key in Windows Credential Manager; its memory file is encrypted.
- One-time codes are never written to disk; the phone-transfer server is per transfer, token-gated, one
  device, and approved by you.
- **"Always allow" rules** match exactly the command you approved (no other arguments: `npm test` never allows
  `npm test --script-shell=./x.cmd`, `git grep -O calc` or `find src -delete`), are off by default, refuse
  programs that run other programs (`find`, `make`, `sed`, `awk`, `tar`, `env`, `xargs`, `start`, `explorer`,
  `forfiles`, `rundll32`, `mshta`, `msiexec` ...), and a rule that appeared in `agent-rules.json` without Moku
  writing it waits for your confirmation in Settings ("Aturan izin diubah di luar Moku").
- **Phone transfer** refuses a network Windows calls Public ("Jaringan publik: kirim ke HP dimatikan demi
  keamanan") unless you press "Tetap lanjutkan" for that session, and received files carry the
  Mark-of-the-Web (`Zone.Identifier`, `ZoneId=3`).

## [0.2.0] - not published

Built and tested but never published on its own; it ships as part of 0.3.0.

### Added

- **Installer** `moku-setup.exe`: a per-user install to `%LOCALAPPDATA%\Programs\Moku` with no
  admin rights, a Start Menu shortcut, an entry in Apps & Features, an optional "start with
  Windows" switch, and a silent mode (`/S`). Uninstall from Apps & Features or with
  `moku-setup.exe --uninstall`; it asks before deleting your data in `%APPDATA%\Moku`.
- **Updates**: once a day Moku asks GitHub Releases for a newer version and offers it in the
  island ("Moku x.y.z tersedia" → "Perbarui"). The installer is downloaded over HTTPS from
  GitHub only, and run only if its SHA-256 matches the release's `.sha256` file **and** the
  release's `moku-setup.exe.sig` is a valid ECDSA P-256 signature of that digest by the key built
  into Moku (checked with Windows CNG). Turn it off in Settings → Tentang. The updater stays off
  in a build that has no release repository or no signing key set.
- **App icon and version info**: the "m on a horizon line" icon (16 to 256 px) and a VERSIONINFO
  block are embedded in `moku.exe`, `moku-settings.exe`, `mokuctl.exe` and `moku-setup.exe`; the
  windows, the taskbar and the tray use them.
- **Claude Code live status**: a pill per active Claude Code session (working with a timer,
  needs input, done), driven by hooks. `mokuctl claude-hook` is a native hook command (about 10 ms,
  prints nothing, never slows Claude Code down); the PowerShell script is only needed for the
  permission cards. `mokuctl session` for other agents. Claude Code's English notification text is
  translated in the Indonesian UI.
- **Power and audio**: a charging animation when the charger is plugged in, Bluetooth headset
  battery, and a per-app volume mixer in the Kontrol tab.
- **Kontrol tiles**: three and three, as in the mockup, with a Jangan ganggu tile; the Bluetooth
  tile is tinted when on or connected.
- **Calendar**: meeting heads-up from iCal (ICS) links with a "Gabung" button for Meet, Zoom and
  Teams links, and today's agenda in the Hari ini tab.
- **Downloads**: browser download progress from the Downloads folder (name over size and speed),
  then "Buka", "Folder" and a **Shelf button** that parks the finished file on the Shelf.
- **Launcher** (`Alt+Space`): apps, a calculator, `todo`, `timer` and settings pages, with fuzzy
  matching ranked by use, and **recent Shelf files** as results (programs and scripts are only
  shown in Explorer, never started). The app list and icons come from a short-lived helper
  process (`moku.exe --launcher-index`), so the island never loads the shell's heavy libraries
  for it.
- Release tooling: GitHub Actions CI (tests and a release build on pull requests and pushes to
  `main`), a tag-driven release workflow, `app/tools/release.ps1` for local packaging and
  `app/tools/release-key.ps1` for the signing key.

### Security

- The pipe between `mokuctl` / the settings app and the island is per user
  (`\\.\pipe\moku-<your SID>`, or derived from `MOKU_DATA_DIR` for a test island), named by one
  function. Clients check that the program serving the pipe runs as the same user before they send
  anything (`GetNamedPipeServerProcessId` + token SIDs), so a squatted pipe name cannot receive
  permission requests. Tests and developer scripts use their own pipes and can never reach the
  real island.
- Updates are signed (see above); the checksum alone only proves the download is intact.
- The launcher never starts a program or script found in the Shelf (shown in Explorer instead),
  and a file is never the first row of an empty search. The list of risky extensions is shared with
  the downloads pill and now covers msc, chm, appinstaller, settingcontent-ms, msp, wsc, sct, py, pyw,
  diagcab, iso, img, vhd, vhdx, library-ms, search-ms and website shortcuts as well.
- `moku-setup --deletedata` (and "Hapus semua data" in the settings) delete only Moku's own files and
  folders, never everything in the data folder; the data folder carries a `.moku-data` marker and the
  uninstaller refuses one without it.
- The installer only replaces an existing `Programs\Moku` folder that is Moku's (it has `moku.exe`, or
  our uninstall entry names it, or it is empty) and leaves another running setup's staging folders
  alone; the uninstaller's delayed cleanup runs `%SystemRoot%\System32\cmd.exe` with the paths in
  environment variables (a `%` or `&` in a path cannot change the command).
- AltGr (Ctrl+Alt) types characters in the launcher instead of acting as Alt shortcuts.
- The launcher gives `Alt+Space` back while the island is hidden, and lets only the program it started
  take the foreground (no more "any process").
- The calendar refuses network (UNC) paths as sources, and "Gabung" accepts `HTTPS://` links.

### Changed

- The version lives in one place, `app/Cargo.toml` (`[workspace.package] version`).
- The Rust version is pinned (`rust-toolchain.toml`); CI pins its actions to commit SHAs, builds with
  `--locked` and `-D warnings`, and has read-only permissions except in the release job.
- Mixer sliders are thin with an accent fill; the finished-download pill is capped in width; the
  launcher shows its key hints as key caps and hides its hint while an IME composes; the tab bar
  spaces its tabs evenly for any number of enabled tabs.
- The settings' "Hapus" buttons are clearly red; the update switch is greyed out ("Belum aktif di
  build ini") while the updater is not in the build.

## [0.1.0] - 2026-11-24

First working version (not published).

### Added

- The island: a glass pill at the top of the screen that opens into a panel with tabs on hover
  or click, with the split bubble for a second activity, and the onboarding in the settings app.
- Tabs: Musik (media session, synced lyrics from LRCLIB, live bars), Hari ini (week strip,
  to-do list, weather from Open-Meteo), Sehat (water and eye-break reminders, prayer times),
  Kontrol (Wi-Fi, Bluetooth, dark mode, keep awake, meeting mode, volume and brightness, output
  device), Shelf (park dropped files), Timer (presets, focus with breaks, stopwatch), Clipboard
  (text and image history), Tangkap (screenshots), Monitor (CPU, GPU, RAM, disk, network),
  Notifikasi (`mokuctl` notifications, do-not-disturb, AI permission cards).
- System pills: battery, Caps Lock, internet lost / back, camera and microphone in use, meeting
  detected with a mute button, progress bars.
- `moku-settings.exe` (Slint) with every setting and hot reload, and `mokuctl.exe` (notify,
  progress, ask, todo, debug) over a local named pipe.
- Indonesian UI by default, English available.
