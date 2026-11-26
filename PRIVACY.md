# Privacy policy

Last updated: 2026-11-25. Applies to Moku, the program at <https://github.com/dikaproject/MokuApp>.

## In short

- Moku has **no account, no server of its own and no telemetry**. Nothing is ever sent to the Moku
  project or to its maintainer.
- Everything Moku keeps lives on your PC, in your own Windows account.
- Moku contacts a network service only for a feature that is switched on, and only the services
  listed below. Each feature has its own switch, and a switched-off feature makes no request.
- The services see what any web request shows (your IP address, and the program name `Moku/<version>`
  as the User-Agent), plus the data named in the table. Moku sends no account, key or identifier of
  its own (the one exception is your own Tanya AI key, sent to your own provider).

Moku is open source: every statement here can be checked in the code, and the longer technical
version is in [`app/README.md`](app/README.md) ("Network" and "Configuration and data").

## What Moku contacts, when, and what is sent

| Feature (where to switch it off) | Service | When | What is sent |
|---|---|---|---|
| Weather in "Hari ini" (Settings → Fitur; **on by default**) | Open-Meteo: `geocoding-api.open-meteo.com`, `api.open-meteo.com` | about every 30 minutes while on | The city name you set (once per city name, to find its coordinates, unless you saved coordinates in `config.toml`), then those coordinates rounded to 4 decimals. |
| Air quality chip (Settings → Fitur; off by default) | Open-Meteo: `air-quality-api.open-meteo.com` | hourly while on | The coordinates of your Hari ini city. |
| Lyrics for the song that is playing (Settings → Fitur → "Musik: lirik berjalan"; **on by default**) | LRCLIB: `lrclib.net` | when a track starts, while lyrics are on | The track's artist, title, album and length (at most four requests per song, the later ones only when the first did not find lyrics; the title and artist may be sent without tags such as "(Official Video)" or "- Topic"). |
| Update check (Settings → Tentang → "Periksa pembaruan otomatis"; **on by default**) | GitHub: `api.github.com`; the installer comes from `github.com` (redirected to `objects.githubusercontent.com` or `release-assets.githubusercontent.com`) | at most once a day, plus once each time you press "Periksa sekarang" (Settings → Tentang, also while the automatic check is off; the same request); the download only when you press "Perbarui" or "Perbarui sekarang" | A request for the latest release of this repository. Nothing about you. The downloaded installer is run only if its SHA-256 and its signature check out. |
| Indonesian public holidays (Settings → Fitur; on by default only when Windows is set to Indonesian) | Google's public holiday calendar on `calendar.google.com` | at start and every 12 hours while on | Nothing except the request for that public calendar file. |
| BMKG earthquake alerts (Settings → Fitur; on by default only when Windows is set to Indonesian) | `data.bmkg.go.id` | every 2 minutes while on | Nothing except the request for the public feeds. |
| BMKG weather warnings (Settings → Fitur; off by default) | `www.bmkg.go.id` | every 10 minutes while on | Nothing except the request for the public feed. |
| Exchange rate chip (Settings → Fitur; off by default) | Frankfurter: `api.frankfurter.dev` | every 6 hours while on | The two currency codes of your pair, for example `USD` and `IDR`. |
| Your calendars (Settings → Fitur → Kalender; off by default) | The `.ics` addresses **you** pasted | every 15 minutes while on | A plain request for that address. The address may contain a secret key; Moku stores it as you typed it, shows it masked and never logs it. |
| Tanya AI (Settings → Tanya AI; **off until you turn it on**) | The AI provider **you** chose (OpenAI, OpenRouter, Groq, Gemini, DeepSeek, Ollama, LM Studio or another OpenAI-compatible address) | only when you ask something (and when you press "Tes koneksi") | See below. |

Moku contacts no other host. The city name also goes to Open-Meteo's geocoding when BMKG alerts or
the air quality chip need your place and no coordinates are saved. Requests go over HTTPS (Tanya AI
may use plain HTTP for `localhost` only). The update download may only follow redirects to the
GitHub hosts above, and Tanya AI never follows a redirect, so your key cannot reach another host.
Moku never logs an address or a header.

Features that make **no** network request: the launcher, screen OCR, the clipboard history and its
smart actions, the Shelf and its file converter, timers, prayer times and the Hijri date
(calculated on your PC), battery, Bluetooth, volume and the other pills, and the coding-agent
status and permission cards. The downloads pill only watches file names in your Downloads folder
and never opens a file.

### Tanya AI

Tanya AI is off until you turn it on and name a provider. When you ask something, the selected
text and the context you allow are sent to **that provider**, and every card shows the host
("Dikirim ke ...") and what goes along. "Lihat yang dikirim" shows the exact payload, and the chips
on the card remove items for that request.

- **Sent:** the selected text, the date and time, your time zone, the app language and the city
  name; the facts you told it to remember (unless you pause the memory); and, only if you opt in
  (Settings → Memori & Privasi), your to-do titles, today's calendar titles and times, the timer
  state and a weather summary.
- **Never sent:** clipboard history, notification text, screen text from OCR, one-time codes,
  coding-agent sessions, a precise location. One-time codes, 16-digit numbers and token-like
  strings are replaced by a marker before sending (switchable in Memori & Privasi).
- **Your API key** is kept in Windows Credential Manager, one key per provider host, and is sent
  only to that host. It is never written to `config.toml` or a log.
- After the text reaches your provider, **that provider's privacy policy applies**: some free plans
  use data to improve their products. To keep everything on your PC, use Ollama or LM Studio.

### Phone transfer

"Kirim ke HP" / "Terima dari HP" does not contact the internet. While a transfer is open (at most
10 minutes) Moku listens on your Wi-Fi / LAN address, accepts one device that has the one-time link,
and moves nothing before you press "Terima". Details are in [`app/README.md`](app/README.md).

## What stays on your PC

- **Data folder `%APPDATA%\Moku`:** settings (`config.toml`), to-dos, the clipboard history (text and
  images; password managers and one-time codes are skipped or kept in memory only), the Shelf
  index, a small log (`logs\moku.log`, rotated at 1 MB, with no addresses, headers, typed text or
  AI answers) and a few small state files. `app/README.md` lists every file.
- **Tanya AI memory** (`ai-memory.bin`): the facts you asked it to remember, and the chat history if
  you turned it on (off by default, 7 or 30 days). It is encrypted with AES-256-GCM, with a random
  key per save wrapped for your Windows account (DPAPI). It is never sent anywhere except as the
  facts that go with a request you make. Settings → Memori & Privasi lists, edits, exports, pauses
  and deletes it.
- **Your files:** screenshots go to `Pictures\Moku`, files received from your phone to
  `Downloads\Moku`.
- Moku writes nothing next to its program files.

## Your choices

- Turn any feature off in Settings → Fitur, the update check in Settings → Tentang, and Tanya AI in
  Settings → Tanya AI. A switched-off feature makes no request and holds no data.
- Settings → "Hapus semua data" deletes Moku's own files in the data folder (it keeps
  `config.toml`); Memori & Privasi deletes the Tanya AI memory.
- Block Moku in Windows Firewall if you want it fully offline: everything except the features in
  the table keeps working.

## Installing and uninstalling

`moku-setup.exe` installs for your account only (no administrator rights) into
`%LOCALAPPDATA%\Programs\Moku`, adds a Start Menu shortcut and an entry in Apps & Features, and, only
if you choose it, a Windows start-up entry. Uninstall from Apps & Features, or with
`moku-setup.exe --uninstall`: it closes Moku, removes the program files, the shortcut, the start-up
entry and the uninstall entry, and **asks whether to delete your data** in `%APPDATA%\Moku` (a
silent uninstall keeps it unless you pass `/deletedata`). Deleting data removes Moku's own files
only, never other things that happen to be in that folder.

## Code signing

Once SignPath Foundation approves the project, release binaries are signed with its certificate;
see the [code signing policy](CODE_SIGNING_POLICY.md) for the status. Signing happens on SignPath's
side and does not change what the program does: Moku sends nothing to SignPath.

## Changes and questions

A change to this policy is made in this file, so it shows in the repository history, and is listed
in [`CHANGELOG.md`](CHANGELOG.md). Questions and reports: <https://github.com/dikaproject/MokuApp/issues>.
