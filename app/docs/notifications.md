# Windows notifications on the island (`notify_mirror`)

Moku can show the Windows notifications of apps you choose (WhatsApp, Telegram, Discord,
Outlook, Teams ...) as a pill, with the app's logo, the title and the text, and a **Salin kode**
button when the text carries a one-time code. This page records what was found out about the
Windows API behind it, how the feature is built, and what a maintainer does if Windows turns out
to need a package identity for it.

**Status: written and unit-tested, not verified with real notifications.** Reading the user's own
notifications was out of bounds while building it (see "The spike"), so whether Windows delivers
notifications to a plain `moku.exe` is something to confirm once, on a machine you own (see
"Check it on your PC"). When it does not, the island says so and the feature stays off.

## The spike: does `UserNotificationListener` work from an unpackaged exe?

`moku.exe` is an ordinary unpackaged desktop program: no MSIX, no package identity.

**What was done** (2026-11-25, Windows 11 build 26300, plain `cargo test` process, which is an
unpackaged exe exactly like `moku.exe`):

* `UserNotificationListener::Current().GetAccessStatus()` and nothing else. No
  `RequestAccessAsync` (that shows the consent prompt: never called, anywhere in Moku), no
  `GetNotificationsAsync`, no `NotificationChanged`, no notification was read.
* `GetCurrentPackageFullName` to confirm the process has no identity (`APPMODEL_ERROR_NO_PACKAGE`).
* A read of `HKCU\...\CapabilityAccessManager\ConsentStore\userNotificationListener` (read only):
  the master value is `Allow`, and there is no entry for any app, packaged or not.

**What came out:** the call works from the unpackaged process and answers `Allowed`, with no
consent entry for the exe at all. `cargo test -p moku spike_ -- --nocapture` prints it:

```
SPIKE unpackaged UserNotificationListener.GetAccessStatus = Allowed; package identity = false
```

**What that does and does not prove.** `Allowed` is the status of the *master switch*; nothing
says that Windows then hands a process without identity the notifications themselves. Microsoft's
documentation describes the API with a manifest capability and does not describe unpackaged use:

* "To use the notification listener, you must add the User Notification Listener capability to
  your app manifest" (a `Package.appxmanifest`, i.e. a package), and the user must grant access
  with `RequestAccessAsync` from a UI thread:
  <https://learn.microsoft.com/en-us/windows/apps/design/shell/tiles-and-notifications/notification-listener>
* The capability is `userNotificationListener` (`uap3:Capability`), a *general-use* one, "access
  to APIs in the Windows.UI.Notifications.Management namespace", and "most scenarios for app
  capabilities are relevant only to apps that have package identity":
  <https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/app-capability-declarations>
* The class page only says it needs the user's permission and lists the methods:
  <https://learn.microsoft.com/en-us/uwp/api/windows.ui.notifications.management.usernotificationlistener>
* Packaging with external location ("sparse package") is Microsoft's way to give an existing
  unpackaged app an identity, without changing its installer:
  <https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/grant-identity-to-nonpackaged-apps>

So the answer is **inconclusive on purpose**: the API answers from an unpackaged exe, the docs
require a capability that only a package can declare, and the only way to learn what really
happens is to ask for notifications, which the task forbade. The implementation therefore does
not guess; it **decides at run time** and says what it found:

| Windows says | The island shows (Notifikasi tab, "Notifikasi Windows") |
|---|---|
| `GetAccessStatus` = `Allowed` and the event can be subscribed | nothing special: it listens; allowed apps appear |
| `Denied` / `Unspecified` | "Windows belum mengizinkan Moku membaca notifikasi. Nyalakan di Pengaturan Windows → Privasi & keamanan → Notifikasi." Moku never opens the prompt itself |
| the API fails (or the subscription is refused) in a process **without** package identity | "**Butuh versi Store/MSIX**: Windows hanya memberi akses notifikasi ke aplikasi dengan identitas paket" |

The same text is in Settings → Notifikasi → "Notifikasi Windows di island". The feature is **off by
default**.

### Check it on your PC (one time, about a minute)

1. Settings → Notifikasi → switch "Tampilkan notifikasi aplikasi pilihan" on (WhatsApp, Telegram,
   Discord, Outlook, Teams and Signal are listed by default).
2. Open the island's Notifikasi tab. "Notifikasi Windows" shows nothing while all is well; a
   message there is the status above.
3. Have an allowed app notify you (a message to yourself works). It should appear as a pill and in
   the list.

If step 3 shows nothing and no status message appears, Windows accepted the subscription but
sends nothing to a process without identity: follow "If it needs a package identity".

## What the API exposes

A notification (`UserNotification`) gives: the app's display name, its AppUserModelId, its logo
(`AppInfo.DisplayInfo`), the creation time, and the toast's text: the first text element is
treated as the title, the others as the body. That is all that is used. Notifications of other
kinds (tiles, badges) are not available; images inside a toast are not read.

* **Allowlist.** Only apps on the list are shown; matched, ignoring case, against the display name
  **or** the AppUserModelId, as a whole (`Teams` is not `Steam`). Default: WhatsApp, Telegram,
  Telegram Desktop, Discord, Outlook, Microsoft Teams, Teams, Signal. An app not on the list is
  dropped after its *name* was read: its title and text are never read.
* **Gmail and other websites** cannot be listed: a browser's web notification is reported under the
  browser's name ("Google Chrome", "Microsoft Edge"), whatever the site. Adding the browser
  mirrors every site you allowed notifications for, so it is not a default.
* Apps that sent something but are not allowed are offered as "Izinkan notifikasi dari: [+ name]"
  buttons in the tab (names only, in memory, at most 8).

## Privacy

* **Never persisted.** The list lives in memory only, capped by `[notify_mirror] max_items`
  (default 30); no file is written. "Hapus semua data" empties it.
* **Never logged.** Not the title, the text, the code, nor the app names; the log only says
  whether the listener started.
* **Meetings.** With `[privacy] hide_notification_body_in_meetings` (default on) the text is
  replaced by "Isi disembunyikan selama rapat", as in the notification centre.
* **Focus mode / Jangan ganggu** hold the pill; the notification still goes to the list.
* **Codes.** A one-time code is recognised only next to a keyword (*kode*, *code*, *OTP*,
  *verifikasi*, *verification*, ...): 4-8 digits, or two equal groups (`123 456`). Amounts, times,
  dates, order numbers and years are not codes. **The code is masked** in the pill and in the panel (one dot
  per digit, as the clipboard history shows a code: "Kode OTP Anda ••••••"); only "Salin kode" has the real digits. "Salin kode" puts the digits on the clipboard
  with the formats that keep them out of Moku's clipboard history, Windows' clipboard history and
  cloud clipboard (`ExcludeClipboardContentFromMonitorProcessing`, `CanIncludeInClipboardHistory`).
* **Windows keeps its own copy.** Moku never removes or clears a notification in Windows
  (`RemoveNotification` / `ClearNotifications` are not used); dismissing in the island only drops
  Moku's copy.
* Only `NotificationChanged` events with `Added` are handled: what is already in the action centre
  when Moku starts is never read.

## If it needs a package identity

Everything for a sparse package is in the repository; **nothing registers it and nothing installs
a certificate** (Moku, `release.ps1` and the installer do none of that).

* `app\packaging\sparse\AppxManifest.xml.in`: the identity package (manifest only, external
  content), with the capabilities `runFullTrust`, `unvirtualizedResources` and
  `userNotificationListener`.
* `app\packaging\sparse\moku.manifest.in`: the `msix` element that ties `moku.exe` to it; linked
  only when `MOKU_MSIX_PUBLISHER` is set while building (`build-support\winres.rs`), so a normal
  build is byte-for-byte what it was.
* `app\tools\release.ps1 -SparsePackage`: builds `moku.exe` with that manifest and writes
  `Moku.Identity.msix` next to the installer (needs `makeappx.exe` from the Windows SDK; signs it
  when `-PackageCert` is given, otherwise leaves it unsigned with a warning).
  `-SparsePackage -IdentityOnly -OutDir <dir> -Publisher "CN=..."` only writes the package. The
  manifest was checked with `makeappx pack`, which accepts it.

The maintainer's steps (Microsoft's, from the page above):

1. A signing certificate whose **Subject equals the package's Publisher** (`CN=...`). A real one
   (Azure Trusted Signing, a code-signing CA) for end users; for yourself, a self-signed one.
2. `release.ps1 -SparsePackage -PackageCert <your.pfx>` (password in
   `$env:MOKU_PACKAGE_CERT_PASSWORD`; the Publisher is read from the certificate).
3. On a machine that should have the identity, registered per user **with the install folder as the
   external location** (this is the installer's job, not done here):

   ```powershell
   Add-AppxPackage -Path .\Moku.Identity.msix -ExternalLocation "$env:LOCALAPPDATA\Programs\Moku"
   ```

   A self-signed certificate must first be trusted on that machine (its public `.cer` in
   `Cert:\CurrentUser\TrustedPeople`, error `0x800B0109` otherwise). Remove with
   `Get-AppxPackage Moku.Island | Remove-AppxPackage`.
4. Start the `moku.exe` from that folder (the `moku.exe` built with `-SparsePackage`, which
   carries the manifest); `GetAccessStatus` / the event then work with an identity. The user
   still grants access in Windows Settings (Privacy & security → Notifications) themselves.

Known limits: the identity package needs Windows 10 2004 (build 19041) or later; the exe and the
package must agree on Publisher, package name (`Moku.Island`) and application id (`MokuIsland`);
registering a package of a version that is already registered fails (`0x80073CF9`), unregister
first. A Microsoft Store (MSIX) build would not need any of this, only the capability in its
manifest.

## Testing

* `cargo test -p moku notify_mirror`: allowlist, code detection, meeting hiding, the in-memory cap,
  de-duplication, the status mapping, the event queue.
* `cargo test -p moku spike_ -- --nocapture`: the spike above (access status only).
* With `MOKU_DEBUG=1` the island adds `notify_mirror.sample:<wa|otp|long|other|many|clear>` and
  `notify_mirror.sample_status:<listening|access|package|off>`
  (`mokuctl debug click notify_mirror.sample:otp`): invented notifications through the real path.
  With `MOKU_NOTIFY_MIRROR_SAMPLE=1` as well the real listener never starts, so a test island
  cannot read your real notifications.
