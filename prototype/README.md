# prototype/

The throwaway spike that came before the app in `app/` (see `SPEC.md`): two small prototypes
(`island-composition/`, `island-slint/`), the reference mockups (`ref/`), helper scripts
(`shot.ps1`, `measure.ps1`, `dwm-compare.ps1`) and screenshots (`results/`).

## Where `results/measurements.jsonl` went

`measure.ps1` appends one JSON line per run to `prototype/results/measurements.jsonl` (idle
working set, private memory, CPU, GPU memory). Each line records the full path of the exe that
was measured, which is a path on the developer's own machine, so **the file is not part of the
repository**: it is listed in `.gitignore` and created again locally the first time you run
`measure.ps1`. The numbers that matter are written up in `app/README.md` ("Known limitations"),
not in that file.

## Which screenshots are published

**The public repository has only `results/v03/final-*.png` and `results/v04/ui-fitur.png`, `ui-island.png`,
`ui-tentang-update-available.png`** (the current look, sample data only). Everything below describes the
developer's local copy, where the older shots are kept.

- `results/v02/` holds screenshots taken with **sample data only** (invented apps, devices and files;
  the launcher, mixer, Kontrol and installer ones are made with the debug sample hooks described in
  `app/docs/ARCHITECTURE.md`, §14).
- The v0.1 shots in `results/v01/` are sample data too, except `final-tab-monitor.png` (a real process
  name and the machine's hardware), `final-tab-control.png` and `final-tab-media.png` (the name of the
  audio device) and `f-tentang.png` / `f-umum.png` (a developer build folder): leave those out of a
  public copy.
- The spike screenshots at the top of `results/` (`comp-*.png`, `slint-*.png` except the `lightbg` and
  `burst-open` ones, `zoom-pills*.png`) and the whole of `results/comp-frames/` were captured over the
  developer's real desktop (browser tabs, chats, photos): they are **not** for publication.
  `results/comp-frames-v2/` and the `*-testbg*` / `lightbg` / `burst-open` shots show only the test
  backdrop (some `*-testbg*` ones have a strip of the real desktop below it: leave those out too).

## v0.3 screenshots (`results/v03/`)

- **`final-*.png` are the current ones**, recaptured from the code of the v0.3 fix round (2026-11-25) with sample
  data only: the island pills with `app/tools/island-burst.ps1` (an opaque backdrop of exactly the capture rect,
  the island rect only, at most 300 ms per burst, no input sent) and the settings pages with
  `app/moku-settings/tools/shot.ps1` (`PrintWindow` of its own window, a throw-away data folder). They show the
  tsunami pill, the masked OTP of a mirrored notification, Hari ini with Hijri + holiday + agenda + BMKG tsunami
  card + weather + to-dos, the polite island aside, the Tanya AI card (a sample selection, nothing is sent), the
  phone cards (approval and the Public-network question), the OCR pill, and the settings sidebar with 12 entries
  (also at the smallest window), Tanya AI, Memori & Privasi, Pintasan, Ibadah, Notifikasi and the adzan page.
- The other `v03/` shots (`l-`, `m-`, `n-`, `o-`, `p-`, `q-`, `r-` prefixes) were taken by the feature tracks
  **before the merge and the fix round**. Superseded ones were deleted (settings pages with the old sidebar
  order, the old default hotkeys or stacked privacy text, the `[REDACTED:otp]` preview, an unmasked mirrored OTP,
  the stretched "Tambah" and "Pilih..." buttons). The ones that remain (`o-settings-*`, `p-settings-fitur*`,
  `m-onboarding-ai`, and the island pills and cards) can still show the old sidebar order (Tentang before
  Tanya AI), the dimmed "HP" tab, or the Hari ini column before the fit rules: read `final-*` for the current look.

