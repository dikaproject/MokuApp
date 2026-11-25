# Moku island — prototype spec (throwaway spike)

Moku is a planned Windows 10/11 app: a small glass "dynamic island" pill at the top centre of the
screen that grows into panels. This spike answers three questions with real numbers before the
product is built:

1. Can the island animate **smoothly** (spring motion, no stutter) on Windows?
2. How close can we get to the **Apple-style glass** look of the mockups?
3. How **light** is it: idle RAM, idle CPU, exe size?

It is a spike: keep code small and readable, no tests needed, but it must build in release mode
and run without crashing. Two implementations are built side by side for comparison:

- `island-composition/` — Rust + `windows` crate + Windows.UI.Composition (main candidate)
- `island-slint/` — Rust + Slint (comparison)

## Reference images (`ref/`)

- `state-diam.png` idle pill · `state-kursor-mendekat.png` hover · `state-musik.png` music pill
- `state-notifikasi.png` notification pill · `panel-musik.png` expanded music panel
- `panel-kontrol.png` control panel (not required, for look only) · `fondasi-gaya.png` style foundations

Mockup rendered on a colourful wallpaper; on a real desktop the glass sits over whatever is behind.

**Change from the mockups:** the idle pill shows the **logo mark** instead of the two "eyes".
Logo mark = a lowercase *m* drawn as one rounded stroke standing on a horizon line, in a 100×100 box:
`M 10 70 H 25 V 51 A 12.5 12.5 0 0 1 50 51 V 70 V 51 A 12.5 12.5 0 0 1 75 51 V 70 H 90`,
stroke width 9.5, round caps/joins, horizontal gradient #4FD8FF → #7C6BFF → #FF5FA2.
In the pill draw it at ~18 px.

## States (all required)

| id (CLI `--state`) | Size (DIP) | Content |
|---|---|---|
| `idle` | ~120 × 36, fully round | logo mark 18 px + time "10.24" (13 px SemiBold, tabular) |
| `hover` | ~200 × 40 | logo mark, "10.24", "Sel, 24 Nov" (secondary text), small chevron-down |
| `music` | ~230 × 36 | 24 px album-art square (gradient #FF8A5C→#FF3D77, radius 7), "Midnight Drive" (SemiBold), "Kaia" (secondary), 5 small audio bars (#FF375F) |
| `music-panel` | 640 × ~280, radius 38 | Expanded music panel: 156 px art (radius 28), title 20 Bold, artist 13, three lyric lines (middle one bright), progress bar with 1:24 / 3:47, row with prev / play-pause (white 46 px circle) / next. Tab bar optional. |

Positions: centred horizontally on the primary monitor, top edge 8 DIP below the screen top
(panel top edge also 8 DIP). DPI-aware (per-monitor v2); sizes above are DIP.

## Behaviour

- Start in `idle` (or the state given by `--state <id>`, which then stays put — used for screenshots).
- Pointer enters the pill → `hover`; leaves → back to the previous resting state.
- Click on the pill (or pointer rests on it for 400 ms) → `music-panel`; pointer leaves the panel → collapse.
- `--demo`: loop forever idle → hover → music → music-panel → music → idle, 1.5 s per step
  (used for CPU-while-animating measurement and to eyeball smoothness).
- `Esc` (when the island has focus) or a tray-less hard exit via `Ctrl+C` in console is enough; no tray icon needed.
- Outside the visible pill/panel, mouse clicks must pass through to the windows below
  (the island must never block clicking the top of the screen).
- Must not appear in the taskbar or Alt-Tab. Always on top.

## Motion (target)

| Transition | Motion |
|---|---|
| pointer enters | ease-out ~120 ms, slight grow |
| open panel | spring, damping ≈ 0.82, ≈ 350 ms |
| close panel | spring, damping ≈ 1.0, ≈ 280 ms |
| pill ↔ pill state change | spring, damping ≈ 0.75, ≈ 400 ms |
| panel content | fade + 8 px slide, staggered ~40 ms per item |

Size and corner radius must morph continuously (no jump-cut between shapes). Content cross-fades.

## Look (target)

- Glass pill: dark tint #0A0A12 at ~47% over a blurred backdrop; panel tint #0C0C16 at ~52%.
- Rim: 1 px gradient stroke, white 55% top-left → 5% → 25% bottom-right.
- Soft shadow: y 10–26, blur 28–64, black 40–50%.
- Text white 100% / 72% / 46%; font Segoe UI Variable or Inter if available (Segoe UI fine).
- If a real backdrop blur is not achievable in your stack, use the tint alone (slightly more
  opaque, ~75%) and **say so plainly** in your report — do not fake it with a screenshot of the wallpaper.

## Lightness rules

- Release profile: `opt-level = 3` (or `"s"` if smaller and no slower), `lto = true`,
  `codegen-units = 1`, `panic = "abort"`, `strip = true`.
- No busy loops or per-frame timers while idle. Idle CPU should be ~0%.
- Windows subsystem (no console window) in release, except allow `--console` for logs if useful.

## Tooling on this machine

- Rust stable MSVC via rustup: `$env:USERPROFILE\.cargo\bin\cargo.exe` (may not be on PATH in
  your shell yet — prepend `$env:USERPROFILE\.cargo\bin` to `$env:Path`).
- MSVC Build Tools 2022 + Windows SDK 10.0.26100. Windows 11 build 26300 (26H2), transparency on.
- `measure.ps1` — RAM/CPU sampler (see its header). `shot.ps1` — screenshot of the top-centre
  region while a state is showing. Both write into `results/`. Running them shows windows at the
  top of the user's screen for a few seconds; that is expected.
- PowerShell 5.1 is the shell (no `&&`; use `;` and `if ($?)`).
