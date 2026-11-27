# Moku v0.1 — architecture and feature API

This is the contract between the island core (Task 1) and the feature tracks (Tasks 2–7).
If you build a feature, this file plus your feature's section of the plan
(`docs/superpowers/plans/2026-11-24-moku-desktop-v0.1.md`) is all you need. Everything named
here exists and compiles today; the stubs in `moku/src/features/` already use it.

## 1. Workspace

```
app/
  Cargo.toml          workspace + shared release profile (lto, panic = "abort", strip)
  moku-core/          lib: config schema, paths, hotkey/colour parsing, IPC protocol + client (plain Rust;
                      Windows APIs: winsid.rs (user SIDs for the per-user pipe), secrets.rs
                      (Credential Manager), aimem/crypt.rs (CNG AES-GCM + DPAPI); ai.rs holds the
                      provider presets, URL rules and privacy note both apps share)
  moku/               moku.exe, the island (Windows subsystem)
  moku-settings/      moku-settings.exe (Slint 1.19, software renderer) — Task 7
  mokuctl/            mokuctl.exe, CLI over the named pipe
  moku-setup/         moku-setup.exe, per-user installer / uninstaller (v0.2, track I): the
                      release exes are embedded by build.rs from MOKU_SETUP_PAYLOAD (empty
                      otherwise); tools/release.ps1 packages it
  tools/              PowerShell smoke tests (hover, tray re-add, pipe robustness)
  docs/ARCHITECTURE.md
```

Build and test (PowerShell 5.1; keep build output on drive E):

```powershell
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
$env:CARGO_TARGET_DIR = "<repo>\.build\<your-worktree-name>"
cd <worktree>\app
cargo build --release          # → moku.exe, moku-settings.exe, mokuctl.exe
cargo test --workspace
```

`moku.exe` flags (testing only): `--state idle|hover|music|music-panel|lyrics|lyrics-panel|panel` (fixed state for
screenshots, `--tab <id>` with `panel`; `lyrics` / `lyrics-panel` run a fake player with timed lyrics through the
real media and lyrics code, from `MOKU_DEMO_POS_MS` (default 1000), to look at and measure the word highlight; `MOKU_DEMO_LYRICS=auto|chosen|none` shows the caption and buttons for Moku's own pick, a user's pick with an offset, or everything rejected),
`--demo` (loops the prototype states), `--gpu`, `--blur`.
In `--state`/`--demo` modes there is no tray, pipe, hit window, services or `on_start` — use a
normal run plus `mokuctl debug …` to test a feature.

### `moku/src` map

| File | What |
|---|---|
| `main.rs` | args, single instance (mutex `Local\MokuIsland.SingleInstance`; a 2nd launch opens the panel of the 1st), DPI, DispatcherQueue (UI thread = COM STA), `OleInitialize`, message loop |
| `app.rs` | `App` (all feature-facing methods), `Ui::post`, `MeetingState`, `Redraw`, timers, view state machine, flush/diff, IPC + debug dispatch |
| `registry.rs` | `Registry`, `TabSpec`, `order`, `ActionEvent`/`ActionKind`, `IpcKind`, `AppEvent`, `ServiceSpec` |
| `activity.rs` | `Activity`, `priority`, `OSD_SECS` …, the pure priority `Stack` |
| `services.rs` | `ServiceCtx`, service start/stop |
| `ui/` | widget tree (`mod.rs`), layout (`layout.rs`), composite widgets (`widgets.rs`), glyphs (`icons.rs`), text editing model (`text_edit.rs`), QR encoder + `qr(text)` widget (`qr.rs`), the word-highlight line `karaoke(..)`: nodes, word positions, key-frame plan (`karaoke.rs`, pure + tested) |
| `scene/` | composition engine (springs, morph, sprites, split bubble), D2D/DirectWrite (`gfx.rs`), `reduce.rs` (reduce motion: pure mapping, tested), `karaoke.rs` (the compositor side of the word highlight) |
| `features/lyrics/` | synced lyrics: `lrc.rs` (LRC parser, enhanced word stamps kept), `words.rs` (when each word is sung: real stamps or an estimate), `title.rs` (cleaning messy titles / artists for the lookup), `lrclib.rs` (the client: the request plan, `lookup` / `alternatives` over an injectable fetcher), `pick.rs` (judging and ranking LRCLIB entries), `view.rs` (the candidates, "Bukan lagu ini", the offset), `choices.rs` (`lyrics-choices.json`, the track key), `links.rs` (Genius / Musixmatch search addresses), `mod.rs` (service, cache, `LyricsState::handle`, the sung line) |
| `shell/` | visual / hit / tray windows, input routing, hotkeys, fullscreen hide, tray menu; keyboard focus, IME, clipboard text (`keyboard.rs`); which monitor and where on it (`monitors.rs`, pure + tested); the input window's rectangle and region, which reaches up to the monitor's top edge over each shape's width (`hit.rs`, pure + tested) |
| `ipc_server.rs` | named-pipe server, `Reply` |
| `hotkeys.rs` | the five global hotkeys in one place (v0.3 fix): `register` refuses a combination another Moku feature holds (`Outcome::Clash`) and reports one another program holds (`Taken`), `announce` raises the one pill "Pintasan ... dipakai aplikasi lain" once per feature and combination; `moku_core::hotkey::{Slot, clash}` is the same rule for the settings app |
| `safety.rs` | `is_risky(name)` (programs and scripts are never opened from the island, shared by the downloads pill and the launcher), `reveal(path)` (Explorer with the file selected) |
| `helper/` | the helper-process modes of `moku.exe` (`--launcher-index`, `--icons`): `pack.rs` (the cache file format, tested), `icon.rs` (shell icons), `sample.rs` (invented apps for screenshots) — see "Helper process" below |
| `config_watch.rs` | hot reload of `config.toml` |
| `imaging.rs` | WIC decode / PNG encode → `ui::Image` |
| `net.rs` | HTTP(S) over WinHTTP: `get` / `get_text`, and `request` (any method, headers, a body, the answer piece by piece, a `Cancel` token, redirects off, loopback never through the proxy). The updater has its own allowlisted fetch in `features/update/fetch.rs`; the settings app includes this file as is (`#[path]`) for the AI "Tes koneksi" |
| `persist.rs` | atomic file writes on one writer thread (`App::save_later` uses it) |
| `i18n.rs` | language switch, `tr`, date/time formatting |
| `log.rs` | `log_info!` / `log_warn!` / `log_error!` → `%APPDATA%\Moku\moku.log` |
| `demo.rs` | prototype "Midnight Drive" content for `--state` / `--demo` |
| `features/mod.rs` | `register_all`: one line per feature (+ core services; the HP placeholder tab is registered but not enabled, so it is not in the tab bar) |
| `features/placement/` | the polite island (v0.3, track O): move aside, hide until the top edge, per-app hide rules, follow the cursor; see 9b |
| `features/ocr/` | screen OCR (v0.3, track L): `mod.rs` (hotkey `Ctrl+Alt+T`, actions `ocr.*`, the pill and the text card), `overlay.rs` (our own full-screen window to drag a region), `region.rs` (pure rectangle maths), `engine.rs` (`Windows.Media.Ocr`, language choice), `sample.rs` (a synthetic picture for tests and screenshots) |
| `features/clipboard/` | clipboard history (`listener.rs`, `sys.rs`, `model.rs`, `store.rs`); v0.3 track L adds the smart actions (`actions/`: colour, URL, math, JSON, one-time code, phone number detectors; `smart.rs`: the action tag, the short pill, the QR card) and the in-memory one-time code |
| `features/ai/` | Tanya AI (v0.3 track M): `provider.rs` + `sse.rs` (OpenAI-compatible streaming), `context.rs` / `redact.rs` / `prompts.rs` (what is sent), `selection.rs` + `clip.rs` (selected text, Ctrl+C fallback, Ganti teks), `mod.rs` (the card, a core sheet; actions `ask.*`) |
| `features/phone_send/` | send / receive files with a phone over Wi-Fi by QR code (v0.3, track R; the Shelf converter is `features/shelf/convert.rs`); see 13b |
| `features/hijri.rs`, `features/ramadan.rs` | the tabular Hijri date and the puasa sunnah calendar (`hijri.rs`), the Ramadan pack built on `prayer.rs` (`ramadan.rs`: imsak / buka puasa countdown ring, "Selamat berbuka", sunnah reminders) (v0.3, track N) |
| `features/adzan/` | the optional adzan sound at prayer time (v0.3, track N): `chime.rs` (a generated chime, nothing is bundled), `player.rs` (`MediaPlayer` on its own thread), `media.rs` (pause the user's media first, resume only what Moku paused) |
| `features/agents/` | the coding-agent hub (v0.2 track G, generalised in v0.3 track Q): `mod.rs` (one pill per Claude Code / Codex CLI / Gemini CLI session), `model.rs` (the pure, agent-neutral session state machine); the permission cards and the "always allow" rules (`moku-core`'s `agent_rules.rs`, `ai_cards/always.rs`) belong to `features/ai_cards/` |
| `features/notify_mirror/` | Windows notifications on the island (v0.3, track Q): `listener.rs` (the `UserNotificationListener` service), `model.rs`, `otp.rs` (one-time code detection for "Salin kode"), `mod.rs` (the pill, the Notifikasi section) |
| `features/bmkg/` | BMKG earthquakes (`quake.rs`) and weather warnings (`nowcast.rs`) for Hari ini (v0.3, track P) |
| `features/glance/` | the exchange-rate (`rate.rs`) and air-quality (`aqi.rs`) chips of Hari ini (v0.3, track P) |
| `features/<name>.rs` | **your file** |

## 2. Rules for feature tracks

1. **Edit only your feature files**: `moku/src/features/<name>.rs` (you may turn it into
   `features/<name>/mod.rs` + submodules — delete `<name>.rs` then). `features/mod.rs` already
   calls your `register`. Track E also owns `mokuctl/src/*`; track F owns `moku-settings/**`.
2. `moku/Cargo.toml` already enables every `windows` feature the plan needs (media control,
   radios, Core Audio, WMI, registry, power, network list manager, IP helper, PDH, clipboard,
   OLE, WIC, GDI, shell, pipes, WinHTTP …). Only if something is truly missing, add the one
   feature line in its group and say so in your report. Do not add crates (no async runtime,
   no `ureq` — use `crate::net`; no `image` — use `crate::imaging`).
3. The config schema is final (`moku-core/src/config/*.rs`, all sections of the plan's table).
   Read it; don't edit it. If you truly need a new field, report it instead.
4. Never block the UI thread (no network, file scans, `.get()` on WinRT async ops, sleeps).
   Do that in a service and `Ui::post` the result.
5. Release builds use `panic = "abort"`: a panic kills the island. No `unwrap()` on anything
   that comes from the OS, the network, files or IPC.
6. Never log personal content (clipboard text, notification bodies, keys, window titles).
7. Tests go in your own file (`#[cfg(test)] mod tests`). `cargo test -p moku` runs them.

## 3. Threading model

* **UI thread**: the one that runs `main`'s message loop. It owns the compositor, all windows
  and `App`. Every `&mut App` / `&App` API below is **UI-thread only**. Feature callbacks
  (`TabSpec::build`, activity builders, `on_action`, `on_config`, `on_start`, `on_event`,
  `on_message`, IPC handlers, timer closures) are always called on the UI thread.
* **Any other thread** (services, WinRT event handlers, COM callbacks) reaches the app only via

  ```rust
  crate::app::Ui::post(|app: &mut App| { /* runs on the UI thread, in order */ });
  ```

  `Ui::post` never blocks and may be called from anywhere (the closure must be `Send`).
  `crate::app::current_config() -> Arc<Config>` reads the current config from any thread.
* The UI thread is a COM **STA** (and OLE is initialised). A service thread that uses COM
  must call `CoInitializeEx(None, COINIT_MULTITHREADED)` (or STA + `ctx.run_message_loop()`).
* **Synchronous cross-process COM** (WMI queries, `INetworkListManager` calls, …) pumps
  messages while it waits; on the UI thread that re-enters the island mid-handler. The core
  defers re-entrant work instead of dropping it, but keep such calls in a service anyway.
* Modal loops (`DoDragDrop`, `TrackPopupMenu`, dialogs) must not run while you hold
  `&mut App`: queue them with `app.run_outside(move || { … })`; inside, use `Ui::post` to come
  back. While a modal loop runs, posted jobs wait and run right after.

## 4. Registry (called once, before the island exists)

```rust
pub fn register(r: &mut Registry) {
    r.add_state(MyState::default());                // one value per type; app.state::<MyState>()
    r.add_tab(TabSpec { .. });                      // a tab of the panel (see §6)
    r.on_action("myfeat", on_action);               // every action id "myfeat.<name>[:arg]"
    r.on_config(on_config);                         // after start and after every config reload
    r.on_start(on_start);                           // once, island up (not in --state/--demo)
    r.on_exit(on_exit);                             // once on quit / logoff, before services stop
    r.on_event(on_event);                           // AppEvent: focus hold, meeting, accent, panel …
    r.on_message(WM_SETTINGCHANGE, on_msg);         // window messages to the main window
    r.add_service(ServiceSpec { name: "myfeat", enabled: |c| c.myfeat.enabled, run: service });
    r.add_ipc_handler(IpcKind::Todo, on_todo);      // Notify / Ask / Todo / Session
}
```

Handler types (plain `fn` pointers — keep data in your state, not in captures):

```rust
pub type ActionFn  = fn(&mut App, &ActionEvent);
pub type IpcFn     = fn(&mut App, moku_core::ipc::Request, Reply);
pub type AppFn     = fn(&mut App);
pub type EventFn   = fn(&mut App, &AppEvent);
pub type MessageFn = fn(&mut App, WPARAM, LPARAM);
```

Registering the same tab id, action prefix or IPC kind twice panics at startup (by design).

## 5. `App` — the feature-facing API (UI thread)

| Method | Notes |
|---|---|
| `config(&self) -> &Config` / `config_arc() -> Arc<Config>` | always valid (bad values were replaced by defaults) |
| `update_config(\|c: &mut Config\| …) -> io::Result<()>` | **the only way a feature changes a setting** (a panel toggle that is also a setting): re-reads `config.toml` (falls back to the in-memory copy if the file is broken right now), applies the change, saves atomically and applies it at once like a hot reload. Never call `Config::save` / `save_to` from a feature: that writes the island's copy over edits the settings app just saved |
| `state::<T>() -> &T` / `state_mut::<T>() -> &mut T` | panics if `T` was not `add_state`d |
| `request_redraw(Redraw::Pill \| Redraw::Tab("id") \| Redraw::All)` | cheap; rebuild + diff once per message batch; `Tab` is a no-op when that tab isn't visible |
| `set_activity(Activity)` / `clear_activity(id)` / `has_activity(id)` / `top_activity()` | §7 |
| `alert(id, priority, secs, \|app\| node)` | transient activity shorthand |
| `open_panel(Some("tab"))` / `open_panel(None)` / `close_panel()` / `is_panel_open()` | `Some` from a feature is **not** the user's choice (reopening later still goes to the tab they chose). `None` = reopen, see "Which tab a reopened panel shows" below |
| `choose_tab("tab")` | open a tab as the user's own choice (remembered) — for explicit "go to X" controls |
| `animations() -> Animations` | effective motion setting (Normal / Kurangi / Mati = `Full` / `Reduced` / `Off`): `appearance.animations`, or `Off` while Windows "Animation effects" is off (`scene::reduce::effective`). Skip waits for animations when `Off` |
| `current_tab() -> Option<&'static str>` / `is_tab_visible(id)` | |
| `hold_panel(bool)` | keep the panel open while the pointer is outside (counted) — e.g. OLE drag over the island |
| `after(Duration, FnOnce(&mut App)) -> TimerId` / `every(Duration, Fn(&mut App)) -> TimerId` / `cancel_timer(id)` | UI-thread timers (closures may capture) |
| `dispatch_action("other.name", ActionKind::Click)` | call into another feature by action id |
| `run_outside(FnOnce())` | run after the `&mut App` borrow ends (modal loops) |
| `focus_hold()` / `set_focus_hold(bool)` | focus mode holds notifications |
| `meeting() -> &MeetingState` / `set_meeting(MeetingState)` | meeting mode (owned by `meeting`) |
| `accent() -> Rgba` / `set_accent_override(Option<Rgba>)` / `base_accent()` | effective accent (album colour while music plays and `follow_album_color`) — **music UI only**; `base_accent()` = the configured accent (#0A84FF default) for every other "on" state (Kontrol tiles, chips, flashes) |
| `main_hwnd() -> HWND` | top-level, never-activated window: target for `RegisterPowerSettingNotification`, `AddClipboardFormatListener`, raw input, … |
| `hit_hwnd() -> Option<HWND>` | the input window over the island shape: `RegisterDragDrop` target (None in `--state`) |
| `scale() -> f32` | DIP → physical px (monitor DPI × `island.scale`) |
| `hide_island(bool)` | counted; hide while capturing the screen, wait ~50 ms before `BitBlt` |
| `force_visible(bool)` / `is_paused()` | counted; keep the island shown over a full-screen app (pending AI card). Never beats tray pause or `hide_island` |
| `save_later(key, fn(&App) -> Option<(PathBuf, Vec<u8>)>)` / `cancel_save(key)` | **how a feature saves its data file**: the snapshot runs ~500 ms after the last call (bursts cost one write) and the bytes are written on the writer thread with temp file + rename (`crate::persist`); pending saves are flushed on exit. Never write files on the UI thread |
| `restart_service(name)` | stop + start your service (e.g. its settings changed) |
| `set_app_hidden(bool)` | the foreground app is one the user listed (`island.hide_apps`): hides like a full-screen app (unless `force_visible`); owned by `placement` |
| `set_away(Away)` / `away()` | step the island out of the way: `Away::{Home, Fade, Slide{dx}, Hide}`. Compositor fade / shift of the whole island, input window hidden so clicks go through (9b); owned by `placement` |
| `home_rect()` / `monitor_rect()` / `monitors()` | the island's resting rectangle (shape + bubble) and its monitor, physical px on the virtual desktop; the connected monitors |
| `set_followed_monitor(Option<&str>)` | "follow the cursor": live on that monitor (device name), `None` = choose again from the config |
| `activity_news() -> u64` / `top_priority() -> u8` | counter of "news" (activities added, transients set again) and the priority of the pill's activity (0 = idle) |
| `open_sheet(id, build)` / `close_sheet()` / `sheet()` | a feature's own panel-sized surface without the tab bar (§9a) |
| `input(id)` / `input_mut(id)` | the core-owned `TextEdit` of text input `id` (§9a) |
| `focus_input(id) -> bool` / `blur_input()` / `focused_input()` | give an input the keyboard / give it back (§9a) |
| `set_clipboard_text(s) -> bool` | put text on the clipboard (owned by the island's window) |
| `allow_foreground_for(pid)` | let the process the user's pick started (and only that one, never "any process") take the foreground; call it before `blur_input` / closing the sheet |

Window messages: `r.on_message(msg, f)` gets messages sent to `main_hwnd()`. Timers you
create yourself with `SetTimer(main_hwnd, id ≥ 100000, …)` and hotkeys with
`RegisterHotKey(main_hwnd, id ≥ 100, …)` arrive as `WM_TIMER` / `WM_HOTKEY` there (ids < 100000 /
< 100 are the core's). Prefer `app.after` / `app.every`.

### Which tab a reopened panel shows

Hover, a click on the pill (main pill background, or a pill whose own click handler calls
`open_panel(None)`, like music), the hotkey and a second launch all *reopen*: the pure
`app::reopen_tab` decides (unit-tested in `app.rs`):

1. a pending AI ask (priority `AI_CARD`) always wins;
2. else the pill's activity if it became **news after the panel last closed**, ranks above
   `MUSIC` and has an enabled tab (a new notification, a timer that just finished). "News" =
   the activity was added, or a *transient* one was set again; updating a persistent activity
   in place (track change, timer tick, progress) is not news (`activity::Entry::changed`);
3. else the tab the user last **chose**: a tab-bar click or wheel (the wheel on the tab bar
   walks to the previous / next tab), a click on the split bubble (opens the bubble's tab),
   `island.open:<tab>`, `mokuctl debug open-tab`, `app.choose_tab`;
4. else the pill activity's tab, else the first enabled tab.

An open panel stays on its tab when something calls `open_panel(None)`. Disabled tabs are
skipped at every step.

### Cross-feature hooks (defined in the core, so no feature edits another's file)

| Hook | Owner (writes) | Readers | API |
|---|---|---|---|
| Focus holds notifications | `timer` (Task 4) — while focus runs and `timer.hold_notifications_in_focus` | `notify` (Task 6) | `app.set_focus_hold(bool)`, `app.focus_hold()`, event `AppEvent::FocusHoldChanged(bool)` (release held notifications on `false`) |
| Meeting mode | `meeting` (Task 3) | `control` toggle (Task 2), `notify` privacy (Task 6) | `app.meeting() -> &MeetingState { active, app_name, mic_in_use, camera_in_use, mic_muted }`, `app.set_meeting(..)`, event `AppEvent::MeetingChanged`; to toggle from elsewhere: `app.dispatch_action("meeting.toggle", ActionKind::Click)` (the stub in `meeting.rs` already flips `active`) |
| Accent colour | `media` (Task 2) from album art when `appearance.follow_album_color` | everyone drawing with the accent | `app.set_accent_override(Some(rgba))` / `None` when music stops; `app.accent()`; event `AppEvent::AccentChanged`. Tip: `image.average()` |
| Tick only while visible | any feature with a tab | — | `TabSpec { tick: Some((Duration::from_secs(1), on_tick)), .. }` — called once when the tab is shown, then every interval, stopped when hidden / panel closed |

Other events (`AppEvent`): `ConfigChanged`, `PanelOpened`, `PanelClosed`, `SheetOpened(id)`, `SheetClosed(id)`, `TabShown(id)`,
`TabHidden(id)`, `DisplayChanged` (monitors, DPI or the island's monitor / position changed), `ForegroundChanged`, `Paused(bool)`, `HotkeysChanged` (the global hotkeys were let go: paused or a full-screen app, **not** `hide_apps`; `app.hotkeys_released()`), `Visibility(bool)` (island hidden / shown again:
nothing is rasterised or presented while hidden — `flush` skips — so stop drawing timers; see
`app.is_hidden()`), `DataCleared` ("Hapus semua data" in the
settings app, IPC `ClearData`: drop all user data you hold in memory and `cancel_save` your
pending saves; the settings app deletes the files right after the island answered).

## 6. Tabs

```rust
r.add_tab(TabSpec {
    id: "today",                         // used in "tab.today" and `mokuctl debug open-tab today`
    order: order::TODAY,                 // tab-bar position (order::MEDIA … order::PHONE, steps of 10)
    icon: icons::CALENDAR,
    label: ("Hari ini", "Today"),        // shown on the active tab
    enabled: |c| c.today.enabled,        // hidden when false (re-evaluated on config reload)
    build: panel,                        // fn(&App) -> Node, the panel body
    tick: None,                          // Some((interval, fn(&mut App))) while visible
});
```

* The core draws the panel chrome and the tab bar (from all enabled tabs; clicking a tab
  sends action `tab.<id>`). Your `build` returns only the **body**: 610 DIP wide, height is
  measured (40–360 DIP; taller content is clipped). Panel width is 640, radius 38.
* If your body is a `col([...])` (without bg/border/action on the col itself), each child is
  its own sprite and they fade + slide in with a 40 ms stagger when the panel opens. Only
  children whose draw ops changed are re-rasterised on a redraw.
* Rebuilds happen when you call `app.request_redraw(Redraw::Tab("today"))` (or `Pill`/`All`).
* To add a tab the plan doesn't list (e.g. "Sehat & ibadah"), pick an unused `order` between
  the existing ones (e.g. 25).

## 7. Activities (what the pill shows)

```rust
use crate::activity::{Activity, priority, OSD_SECS, HEADS_UP_SECS, NOTIFICATION_SECS};

app.set_activity(
    Activity::persistent("media.music", priority::MUSIC, |app| music_pill(app))
        .tab("media")                         // background click on the pill opens this tab
        .bubble(|app| art_bubble(app)),       // ≈36×36 content when shown as the split bubble
);
app.alert("volume.osd", priority::OSD, OSD_SECS, |app| volume_pill(app)); // transient
app.clear_activity("media.music");
```

* Highest priority wins; ties → the most recently added. Re-setting an existing id updates it in place (keeps its age, restarts a transient's timer).
* Transients expire on their own; the pill falls back to what is underneath (or the idle clock).
* The **split bubble** shows the best *persistent* activity other than the one in the pill.
* Priorities (`activity::priority`): `AI_CARD` 100 · `DUE` 90 (timer done / reminder) ·
  `NOTIFICATION` 80 · `OSD` 70 (transient 1.5 s) · `WARNING` 65 (internet lost / battery low) ·
  `MEETING` 60 · `PROGRESS` 55 · `TIMER` 50 · `MUSIC` 40 · `HEADS_UP` 30 (transient 6 s) · idle 0.
* The builder runs on every flush while visible: keep it pure and cheap; read your state.
* Pill size = measured content (min height 36, max width 640). Use `ui::pill([...])` for the
  standard frame (36 tall, 14 side padding, 8 gaps). Big pills (AI card) are fine.
* A click on the pill that hits no action widget reopens the panel (§5 "Which tab a reopened
  panel shows": the activity's `tab` only when it is news, else the user's last tab); a click on
  the split bubble opens the bubble activity's `tab`.
  Actions inside pills work like in panels (buttons, scroll on the whole pill via
  `.action("media.pill")` on the root → `Scroll`, `DoubleClick`).
* Hover on the idle pill shows the date; hover on an activity pill grows the shape slightly.

## 8. Actions

Action ids are strings `"<feature>.<name>[:<arg>]"`, e.g. `"clipboard.copy:17"`. The part
before the first `.` routes to the handler registered with `r.on_action("clipboard", f)`.

```rust
fn on_action(app: &mut App, ev: &ActionEvent) {
    match (ev.name(), ev.kind) {
        ("copy", ActionKind::Click) => copy(app, ev.arg().and_then(|a| a.parse().ok())),
        ("volume", ActionKind::Slide { value, done }) => set_volume(app, value, done),
        ("pill", ActionKind::Scroll(notches)) => nudge_volume(app, notches),
        ("pill", ActionKind::DoubleClick) => toggle_play(app),
        ("file", ActionKind::DragStart) => app.run_outside(move || start_ole_drag()),
        _ => {}
    }
}
```

`ActionKind`: `Click`, `DoubleClick`, `Scroll(f32 notches, + = up)`, `Slide { value 0–1, done }`
(on `slider` widgets, while pressed; `done: true` on release **or** when the press is cancelled,
e.g. capture lost — then with the last value), `DragStart` (press-and-move > 4 DIP on a
`.draggable()` node; the press ends there, start your OLE drag via `app.run_outside`), and for a
focused text input (§9a): `Edited`, `Key(KeyPress)`, `Blur`.
A double-click delivers `Click`, `Click`, then `DoubleClick` (only if both clicks hit the same
action) — so `+1` buttons count twice and play/pause pills can use `DoubleClick`.
Scroll and clicks go to the **innermost** action widget under the pointer: an action on a pill's
root receives scroll only where no inner action widget is, and a pill with a root action no
longer opens the panel on click by itself (call `app.open_panel(Some("tab"))` on `Click`). Core prefixes: `tab.<id>`, `island.open[:tab]`, `island.close`,
`island.settings`. `mokuctl debug click <action>` delivers a `Click`.
Text inputs (`ui::text_input`) also report under their id: see §9a.

## 9. UI kit (`crate::ui::*`)

A `Node` tree, built fresh from state each time and diffed (`PartialEq`).

**Primitives**: `row([..])` (children centred vertically), `col([..])` (left-aligned),
`stack([..])` (overlay, centred), `text(s)` (13 Regular white, one line, ellipsis),
`caption(s)` (11.5, white 46 %), `label(s)` (10.5 SemiBold CAPS), `title(s)` (20 Bold),
`icon(glyph)` (14, see `ui::icons`), `image(&img, radius)` (+ `.size(w,h)`), `toggle(on)`,
`slider(v)` (thick 36-tall Control Center slider; `.slider_icon(icons::VOLUME)`),
`text_input(id, &TextEdit)` (one-line field with caret, selection and IME composition; see §9a),
`progress(v)` (3 px bar; `.thickness`, `.track`, `.color`), `ring(v)` (20 px; `.size`,
`.thickness`), `sparkline(values)` (`.range(lo,hi)`), `bars(playing)` (audio bars animated by
the compositor, zero CPU), `spinner()` (20 px indeterminate ring: a short arc turning on a faint
track, compositor-animated unless animations are off; for work with no known percentage, e.g.
downloads; put it first in a pill so its position stays put and the animation is not restarted
by a rebuild), `battery(v)` (gauge: outline, terminal, level bar; `.color(c)`; `.charging()` hands
the bar to the compositor, see below), `logo(size)`, `spacer()`, `gap_px(n)`, `empty()`.

**Modifiers** (chain on any node): `.w(px)` `.h(px)` `.size(w,h)` `.fill_w()` `.fill_h()`
`.max_w(px)` `.grow(weight)` `.pad(a)` `.pad_xy(x,y)` `.pad4(l,t,r,b)` `.gap(px)`
`.align(Align::Start|Center|End|Stretch)` `.justify(Justify::Start|Center|End|SpaceBetween)`
`.center()` `.bg(rgba)` `.radius(r)` `.capsule()` `.border(rgba)` `.dashed()` `.opacity(a)`
`.action(id)` `.draggable()` `.child(node)`; text/icon: `.font_size(px)` `.bold()` `.semibold()`
`.medium()` `.color(rgba)` `.secondary()` (72 %) `.tertiary()` (46 %) `.display()` `.mono()`
`.lines(n)` (wrap) `.text_center()` `.text_right()`; `.name(label)` = accessible name of an
icon-only control (carried into its hit region; `mokuctl debug state` lists them as `labels`).

**Keyed motion** (compositor animations, zero CPU per frame, nothing to drive from a timer):

```rust
tile.key(format!("shelf.f:{key}"))                         // stable identity within the pill / panel body
    .enter(Anim::pop().delay(40.0 * i as f32))             // new key: opacity 0→1, scale 0.6→1, 14 DIP up
    .exit(Anim::into_key("shelf.trash").delay(30.0 * i))   // gone key: fly into that keyed element
    .pulse(gen)                                            // bump `gen` → scale pop once (.wiggle(gen): rotate)
    .leaving()                                             // exit now, keep the layout space
```

* A keyed node is laid out normally but rasterised as **its own sprite** (`Laid::keyed`); its hit
  regions stay in the block, so clicks / drags work as before. Keys must be unique per body.
* Between two trees the scene matches keyed sprites by key: changed content is repainted in
  place; a new position **glides** there (per-axis springs, ≈ 320 ms, continues from wherever an
  interrupted move was); a new key plays `enter` (default: 180 ms fade); a key that is gone
  plays `exit` from the **previous** tree (default: 160 ms fade) and its sprite is removed when
  that is over (deadline checked by the island's GC timer: no orphaned layers). A changed
  `pulse` plays `Pulse::Pop` (≈ 360 ms) or `Pulse::Wiggle` (≈ 420 ms).
* `Anim`: `fade()`, `pop()` (280 ms, ease-out with a slight overshoot on scale),
  `into_key(target)` (exit only: 340 ms on a curve bowed upwards, scale → 0.2, fades over the
  second half; falls back to a fade when `target` is not in the new tree); `.ms(..)`, `.delay(..)`.
* Since an exit uses the spec of the tree that still had the node, a sequence that decides the
  exit at the last moment ("empty the shelf: everything flies into the bin") marks the nodes
  `.leaving()` with the exit it wants, waits `delay + ms`, then drops them from the state.
* When a panel opens, keyed sprites fade / slide in with the block they are in. A new pill /
  tab (different content key) cross-fades as a whole; keyed diffs only run within one key.
* `appearance.animations = "reduced"` ("Kurangi"): enter / exit become short fades, no moves, no pulses, and the
  shape's springs have no overshoot and settle within 160 ms (9b "Reduce motion").
  `"off"` ("Mati") or Windows "Animation effects" off: everything is instant (`app.animations()`).
* A top-level body `col` is split into one block per child: put the key on a child, not on
  that `col` (wrap a lone keyed node in `col([..])`).

**Charging battery** (`battery(v).size(54.0, 26.0).charging()`, `scene/charge.rs`): the outline, nub and
track are ordinary ops; the level bar, a glint that crosses it once, a soft radial glow behind the
gauge and a lightning bolt that pops in are compositor visuals (`Live::Charge`: key-frame animations
on shape geometry, offset, scale and opacity, started once and finite, so nothing runs afterwards).
`scene::charge::plan(fill, anim)` is the pure timing: `"reduced"` fades the bar in and keeps the
glow steady, `"off"` shows the end state at once. A layer's children, bottom to top, are: glows
(`under_root`), block sprites, live items (`live_root`), keyed sprites. `Op::Poly` is a filled polygon.

**Lyric matching and corrections** (`features/lyrics/`). A wrong song is worse than none, so a lookup keeps only entries that
*are* the song. `pick::judge` drops noise (an entry under 20 s, or whose lyrics run 20 s past its own length), then needs the
title (`TitleMatch`: equal after cleaning and normalising, or equal once a bracket / " - " part is dropped; a longer title that
merely contains it never matches, two different qualifiers are two versions), the artist (`ArtistMatch`: equal, one inside the
other word for word, the same words, or a shared artist of a duet; required in every request type when the track has one) and the
length (±3 s, ±5 s when artist and title are exact; without an artist the length has to vouch) for a **strict** hit. Looser hits
(the same artist or the same title, ±10 s, synced) are kept for the user. `pick::rank` orders them: strict first, then one score
(title 300/200, artist 200/140, length 300 minus 50 per second off, instrumental minus 150, +15 for a real album), the lowest LRCLIB
id breaking ties (so: synced, exact length, has an album, lowest id); copies of the same lyrics are listed once; six are kept.
`lrclib::lookup` follows the plan until a strict hit turns up (an exact `get` hit leaves the searches undone: `Lookup::searched`
is false, and `alternatives` runs them only when the user presses "Bukan lagu ini" again); with a remembered id it fetches
`/api/get/<id>` alone. `view::View` is the state of the current track (candidates, shown id, rejected ids, offset); its `Next` /
`restart` / `shift` / `add` are pure and tested, and `LyricsState::handle(Cmd, artist, title) -> Effects` is the pure core of the
buttons (`media.lyrics_next`, `lyrics_earlier`, `lyrics_later`, `lyrics_web:<site>`), which the thin `on_action` runs. The shown
lyrics are `Lyrics::shifted(offset)` of the entry, so line scheduling, `current`/`around` and the karaoke word spans all follow the
offset, and `line_id` includes the entry and the offset so a sweep restarts when either changes. `choices::Store` is
`lyrics-choices.json` (read lazily on the first lookup, written with `App::save_later` + `persist::write_atomic`; the key is an
FNV hash of the normalised artist, title and a 6 s length bucket, the neighbouring bucket is tried too; 500 entries, oldest
dropped; cleared on `AppEvent::DataCleared`). Tests run offline against answers recorded from LRCLIB in `lyrics/fixtures/` (lyric
words replaced by "la", ids, names, lengths and line times kept; `fixture_tests.rs`).

**Karaoke line** (`karaoke(KaraokeSpec { id, text, dim, words, anchor, rise })`, `ui/karaoke.rs` + `scene/karaoke.rs`; used by the
Musik panel's sung line and the lyric pill, setting `media.lyrics_word_highlight`): one line of text whose words light up in time
with the music. It lays out like `text(..)` and draws nothing into its block (it produces `Live::Karaoke`); the scene draws the line
**twice** into two sprites that are children of the sprite of the block the line is in (so they fade and slide in with the panel):
the dim base and, on top, the bright copy seen through a `CompositionMaskBrush` whose mask is a linear gradient (opaque, a soft
edge about 0.8 x the font size wide, transparent). One scalar `X` in a property set places the edge (the gradient's start and end
points are expressions of it) and one `ScalarKeyFrameAnimation` on `X` moves it from where the voice is to the end of the line.
Both layers are rasterised once when the line becomes the sung line; afterwards the compositor does all the work (no per-frame
redraw, no timer). What the node carries: `words` (media ms and byte range per word: real stamps, or the estimate of
`features/lyrics/words.rs`), `anchor` (the player's position as of an `Instant`, `playing`, `rate`: only the player's own reports
change it) and `id` (track + line index). The pure parts, all unit-tested: `segs_for` (word → x), `x_at` (the lit width over time: each
word eased half linear / half smoothstep, steady across pauses), `plan` (key frames from "now" to the end, sampled four times per
word; a paused line is just placed), `needs_restart` (see below). **Sync**: a tree rebuilt for any reason keeps a running sweep by
its `id`; it is restarted only after a pause, resume, seek, rate change or when it is more than 140 ms off the player's clock (the
animation is started 20 ms ahead because the compositor picks it up a frame later). A new `id` (the next line) lets the old line
fade up and out (150 ms) and the new one fade in rising `rise` DIP (190 / 260 ms, `appearance.animations` full); reduced = a
short fade, off = instant. With animations reduced or off (or when the mask cannot be built) the line is just drawn bright. The
sweep never marks the island as animating (`busy_until`). Right-to-left lines and lines with nothing to sing ("♪") are plain text.
Word times: `words::spans(line, next_start)` uses real enhanced-LRC stamps when the lyrics have them, else
`estimate`: the line is sung for `min(slot, clamp(letters x 80 ms x 1.75, 1.2 s, 8 s))` (slot = gap to the next line minus 150 ms; 5 s
for the last line), spread over the words by syllables / letters (CJK and Thai: per character) with short pauses after commas and
full stops. The estimate is approximate by nature.

**Layout**: rows/cols give children their measured or fixed size; `grow`/`Fill` children share
the rest; if fixed children overflow a row, auto-sized ones shrink (text ellipsises). Cross
axis: `Px`, `Fill` or parent `Align::Stretch` = full, otherwise measured + aligned.

**Composite widgets** (`ui/widgets.rs`): `pill([..])`, `button(label, action)`,
`button_primary(..)` (white, dark text), `button_accent(label, accent, action)`,
`capsule(glyph, label, action)` (chip like "♫ Lirik"), `circle_button(glyph, size, action)`,
`icon_button(glyph, action)` (28 px target), `card([..])` (white 8 %, radius 22, pad 14),
`tile(glyph, colour)` (28 px rounded colour square), `trash_button(action, name)` (30 px round
bin, Segoe Fluent / MDL2 `Delete`), `list_row(leading, title, subtitle, trailing)`,
`toggle_tile(glyph, title, status, on, accent, action)` (Kontrol tiles), `placeholder(..)`.

**Colours / type** (`ui::color`, `ui::typo`, from `pratinjau/00-Ringkasan.png`): `WHITE`,
`TEXT2` 72 %, `TEXT3` 46 %, `FILL` 8 %, `FILL2` 14 %, `FILL3` 16 %, `INK` (dark text on white),
`BLUE #0A84FF`, `GREEN #30D158`, `ORANGE #FF9F0A`, `RED #FF453A`, `YELLOW #FFD60A`,
`PURPLE #BF5AF2`, `TEAL #64D2FF`, `MINT #63E6BE`, `PINK #FF375F`; `Rgba::hex(0xRRGGBB, a)`.
Sizes: `PANEL_TITLE 20`, `CARD_TITLE 16`, `BODY 13`, `CAPTION 11.5`, `LABEL 10.5`,
`RADIUS_CARD 22`, `MIN_TARGET 28`. Use `app.base_accent()` for "active" things; `app.accent()` (album colour) only in music UI.

**QR codes** (`ui/qr.rs`, v0.3): `qr("https://example.com")` is an image node of about 176 DIP (white
modules on a quiet zone, rounded corners), for any feature. `qr_image(text, module_px) -> Option<Image>` is the same
encoder without the widget or the cache, for a card that lays the picture out itself (the phone-transfer session
card). The encoder is `QrCode::encode(bytes)`: byte mode, error correction M, versions 1-10 (213 bytes), tested
against an independent implementation. The rendered bitmap is cached by text (the last two), so a
builder can call `qr(..)` on every flush without forcing a redraw; call `ui::qr::clear_cache()` when
the code leaves the screen. Text over 213 bytes gives a caption instead (`QrCode::encode` is `None`).

**Images**: `ui::Image` = premultiplied BGRA (`Image::new(w, h, bytes)`), compared by identity:
keep it in your state and reuse it; a new `Image` forces a redraw. Helpers in
`crate::imaging` (any thread with COM): `decode(bytes, max_px)`, `from_bgra(w, h, stride,
pixels, premultiplied, max_px)`, `encode_png(&img)`, `save_png(&img, path)`, `fit(..)`.

## 9a. Sheets and text input (keyboard)

The island normally never takes focus: its windows are `WS_EX_NOACTIVATE` and a click answers
`MA_NOACTIVATE`. A feature that needs typing (the launcher) combines two independent pieces:

**Sheet** — `app.open_sheet("launcher", build)` shows `build(&App) -> Node` as a 640-wide glass
surface like the panel but without the tab bar (`Mode::Sheet`; body 610 wide, height measured up
to `SHEET_BODY_MAX_H` = 412; a top-level `col` staggers in like a tab body). It replaces an open
panel, stays open when the pointer leaves, is never opened by hover, and hides the split bubble.
It closes with `app.close_sheet()`, and also when anything opens the panel, on the panel hotkey,
on pause / full screen and `mokuctl debug close`; the owner then gets
`AppEvent::SheetClosed(id)` — release the keyboard there (`app.blur_input()`).

**Text input** — the editing state lives in the core: `app.input(id)` / `app.input_mut(id)` give
the `ui::text_edit::TextEdit` (text, caret, selection, IME composition; pure and unit-tested).
Draw it with the widget

```rust
text_input("launcher.q", app.input("launcher.q"))     // fills the width, action id = the input id
    .placeholder(tr("Cari…", "Search…")).font_size(15.0).accent(app.base_accent())
```

and give it the keyboard with `app.focus_input("launcher.q")`. The core then:

* removes `WS_EX_NOACTIVATE` from the hit window and calls `SetForegroundWindow` on it — allowed
  right after the user's own hotkey (`WM_HOTKEY` hands the foreground right to this process).
  `focus_input` returns whether that worked; if not (e.g. driven by `mokuctl`), a click on the
  island activates it (`MA_ACTIVATE` while an input has focus);
* edits on `WM_CHAR` (surrogate pairs joined) and `WM_KEYDOWN`: ←/→ (Ctrl = word, Shift =
  select), Home/End, Backspace/Delete (Ctrl = word), Ctrl+A, Ctrl+V (clipboard text, line breaks
  become spaces). Alt / Alt+key never open a menu or beep;
* IME: `WM_IME_COMPOSITION` puts the composition inline (underlined) and commits the result
  string; the default composition window is hidden (`WM_IME_SETCONTEXT`), and the candidate window
  is placed under the caret (`ImmSetCompositionWindow` / `ImmSetCandidateWindow`, re-placed when
  the caret moves);
* draws the caret as `Live::Caret`: a compositor sprite blinking at the Windows caret blink rate
  (`GetCaretBlinkTime`; steady when blinking or animations are off) — no CPU timer;
* sends the owner (the feature that owns the id's prefix) `ActionKind::Edited` when the text
  changed, `ActionKind::Key(KeyPress)` for keys an input does not use (Enter, Esc, ↑/↓, Tab,
  PgUp/PgDn, Alt+letter/digit, Ctrl+letter other than A/V), and `ActionKind::Blur` when the user
  went elsewhere (another window activated, a click outside) — the keyboard is already released
  then and nothing is given back.

`app.blur_input()` releases the keyboard and gives the foreground back to the window that had it
(only while the island still holds it). When something the user picked is started, call
`app.allow_foreground_for(pid)` with the id of THAT process while the island still holds the
foreground (the launcher starts it first, then allows it, then closes), so its window may come to
the front; there is no "allow any process" call.
Rules: never send input to other windows; only receive keys sent to our own window.

Test without a keyboard: `mokuctl debug type <text> [--compose]` and `mokuctl debug key
<down|enter|esc|ctrl+backspace|alt+1|…>` feed the focused input (MOKU_DEBUG only; an error when
no input has focus). `debug state` shows `sheet`, `input` (id, caret, selection, whether the window
really has the foreground; the text only with MOKU_DEBUG) and `flush_ms` (last rebuild + present).

## 9b. Placement: the polite island (v0.3, track O)

Where the island lives and when it gets out of the way. The generic parts are in the core
(`shell/monitors.rs`, `Shell`, `App`, `scene/reduce.rs`); the policies are the feature
`features/placement/`. Config: `[island]` (`moku-core/src/config/island.rs`).

**Monitor, anchor, offset** (`island.monitor`, `anchor`, `offset_x`; applied by `reposition`):

* `monitor` = `"primary"`, `"cursor"` (follow the pointer), `"all"` (**not implemented**: one island
  per monitor needs one scene and one window set per monitor; it behaves like `"primary"`) or a
  device name (`\\.\DISPLAY2`, case-insensitive; a monitor that is not connected falls back to the
  primary one). `shell::monitors::choose` is pure; `Shell::refresh_monitors` lists the monitors
  again on every display change and `Target::resolve` picks the island's. The scale is that
  monitor's DPI x `island.scale`, so moving between monitors of different DPI rebuilds the scene.
* Follow the cursor: `monitors::Follow` is a pure debounce: the pointer must stay on another monitor
  for 300 ms without a break, so crossing a monitor on the way elsewhere moves nothing.
* `anchor` (left / centre / right) + `offset_x` (DIP, positive = right) give the shift of the
  island's resting *centre* from the monitor's centre, clamped by `monitors::shift_px` so that an
  open panel (640 DIP + 12 DIP margin each side) always stays on the monitor. The island opens from
  its resting centre, so it cannot get closer to a side than half a panel; a monitor narrower than a
  panel keeps it centred. The visual window hangs from the top of its monitor (`monitors::visual_rect`).

**Per-app rules** (`moku_core::apps`, shared with the settings app): `island.hide_apps` (process
names, case-insensitive, `.exe` optional) hide the island while such an app is in front
(`ForegroundChanged` -> `App::set_app_hidden`, which hides like a full-screen app unless a feature
forces the island visible; a UWP window resolves to the app's own process, not
`ApplicationFrameHost.exe`; Moku's own windows keep the previous state).
`island.media_ignore_apps` are matched against the media session's `SourceAppUserModelId` (exe name,
a whole word of the id, or the start of a word of 4+ letters); the media service skips such sessions
when it chooses the one to show and chooses again when the list changes (`media::repick`).

**Away moves** (`island.move_aside` = off / fade / slide, `island.hide_until_edge`, `island.hide_when_idle`): all end in
`App::set_away(Away)`:

* `Island::set_away` animates the opacity and horizontal offset of the scene's root on the
  compositor (key frames from wherever they are, so an interrupted move continues). **Fade** = 30 %
  opacity in place; **Slide** = shifted by the island's width + 24 DIP (as far as the 784 DIP window
  has room) away from the pointer, at 55 %; **Hide** = invisible.
* The input window is *hidden* (`Shell::set_passthrough`) the moment the island leaves home, so
  clicks reach what is underneath whether or not the animation has finished; a hover in progress ends.
* The pointer is read by polling `GetCursorPos` (one `app.after` chain, only while a setting needs
  it: every 100 ms with move aside on and 200 ms when only follow the cursor / hide until the edge
  need it, every ~30 ms while the pointer is near the island or the island is away, every 400 ms
  while the pointer has rested 1.5 s far from everything; no hooks, nothing is sent to another
  window). The left button's state is read only to tell a click
  from a rest.
* **Move aside** (`placement/aside.rs`, pure): `classify(samples, island_rect, params)` looks at the
  pointer's velocity and direction over the last 140 ms. On the island: stay. Heading at it (the
  path, extended, meets the island grown by 14 DIP) from closer than 240 DIP and faster than
  140 DIP/s: if the path goes through its middle (*aimed*), a pointer that brakes or is under
  650 DIP/s wants the island (stay) and a fast one that keeps its speed is going *through* it (step
  aside, only when it would arrive within 120 ms, because homing pointers brake late); if the path
  only grazes it (along it, under it), step aside (within 400 ms). `Aside` adds the hysteresis: it
  comes back 400 ms after the pointer left the island grown by 60 DIP, or after the pointer rested
  on the island's own spot for 450 ms without a click (the user wanted it); a click while away means
  the target is underneath, so it stays away until the pointer leaves. Never while the panel / a sheet
  is open, a text input has focus or something urgent (priority >= `DUE`) is shown.
* **Hide until the top edge** (`placement/edge.rs`, pure): hidden until the pointer rests 120 ms on
  the top row of the monitor within the island's span + 100 DIP, or something new arrives
  (`activity_news`: up for 4 s), or the panel / a sheet opens or something urgent is shown (never
  hidden meanwhile; 1.2 s after). It hides again once the pointer has been away 900 ms (and the
  hold is over).
* **Hide when idle** (`island.hide_when_idle`, `HideMode::WhenIdle`) is the same `Edge` machine with another `busy`: `pure_idle_busy(ui, top) = ui || top > 0` (`top` = `App::top_priority()`, 0 = only the idle clock), i.e. *any* activity in the pill keeps the island up: music (a paused player keeps its pill, so it counts until the session is gone), `timer.run` / `timer.watch` (countdown, focus, break and stopwatch are one persistent activity at `TIMER` priority, never a gap between focus and break), downloads, charging, calls, agent pills, notifications, cards. After the last activity ends it stays `IDLE_HOLD_MS` (2 s, `In::busy_hold_ms`; 1.2 s in the old mode) and then needs the usual 900 ms away from the pointer, so a track change or the next pill does not blink it. Hidden, the top edge reveals it with the same zone / dwell / band as the other mode. `AppEvent::ActivityStarted` (emitted by `set_activity` when the pill leaves the idle clock) takes a sample at once so the island appears without waiting for the next poll. The input window is hidden while `Away::Hide` (`set_passthrough`), which is why a hidden island never swallows clicks on a page's navbar. `HideMode` (`Island::hide_mode`): both flags on = "always" wins. `placement::poll_ms` is given `edge_waiting` (hidden *and* a hide mode on), so a pointer parked in a browser's tab strip while music keeps the island up is looked at at the slow rate.
* `placement::compose` merges the two (hidden wins); `poll_ms` is the polling cadence. All of it is
  unit-tested with simulated pointer paths.

**Reduce motion** (`scene/reduce.rs`, `appearance.animations` = Normal / Kurangi / Mati): Windows
"Animation effects" off always wins (`effective`). *Kurangi*: the shape's springs are critically
damped (no overshoot) and settle within 160 ms instead of jumping; content fades; the away moves
never slide (a short fade). *Mati*: everything jumps, nothing moves.

**Testing without moving the cursor** (never send input; needs `MOKU_DEBUG=1`): `mokuctl debug cursor
<x> <y> [--dt <ms>] [--down]` feeds a *fake* pointer through `placement::sample`, the same path as
the real one; its clock is virtual (`--dt` ms since the previous sample) and the real pointer is
ignored until `debug cursor real`. The reply is the `placement` object of `debug state`, which also
shows `away`, `passthrough`, `app_hidden`, `monitor`, `home`. `placement.debug_fg:<exe>` and
`placement.debug_monitors:<spec>` (see 14) pretend a foreground app / other monitors.

## 10. Services (background threads)

```rust
r.add_service(ServiceSpec { name: "weather", enabled: |c| c.today.enabled && c.today.weather, run: weather });

fn weather(ctx: ServiceCtx) {
    loop {
        if let Ok(json) = crate::net::get_text(&url(&ctx.config()), Duration::from_secs(10)) {
            let w = parse(&json);
            ctx.post(move |app| { app.state_mut::<Today>().weather = w; app.request_redraw(Redraw::Tab("today")); });
        }
        if !ctx.sleep(Duration::from_secs(30 * 60)) { return; }  // false = stopping
    }
}
```

* Started when `enabled(config)` becomes true (at startup and on each reload), stopped when it
  becomes false or on exit. Stopping sets `ctx.should_stop()`, signals `ctx.stop_event()`
  (a Win32 event handle for `WaitForMultipleObjects`), unparks the thread (wakes
  `ctx.sleep`) and posts `WM_QUIT` to it (ends `ctx.run_message_loop()`). **Return promptly.**
* `ctx.post(f)` = `Ui::post(f)`; `ctx.config()` = current config snapshot;
  `ctx.run_message_loop()` pumps messages for message-only windows / STA COM callbacks.
* Poll at ≤ 1 Hz, and only what the user can see: prefer events; prefer `TabSpec::tick` for
  things only the panel shows. Idle budget for the whole island: ≤ 35 MB, 0 % CPU.
* Tests: `ServiceCtx::detached("name")` + `ctx.request_stop()`.

## 11. IPC

Protocol in `moku-core/src/ipc.rs`: one JSON line each way over the per-user pipe `ipc::pipe_name()`
(see "The pipe" below), ≤ 64 KB,
request must arrive within 2 s. `Request::{Ping, Notify{title, body, icon, id, progress},
Ask{title, detail, command, options, timeout_secs, kind, agent}, Todo{add},
Session{id, project, state, detail, ts, agent},
ClearData, Update{action: status|check|install}, Debug(DebugOp)}`,
`Response::{Ok, Pong{version}, Answer{choice}, State{state}, Update{status}, Error{message}}`.
`Update` is the updater's (feature `update`; Settings → Tentang and `mokuctl update`): the answer is
always the status `{state: idle|checking|up_to_date|available|downloading|installing|failed, current,
version?, error?, progress?, checked_at?}`; `check` is the daily request made now and answers `checking`.
`Ping`, `ClearData` (emits `AppEvent::DataCleared`, cancels pending saves, answers `Ok` once
the writer thread is idle) and `Debug` are handled by the core; register handlers for the others:

```rust
r.add_ipc_handler(IpcKind::Todo, on_todo);
fn on_todo(app: &mut App, req: Request, reply: Reply) {
    let Request::Todo { add } = req else { return };
    app.state_mut::<Today>().todos.push(add);
    reply.ok();                                   // or reply.send(Response::..) / reply.error("..")
}
```

`Reply` is `Send`; keep it in your state to answer later (`Ask`). `reply.is_closed()` is true
once the client went away (closed its end of the pipe, e.g. Ctrl+C on `mokuctl ask`) or the
connection thread stopped waiting — drop such pending cards. Dropping a `Reply` unanswered sends
`Error{"no answer"}`. The connection thread waits up to `timeout_secs + 30 s` for `Ask`, 5 s for
others. At most 64 connection threads wait for answers at once (`MAX_WAITING`); beyond that a
request is answered `Error{"busy"}` right away. With `ipc.enabled = false` the server is stopped. Clients: `moku_core::ipc::send(&req,
wait)`; tests: `moku::ipc_server::run_on(ctx, r"\\.\pipe\moku-test-<tag>-<pid>")` + `ipc::send_to` (a name
of the test process only).

`Session` (v0.2, feature `claude`, since v0.3 `agents`): agent session status, `state` = `SessionState`
(`start | working | active | needs_input | done | failed | subagent | end`), `ts` = ms since
the epoch (older than the session's last event → ignored). Sent by `mokuctl session` and by
`mokuctl claude-hook`, which maps Claude Code hook input to it and gives up after 100 ms
(`docs/claude-code-hook.md`). v0.3 (track Q): `agent` = `claude` (absent = Claude Code, the v0.2 wire
format) | `codex` | `gemini`: sessions of different agents never mix, and `mokuctl agent-hook` is the
hook command of all three (`docs/agents.md`). `Ask` gets `kind` (`shell` = `command` is a shell command
line) and `agent`: only such a card offers "Selalu izinkan perintah ini" and is auto-approved by a rule of
that agent, and only when the command is exactly the one a rule was made from (`moku_core::agent_rules`,
file `agent-rules.json` plus Moku's own copy `agent-rules.trusted.json`; never for a compound command line;
off unless `[agents] always_allow`; a rule that the file has but Moku did not write is pending until the
user confirms it in Settings).

**The pipe.** ONE function, `moku_core::ipc::pipe_name()`, names it for everybody: the island
(`ipc_server::run`), `mokuctl`, the settings app ("Hapus semua data" tells the island). Order:

1. `MOKU_PIPE`, when it starts with `\\.\pipe\` (otherwise it is ignored with a warning);
2. else, when `MOKU_DATA_DIR` is set, `\\.\pipe\moku-<12 hex digits hashed from that folder>`
   (spelling-insensitive: case, `/` vs `\`, a trailing separator). An island with a scratch data
   folder therefore never shares a pipe with the user's real island, and any client started with
   the same `MOKU_DATA_DIR` finds exactly that island: **tests and dev tools set `MOKU_DATA_DIR`
   (or a unique `MOKU_PIPE`) and so can never reach the real island**;
3. else `\\.\pipe\moku-<current user SID>`.

The server's DACL admits only the current user, SYSTEM and administrators. The *client* also
checks the server: before it writes anything `ipc::send_to` asks `GetNamedPipeServerProcessId`
who serves the pipe, compares that process's user SID with its own (`moku_core::winsid`) and
refuses with `IpcError::Untrusted` on a mismatch or when it cannot tell (anybody can create a
pipe with a given name first, and an `ask` would land in their hands). `mokuctl pipe-name`
prints the name for scripts (`tools/pipe-robustness-test.ps1`).

`mokuctl debug` ops (all return JSON; exit code 0/1): `cursor <x> <y> [--dt ms] [--down]` / `cursor real` (a fake pointer for the
placement logic, 9b), `open-tab <id>`, `close`, `state` (mode,
tab, enabled tabs, shown key, top/split activity, activities, hit rects, action ids on screen,
scale, services, tray_adds, sheet, input, flush_ms …), `click <action>`, `screenshot-ready` (returns when animations
settled), `activity <id> <priority> <label> [--secs s]`, `clear-activity <id>`, `type <text>
[--compose]`, `key <key>` (the focused text input, §9a).
**Every op except `state` is refused** (`Error`, "debug ops are disabled …") unless the island
runs with `MOKU_DEBUG=1` or is a debug build (`app::debug_op_allowed`): `click` reaches any
action, including `ai.approve` on a pending permission card, so another process of the same
user must not be able to drive the UI. Ordinary actions are never reachable over the pipe in a
release island started without `MOKU_DEBUG=1`.

## 12. Config, strings, network, logging

* Config: `app.config().today.city` etc. (structs in `moku-core/src/config/<section>.rs`).
  Changes arrive by hot reload: `on_config` handlers run, services are synced, the island
  redraws. Settings → island always goes through `config.toml` (the settings app saves it,
  the island watches it). Both writers do read-modify-write (`Config::update_file`): the
  settings app in `moku-settings/src/persist.rs`, features through `app.update_config`.
* Strings: Indonesian default, English available. Write pairs inline:
  `tr("Kosongkan", "Clear")` (any thread); formatted: `tr_s(format!(..), format!(..))`.
  Tab labels are `(id, en)` pairs. Dates: `i18n::clock(&t)` ("10.24"), `date_short(&d)`
  ("Sel, 24 Nov"), `weekday_short/long(mon0)`, `month_short/long(m1)`.
* Network: `crate::net::get(url, timeout) -> Result<(status, Vec<u8>), String>`,
  `get_text(url, timeout)`, `encode(s)` (query escaping), and `request(url, &Options, sink)` for
  anything else (POST, headers, a streamed answer, `Cancel`). Blocking — services and workers
  only. Only the hosts listed in the README ("Network"): Open-Meteo, LRCLIB (at most four requests per track: `get`, then three searches with the title and artist cleaned by `features/lyrics/title.rs`; `/api/get/<id>` alone for a choice the user made; "Bukan lagu ini" may add the searches later, never more than the plan), the Hari ini ones of
  v0.3 (the Google holiday calendar, BMKG, Frankfurter, Open-Meteo air quality), GitHub for updates,
  the user's own calendar addresses and the one base URL of Tanya AI (only while it is on, only
  when asked), each only while its feature is on. Services that need the Hari ini city
  as coordinates share one geocoding through `features/today/place.rs`
  (`place::resolve_and_publish`).
* Data files: `moku_core::paths::{data_dir, todo_file, clipboard_dir, shelf_dir, data_file}`
  (`%APPDATA%\Moku\…`; `MOKU_DATA_DIR` overrides for tests). Never write next to the exe.
* Logging: `log_info!(..)`, `log_warn!(..)`, `log_error!(..)` (macros, any thread) →
  `%APPDATA%\Moku\logs\moku.log`.
* A config file that cannot be parsed as a whole (half-saved, locked) is ignored at runtime:
  the island keeps its last good config and logs a warning. Bad single values → defaults.

## 13. Complete example feature

A "water" counter: a tab with a button, a pill alert every hour, a service, an IPC-free design.

```rust
//! features/water_example.rs
use std::time::Duration;
use crate::activity::{priority, HEADS_UP_SECS};
use crate::app::{App, Redraw, Ui};
use crate::i18n::tr;
use crate::registry::*;
use crate::services::ServiceCtx;
use crate::ui::*;

#[derive(Default)]
struct Water { glasses: u32 }

pub fn register(r: &mut Registry) {
    r.add_state(Water::default());
    r.add_tab(TabSpec { id: "water", order: 25, icon: icons::CLOUD, label: ("Minum", "Water"),
        enabled: |c| c.reminders.water, build: panel, tick: None });
    r.on_action("water", |app, ev| if ev.name() == "add" && ev.kind == ActionKind::Click {
        app.state_mut::<Water>().glasses += 1;
        app.request_redraw(Redraw::All);
    });
    r.add_service(ServiceSpec { name: "water", enabled: |c| c.reminders.water, run: reminder });
}

fn panel(app: &App) -> Node {
    let w = app.state::<Water>();
    let target = app.config().reminders.water_target;
    col([
        label(tr("Minum", "Water")),
        text(format!("{} / {target}", w.glasses)).font_size(typo::PANEL_TITLE).bold(),
        progress(w.glasses as f32 / target as f32).color(color::TEAL).thickness(6.0),
        button_accent(tr("+1 gelas", "+1 glass"), app.accent(), "water.add"),
    ]).gap(10.0).fill_w()
}

fn reminder(ctx: ServiceCtx) {
    let every = Duration::from_secs(60 * ctx.config().reminders.water_interval_min as u64);
    while ctx.sleep(every) {
        Ui::post(|app| app.alert("water.due", priority::DUE, HEADS_UP_SECS, |_| {
            pill([icon(icons::CLOUD).color(color::TEAL), text(tr("Waktunya minum", "Time to drink")).semibold(),
                  button(tr("Sudah", "Done"), "water.add")])
        }));
    }
}
```

## 13a. Helper process (`moku.exe --launcher-index`, `--icons`)

The shell's heavy libraries (shortcut resolution, `shell:AppsFolder`, the image factory with its
thumbnail handlers) pull several MB of pages into a process. Instead of loading them in the island and
then trying to hide the number (trimming the working set is forbidden here: it only hides it), the island
starts itself again for that work and reads a file:

```
moku.exe --launcher-index <out> [--no-store-apps] [--sample]   Start Menu + packaged apps + 48 px icons -> <out>, exit
moku.exe --icons <list-file> <out>                             the icons of the files named in <list-file>, one per line -> <out>, exit
```

Both run before anything else in `main` (no single-instance mutex, no window, no log), below normal
priority, without a console, and the island kills them after 90 s / 15-20 s (`helper::run_index`,
`run_icons`). The output is written atomically (temp file + rename) in the compact format of
`helper/pack.rs` (names, targets / AUMIDs, premultiplied BGRA icons; a damaged file decodes to
nothing). The island reads a file into ONE buffer and keeps icons as offsets into it, so dropping it
(the launcher does so 2 minutes after it was last closed) returns the whole block. Users today: the
launcher's app list (`features/launcher/{enumerate,index}.rs`; first open starts the helper, then at most
every 10 minutes while in use; before the first index the previous cache is shown, else "Mengindeks…"),
the launcher's Shelf-file icons and the mixer's app icons (`control/mixer/wasapi.rs`). `launch()` itself
(ShellExecute when the user presses Enter) still runs in the island, on a worker thread.

## 13b. Phone transfer and the Shelf converter (`features/phone_send/`, `features/shelf/convert.rs`; v0.3, track R)

A session is one temporary HTTP server. Nothing exists (no thread, no socket) while no session runs.

| File | What |
|---|---|
| `phone_send/mod.rs` | state (`Phone`, one `Live` session), start / end, the events from the server (`Ui::post`), the approval card, the Shelf-tab session card (`panel(app)`, which `shelf::panel` shows instead of its grid), pills, `phone.sample:*` |
| `phone_send/session.rs` | **pure** session rules with an injected clock: token (CNG random, 22 base64url characters, constant-time compare), 10-minute expiry, linger after a transfer, one-device lock, approval (`Pending` / `Allowed` / `Denied`, 90 s), the announced list and caps (2 GiB per file, 4 GiB per batch, 50 files, 100 KB text) |
| `phone_send/adapter.rs` | which adapter: pure `pick` / `candidates` over a list (up, Ethernet or Wi-Fi, default gateway, private IPv4, not virtual / VPN), `list()` = `GetAdaptersAddresses` |
| `phone_send/http.rs` | request head reader with limits (8 KB head, 32 headers, deadline), `Body` (exactly `Content-Length`, idle timeout), response head with the defensive headers |
| `phone_send/multipart.rs` | streaming `multipart/form-data` parser (boundary split across reads, 8 KB part heads, truncation detected by `finish`) |
| `phone_send/names.rs` | `sanitize` (one safe Windows file name), `numbered` / `moku_variant` + `create_unique` (`create_new`, never replaces), `move_no_replace`, `content_disposition` |
| `phone_send/server.rs` | listener thread, one short thread per connection (cap 8), routing by literal segments after the token, streamed upload to `<name>.mokupart` then rename, indexed download |
| `phone_send/page.rs` | the one HTML page (no framework, no external file) |
| `shelf/convert.rs` | `Target`, `targets_for`, `convert` (WIC in 4 MB bands with EXIF orientation / `MediaTranscoder`), `create_output` = `name (Moku).ext` |
| `ui/qr.rs` | the one QR encoder of the app (see section 9, "QR codes"): the session card calls `qr_image(url, 8) -> Option<Image>` (8 px modules plus the quiet zone) and keeps the image in `Live` while it shows it |

Rules: the server binds ONE address chosen by `choose_bind` (the LAN adapter's IPv4; with `MOKU_DEBUG=1`
`MOKU_PHONE_BIND` may name a *loopback* address, nothing else). Every request is checked in this
order: `Host`, token, expiry, one-device lock, then for `POST` the `Origin`, `X-Moku` and `Sec-Fetch-Site`
headers, then the approval. Server threads never touch the UI: they send `server::Event`s through
`Ui::post`. `shelf::add_received(app, path)` puts a received / converted file on the Shelf without the
"Disimpan ke Shelf" pill. Dependencies: `windows-future` (already part of the `windows` crate's tree)
for the `MediaTranscoder` progress handler, and the `Media_Transcoding` / `Media_MediaProperties`
features.

## 14. Testing your feature

* Unit tests for pure logic in your file; `cargo test -p moku <name>`.
* Run the island with `MOKU_DEBUG=1` in its environment (`$env:MOKU_DEBUG = "1"` before
  starting `moku.exe`, no flags), then drive it with `mokuctl debug …`: `open-tab <id>`,
  `click <action>`, `state`, `screenshot-ready`. Without `MOKU_DEBUG=1` a release island
  answers every debug op except `state` with an error (§11). Then screenshot with
  `prototype\shot.ps1 -NoStart -Out prototype\results\v01\<feature>-<what>.png`.
* Idle check: `prototype\measure.ps1 -Exe <...>\moku.exe -Seconds 30 -Label <name>`.
* `app/tools/`: `hover-test.ps1` (moves the cursor, never clicks), `tray-reregister-test.ps1`,
  `pipe-robustness-test.ps1`, `island-burst.ps1` (frames of the island only), `island-lock.ps1` (one lock file
  for all git worktrees: `<main repo root>\.build\island.lock`),
  `release.ps1` / `release-key.ps1`. Every script finds the repo and the build folder from its own
  location (`tools\_common.ps1`), and the ones that send debug commands refuse to run unless
  `MOKU_DATA_DIR` or `MOKU_PIPE` is set (see "The pipe"): a script can never reach the user's own island.
* **Debug / sample actions** feed fake data through the real code paths. They only run with
  `MOKU_DEBUG=1` in the island's environment or in a debug build; otherwise
  `App::dispatch_action` ignores them and logs a warning (one check, in the core:
  `app::is_debug_action`). The rule: the action name (before any `:`) starts with `sample` or
  `debug_`, or is listed in `app::DEBUG_ACTIONS`. Current list:

  | Action | What it does |
  |---|---|
  | `media.sample` / `media.sample_off` | fake music session (art, lyrics) / drop it and rebind the real one |
  | `volume.test` | show the volume pill with the real level (changes nothing) |
  | `system.sample:<caps-on\|caps-off\|battery-low\|battery-plugged\|battery-unplugged\|net-lost\|net-back>` | system pills |
  | `privacy.sample:<cam\|mic\|both\|off>` | camera / mic dots |
  | `meeting.sample:<zoom\|teams\|meet\|off>` | meeting detected |
  | `clipboard.debug_text:<text>` / `clipboard.debug_image:<seed>` | add a history item without touching the real clipboard |
  | `shelf.debug_drop:<path\|path…>` / `shelf.debug_enter` / `shelf.debug_leave` | file drop on the island |
  | `reminders.sample_water` / `reminders.sample_eye` | reminder pills |
  | `prayer.sample_next` / `prayer.sample_now` | prayer heads-up / now pills |
  | `prayer.debug_at:<yyyy-mm-dd>T<hh:mm[:ss]>` / `prayer.debug_at:off` | live in that moment (the clock advances in real time): the prayer times, Hijri date, Ramadan pill and sunnah reminders all follow it. For testing Ramadan, which is months away |
  | `adzan.debug_play[:<fajr\|dhuhr\|asr\|maghrib\|isha>[:<volume %>]]` | play the configured adzan sound through the real code path, at most 15 % loud and 2.5 s long |
  | `adzan.debug_media` | log how many media sessions play / are paused / are something else (counts only) |
  | `ramadan.sample_sunnah[:<monday\|thursday\|bidh\|arafah\|tasua\|asyura>]` | the puasa sunnah reminder pill |
  | `today.sample_rain` | rain heads-up pill |
  | `system.sample:battery-plugged[:<pct>]` / `battery-plugged-idle` | the charging pill (level 1-100) / plugged in but not charging |
  | `earbuds.sample:<connect\|connect-nobattery\|update:<pct>\|low\|off>` | Bluetooth headset pills through the real snapshot logic |
  | `notify_mirror.sample:<wa\|otp\|long\|other\|many\|clear>` / `notify_mirror.sample_status:<listening\|access\|package\|off>` | invented Windows notifications through the mirror's real path / its status line (`MOKU_NOTIFY_MIRROR_SAMPLE=1` also keeps the real listener from starting) |
  | `ask.debug_open:<text>` / `ask.debug_ask:<question>` | the Tanya AI card with a sample selection / a question sent at once (never reads another program, never presses keys) |
  | `launcher.debug_hotkey` | the launcher hotkey's handler (open ↔ close), then `debug type` / `debug key`; with `MOKU_LAUNCHER_SAMPLE=1` in the island's environment the helper lists invented apps instead of the real Start Menu |
  | `control.debug_mixer` | the mixer view with invented apps (never touches real audio sessions) |
  | `volume.debug_device:<name>` | show `<name>` as the output device on the Kontrol tab (an invented one, for screenshots) |
  | `placement.debug_fg:<exe>` / `placement.debug_fg:-` | pretend `<exe>` is the foreground app (the `island.hide_apps` rule runs through its real code) / the real one again |
  | `placement.debug_monitors:<WxH@X,Y[*dpi];...>` | invented monitors (the first is the primary one; empty = the real ones), e.g. `1920x1080@0,0;2560x1440@1920,0*1.25`; the island moves to the one the config chooses |
  | `today.sample_due` | a to-do that is due now, through the real reminder path (pill with "Selesai" / "Tunda 10 m"; adds the to-do) |
  | `today.sample_holiday[:today|:eve]` | invented holidays: the badge under the date and the next-holiday line, or the "Besok libur" pill |
  | `bmkg.sample_quake[:near|:big|:tsunami]` / `bmkg.sample_warning` | an invented earthquake (about 90 km away, M 6.4 far away, or M 7.1 with a tsunami line) / weather warning, through the real thresholds (nothing saved) |
  | `glance.sample_aqi[:<aqi>]` / `glance.sample_rate` | an invented air quality reading (default 159: the pill and the chip) / exchange rate chip |
  | `clipboard.debug_text:<text>` / `clipboard.debug_text_from:<exe>\|<text>` | as above, through the real `ingest` (detectors, action tags, the one-time-code rule); `_from` says which app "copied" it (`WhatsApp.exe\|482913` is a code) |
  | `ocr.debug_start` / `ocr.debug_region:<x>,<y>,<w>,<h>` | the OCR overlay over a *synthetic* picture of rendered text (never the real screen), then choose that rectangle of it as if it had been dragged; the debug run does not write the real clipboard |
  | `phone.sample:<waiting\|send\|connected\|card\|card-risky\|progress\|done\|text\|done-pill>` | the phone-transfer session card and pills with sample data (a sample address, never a real one; no server) |
  | `shelf.debug_converting:<tile>:<percent>` / `:off` | a conversion progress ring on a Shelf tile without converting anything |

  Name new test hooks `<feature>.sample…` or `<feature>.debug_…` so they are gated too.
* Never click, scroll or type into windows that aren't the island; never restart Explorer.
* Only one island at a time (single-instance mutex); stop it with tray → Keluar,
  Ctrl+Alt+Shift+Q, or `Stop-Process -Name moku` before rebuilding.

## 15. Things that are easy to get wrong

* `on_start` is not called in `--state` / `--demo` (screenshot modes); `on_config` is.
* A builder or `TabSpec::build` is called often (every flush while visible). Keep it pure; no
  I/O, no allocation of new `Image`s.
* After changing state from a service, post to the UI thread **and** call `request_redraw`.
* Activity ids are global: prefix them with your feature name.
* `hit_hwnd()` changes size/shape on every state change but keeps its handle; register drop
  targets once in `on_start`.
* The UI thread is an STA: WinRT async `.get()` there can deadlock — do it in a service.
* Transient activities with a high priority hide persistent ones only while they last; to
  "stay until dismissed" use `persistent` and `clear_activity` on the dismiss action.
* Feature-local `windows` imports: everything is under `windows::…`.
* **COM callbacks** (Core Audio `IAudioEndpointVolumeCallback`, `IMMNotificationClient`,
  `IDropTarget`, `INetworkListManagerEvents` …): `#[windows::core::implement]` does **not**
  compile in this crate — the macro expands to `::windows_core::…`, and `windows-core` is not a
  direct dependency of `moku`. Write the object by hand instead, like
  `features/volume_osd.rs` (`StaticCom` + a `static …_Vtbl` with `IUnknown_Vtbl { QueryInterface,
  AddRef, Release }` for a process-lifetime singleton), `features/shelf/dnd.rs` (`IDropTarget`)
  and `features/system/service.rs` (`INetworkListManagerEvents`): a `#[repr(C)]` struct whose
  first field points at the generated `I…_Vtbl`, `unsafe extern "system"` functions for each
  method, `QueryInterface` answering `IUnknown::IID` and the interface's `IID`. Never panic
  inside these functions (they are called by the OS; `panic = "abort"`).
