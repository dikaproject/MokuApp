<#
.SYNOPSIS
  Renders Moku's application icon ("m di garis air": a lowercase m drawn as one rounded stroke
  standing on a horizon, cyan -> violet -> pink, on a dark squircle) at every size Windows asks
  for and writes
    moku.ico                  16 20 24 32 40 48 64 96 128 256, all 32-bit with alpha
                              (BMP entries up to 48 px, PNG entries from 64 px up)
    png\moku-<size>.png       the same renders as plain PNGs
  next to this script. Every size is drawn from the vector geometry, never scaled from another
  size (each one is drawn at 8x / 4x and box-filtered down).

  The geometry and colours are those of `Gfx::logo` in the island (moku/src/scene/gfx.rs) and of
  moku-settings\assets\make-logo.ps1, whose 256 px output this reproduces. What changes with the
  size, so the m stays readable:
    256 .. 96   the logo as is (proportional padding)
    64          the logo, a slightly heavier stroke, a whole pixel of padding (so the squircle edge is crisp)
    48 .. 32    heavier stroke, a slightly larger m, one pixel of padding, no hairline edge
    24 .. 16    the grid takes over: stems on whole pixels, stroke 2 px, shorter feet (the
                horizon shrinks to a nub), a flat navy body without the magenta glow, and the
                squircle runs to the edge of the icon (no padding) so the m can be as big as possible

.PARAMETER Sheet
  Optional path of a contact sheet (all sizes at 1:1 and the small ones enlarged, on a dark and a
  light background), e.g. prototype\results\v02\icon-sheet.png.

.EXAMPLE
  powershell -File make-icon.ps1
  powershell -File make-icon.ps1 -Sheet ..\..\..\prototype\results\v02\icon-sheet.png
#>
param(
    [string]$OutDir = $PSScriptRoot,
    [string]$Sheet = ""
)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
if (-not ([System.Management.Automation.PSTypeName]'IconKit').Type) {
    Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @'
using System;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;

public static class IconKit {
    static byte[] Read(Bitmap b) {
        BitmapData d = b.LockBits(new Rectangle(0, 0, b.Width, b.Height), ImageLockMode.ReadOnly, PixelFormat.Format32bppArgb);
        byte[] buf = new byte[b.Width * b.Height * 4];
        for (int y = 0; y < b.Height; y++) Marshal.Copy(IntPtr.Add(d.Scan0, y * d.Stride), buf, y * b.Width * 4, b.Width * 4);
        b.UnlockBits(d);
        return buf;
    }

    // Straight-alpha BGRA, top row first, no padding.
    public static byte[] Pixels(Bitmap b) { return Read(b); }

    // Box filter by an integer factor, averaging in premultiplied space (so the transparent
    // corners do not bleed their colour into the edge pixels).
    public static Bitmap Downsample(Bitmap src, int f) {
        int w = src.Width / f, h = src.Height / f;
        byte[] s = Read(src);
        byte[] o = new byte[w * h * 4];
        int n = f * f;
        for (int y = 0; y < h; y++) {
            for (int x = 0; x < w; x++) {
                long a = 0, r = 0, g = 0, bl = 0;
                for (int j = 0; j < f; j++) {
                    int row = ((y * f + j) * src.Width + x * f) * 4;
                    for (int i = 0; i < f; i++) {
                        int p = row + i * 4;
                        int pa = s[p + 3];
                        bl += s[p] * pa; g += s[p + 1] * pa; r += s[p + 2] * pa; a += pa;
                    }
                }
                int q = (y * w + x) * 4;
                if (a > 0) {
                    o[q] = (byte)((bl + a / 2) / a);
                    o[q + 1] = (byte)((g + a / 2) / a);
                    o[q + 2] = (byte)((r + a / 2) / a);
                }
                o[q + 3] = (byte)((a + n / 2) / n);
            }
        }
        Bitmap dst = new Bitmap(w, h, PixelFormat.Format32bppArgb);
        BitmapData d = dst.LockBits(new Rectangle(0, 0, w, h), ImageLockMode.WriteOnly, PixelFormat.Format32bppArgb);
        for (int y = 0; y < h; y++) Marshal.Copy(o, y * w * 4, IntPtr.Add(d.Scan0, y * d.Stride), w * 4);
        dst.UnlockBits(d);
        return dst;
    }
}
'@
}
Add-Type -AssemblyName System.Drawing

$Sizes = 16, 20, 24, 32, 40, 48, 64, 96, 128, 256
$Navy1 = [System.Drawing.Color]::FromArgb(255, 16, 20, 40)
$Navy2 = [System.Drawing.Color]::FromArgb(255, 10, 10, 22)
$Cyan = [System.Drawing.Color]::FromArgb(255, 0x4F, 0xD8, 0xFF)
$Violet = [System.Drawing.Color]::FromArgb(255, 0x7C, 0x6B, 0xFF)
$Pink = [System.Drawing.Color]::FromArgb(255, 0xFF, 0x5F, 0xA2)

# ---------------------------------------------------------------------------------------------
# What is drawn at each size. All lengths are pixels of the final icon.
#   d      distance between the stems (the m's arches are half circles of diameter d)
#   h      stem height below the arches' spring line      f   length of the feet
#   w      stroke width                                    cx / base   centre and baseline of the m
#   inset  padding between the squircle and the icon edge  radius  corner radius of the squircle
#   glow   magenta glow in the top-right corner            edge  hairline along the squircle
#   ss     supersampling factor
# ---------------------------------------------------------------------------------------------
function Get-Spec([int]$S) {
    # The logo proper: 100-unit geometry (stems 25 apart, arches 12.5 radius, stems 19 tall, feet
    # 15 long, stroke 9.5) times k, which centres the ink in the squircle like make-logo.ps1.
    function Logo([int]$S, [double]$kScale, [double]$strokeUnits) {
        $k = $S * 0.0086 * $kScale
        $d = 25 * $k; $h = 19 * $k
        return @{ S = $S; d = $d; h = $h; f = 15 * $k; w = $strokeUnits * $k; cx = $S / 2.0; base = $S / 2.0 + ($h + $d / 2) / 2
            inset = $(if ($S -le 64) { 1 } else { $S * 0.02 }); radius = $S * 0.24; glow = $true; edge = ($S -ge 64); ss = $(if ($S -ge 96) { 4 } else { 8 }) }
    }
    switch ($S) {
        256 { return Logo 256 1.00 9.5 }
        128 { return Logo 128 1.00 9.5 }
        96 { return Logo 96 1.00 10 }
        64 { return Logo 64 1.00 10.5 }
        48 { return Logo 48 1.05 10.5 }
        40 { return Logo 40 1.06 11 }
        32 { return Logo 32 1.08 11.5 }
        # Hand-fitted to the pixel grid: stems and the feet's undersides land on whole pixels.
        24 { return @{ S = 24; d = 6; h = 5; f = 2; w = 2; cx = 12; base = 16; inset = 0; radius = 5.5; glow = $false; edge = $false; ss = 8 } }
        20 { return @{ S = 20; d = 5; h = 4.5; f = 1.5; w = 2; cx = 10; base = 14; inset = 0; radius = 4.5; glow = $false; edge = $false; ss = 8 } }
        16 { return @{ S = 16; d = 4; h = 4; f = 1; w = 2; cx = 8; base = 11; inset = 0; radius = 3.5; glow = $false; edge = $false; ss = 8 } }
    }
    throw "no spec for $S"
}

function New-RoundRect([double]$x, [double]$y, [double]$w, [double]$h, [double]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $dd = 2 * $r
    $p.AddArc([float]$x, [float]$y, [float]$dd, [float]$dd, 180, 90)
    $p.AddArc([float]($x + $w - $dd), [float]$y, [float]$dd, [float]$dd, 270, 90)
    $p.AddArc([float]($x + $w - $dd), [float]($y + $h - $dd), [float]$dd, [float]$dd, 0, 90)
    $p.AddArc([float]$x, [float]($y + $h - $dd), [float]$dd, [float]$dd, 90, 90)
    $p.CloseFigure()
    return $p
}

function Render-Icon([hashtable]$s) {
    $ss = $s.ss
    $N = $s.S * $ss
    $bmp = New-Object System.Drawing.Bitmap($N, $N, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.Clear([System.Drawing.Color]::Transparent)

    # Squircle body: deep navy; the bigger sizes get the soft magenta glow in the top-right corner.
    $in = $s.inset * $ss
    $body = New-RoundRect $in $in ($N - 2 * $in) ($N - 2 * $in) ($s.radius * $ss)
    $bg = New-Object System.Drawing.Drawing2D.LinearGradientBrush((New-Object System.Drawing.PointF(0, 0)), (New-Object System.Drawing.PointF([float]$N, [float]$N)), $Navy1, $Navy2)
    $g.FillPath($bg, $body)
    if ($s.glow) {
        $g.SetClip($body)
        $glow = New-Object System.Drawing.Drawing2D.GraphicsPath
        $glow.AddEllipse([float]($N * 0.45), [float](-$N * 0.45), [float]($N * 0.9), [float]($N * 0.9))
        $pgb = New-Object System.Drawing.Drawing2D.PathGradientBrush($glow)
        $pgb.CenterColor = [System.Drawing.Color]::FromArgb(150, 200, 50, 110)
        $pgb.SurroundColors = @([System.Drawing.Color]::FromArgb(0, 200, 50, 110))
        $g.FillPath($pgb, $glow)
        $g.ResetClip()
    }
    if ($s.edge) {
        $edge = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(40, 255, 255, 255), [float]($N * 0.006))
        $g.DrawPath($edge, $body)
    }

    # The m: left foot, left stem, arch, middle stem (down and back up), arch, right stem, right foot.
    $cx = $s.cx * $ss; $base = $s.base * $ss; $d = $s.d * $ss; $h = $s.h * $ss; $f = $s.f * $ss
    $spring = $base - $h
    $m = New-Object System.Drawing.Drawing2D.GraphicsPath
    $m.StartFigure()
    $m.AddLine([float]($cx - $d - $f), [float]$base, [float]($cx - $d), [float]$base)
    $m.AddLine([float]($cx - $d), [float]$base, [float]($cx - $d), [float]$spring)
    $m.AddArc([float]($cx - $d), [float]($spring - $d / 2), [float]$d, [float]$d, 180, 180)
    $m.AddLine([float]$cx, [float]$spring, [float]$cx, [float]$base)
    $m.AddLine([float]$cx, [float]$base, [float]$cx, [float]$spring)
    $m.AddArc([float]$cx, [float]($spring - $d / 2), [float]$d, [float]$d, 180, 180)
    $m.AddLine([float]($cx + $d), [float]$spring, [float]($cx + $d), [float]$base)
    $m.AddLine([float]($cx + $d), [float]$base, [float]($cx + $d + $f), [float]$base)

    # Cyan -> violet -> pink across the ink, left to right.
    $x0 = $cx - $d - $f - $s.w * $ss / 2
    $x1 = $cx + $d + $f + $s.w * $ss / 2
    $gr = New-Object System.Drawing.Drawing2D.LinearGradientBrush((New-Object System.Drawing.PointF([float]$x0, 0)), (New-Object System.Drawing.PointF([float]$x1, 0)), $Cyan, $Pink)
    $blend = New-Object System.Drawing.Drawing2D.ColorBlend(3)
    $blend.Colors = @($Cyan, $Violet, $Pink)
    $blend.Positions = @([single]0, [single]0.5, [single]1)
    $gr.InterpolationColors = $blend
    $pen = New-Object System.Drawing.Pen($gr, [float]($s.w * $ss))
    $pen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $pen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    $pen.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
    $g.DrawPath($pen, $m)
    $g.Dispose()

    $out = [IconKit]::Downsample($bmp, $ss)
    $bmp.Dispose()
    return $out
}

# ---- ICO writing -----------------------------------------------------------------------------
function Get-PngBytes([System.Drawing.Bitmap]$b) {
    $ms = New-Object System.IO.MemoryStream
    $b.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    return ,$ms.ToArray()
}

# A classic icon image: BITMAPINFOHEADER (height doubled), bottom-up BGRA, then the 1-bpp AND mask.
function Get-DibBytes([System.Drawing.Bitmap]$b) {
    $n = $b.Width
    $px = [IconKit]::Pixels($b)
    $maskRow = [int]([math]::Floor(($n + 31) / 32) * 4)
    $ms = New-Object System.IO.MemoryStream
    $w = New-Object System.IO.BinaryWriter($ms)
    $w.Write([uint32]40); $w.Write([int32]$n); $w.Write([int32](2 * $n))
    $w.Write([uint16]1); $w.Write([uint16]32); $w.Write([uint32]0)
    $w.Write([uint32]($n * $n * 4 + $maskRow * $n))
    $w.Write([int32]0); $w.Write([int32]0); $w.Write([uint32]0); $w.Write([uint32]0)
    for ($y = $n - 1; $y -ge 0; $y--) { $w.Write($px, $y * $n * 4, $n * 4) }
    for ($y = $n - 1; $y -ge 0; $y--) {
        $row = New-Object byte[] $maskRow
        for ($x = 0; $x -lt $n; $x++) {
            if ($px[($y * $n + $x) * 4 + 3] -lt 128) { $row[$x -shr 3] = $row[$x -shr 3] -bor (0x80 -shr ($x -band 7)) }
        }
        $w.Write($row)
    }
    $w.Flush()
    return ,$ms.ToArray()
}

function Write-Ico([string]$path, $images) {
    $ms = New-Object System.IO.MemoryStream
    $w = New-Object System.IO.BinaryWriter($ms)
    $w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$images.Count)
    $offset = 6 + 16 * $images.Count
    foreach ($im in $images) {
        $dim = if ($im.Size -ge 256) { 0 } else { $im.Size }
        $w.Write([byte]$dim); $w.Write([byte]$dim); $w.Write([byte]0); $w.Write([byte]0)
        $w.Write([uint16]1); $w.Write([uint16]32)
        $w.Write([uint32]$im.Data.Length); $w.Write([uint32]$offset)
        $offset += $im.Data.Length
    }
    foreach ($im in $images) { $w.Write($im.Data) }
    $w.Flush()
    [System.IO.File]::WriteAllBytes($path, $ms.ToArray())
}

# ---- go --------------------------------------------------------------------------------------
$pngDir = Join-Path $OutDir "png"
New-Item -ItemType Directory -Force $pngDir | Out-Null
$renders = @{}
$images = @()
foreach ($S in $Sizes) {
    $bmp = Render-Icon (Get-Spec $S)
    $renders[$S] = $bmp
    $bmp.Save((Join-Path $pngDir "moku-$S.png"), [System.Drawing.Imaging.ImageFormat]::Png)
    $data = if ($S -ge 64) { Get-PngBytes $bmp } else { Get-DibBytes $bmp }
    $images += [pscustomobject]@{ Size = $S; Data = [byte[]]$data }
}
$ico = Join-Path $OutDir "moku.ico"
Write-Ico $ico $images
"wrote $ico ($((Get-Item $ico).Length) bytes, $($Sizes.Count) sizes) and $($Sizes.Count) PNGs in $pngDir"

# ---- contact sheet ---------------------------------------------------------------------------
function Write-Sheet([string]$path) {
    $zoomed = 16, 20, 24, 32
    $gap = 28; $pad = 36
    $rowW = ($Sizes | Measure-Object -Sum).Sum + $gap * ($Sizes.Count - 1)
    $zoomW = (($zoomed | ForEach-Object { $_ * 8 }) | Measure-Object -Sum).Sum + $gap * ($zoomed.Count - 1)
    $W = [int]([math]::Max($rowW, $zoomW) + 2 * $pad)
    $panelH = 36 + 256 + 36 + 28 + 256 + 36 + 28
    $H = 2 * $panelH + 24
    $sheet = New-Object System.Drawing.Bitmap($W, $H, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($sheet)
    $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
    $g.Clear([System.Drawing.Color]::FromArgb(255, 128, 128, 128))
    $font = New-Object System.Drawing.Font("Segoe UI", 10)
    $title = New-Object System.Drawing.Font("Segoe UI Semibold", 11)
    $panels = @(
        @{ Name = "dark background"; Bg = [System.Drawing.Color]::FromArgb(255, 32, 32, 32); Fg = [System.Drawing.Color]::FromArgb(255, 200, 200, 205) },
        @{ Name = "light background"; Bg = [System.Drawing.Color]::FromArgb(255, 243, 243, 243); Fg = [System.Drawing.Color]::FromArgb(255, 60, 60, 66) }
    )
    $top = 0
    foreach ($p in $panels) {
        $g.FillRectangle((New-Object System.Drawing.SolidBrush($p.Bg)), 0, $top, $W, $panelH)
        $fg = New-Object System.Drawing.SolidBrush($p.Fg)
        $g.DrawString("Moku icon on a $($p.Name): actual size (1:1)", $title, $fg, [single]$pad, [single]($top + 12))
        # Row 1: every size at 1:1, sharing a baseline.
        # Edge pixels must not sample beyond the image (that draws a grey halo around the enlarged ones).
        $clamp = New-Object System.Drawing.Imaging.ImageAttributes
        $clamp.SetWrapMode([System.Drawing.Drawing2D.WrapMode]::TileFlipXY)
        $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::NearestNeighbor
        $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::Half
        $y0 = $top + 44 + 256
        $x = $pad
        foreach ($S in $Sizes) {
            $g.DrawImage($renders[$S], (New-Object System.Drawing.Rectangle($x, ($y0 - $S), $S, $S)), 0, 0, $S, $S, [System.Drawing.GraphicsUnit]::Pixel, $clamp)
            $g.DrawString("$S", $font, $fg, [single]$x, [single]($y0 + 6))
            $x += $S + $gap
        }
        # Row 2: the small sizes at 8x, nearest neighbour, so every pixel is visible.
        $y1 = $y0 + 36 + 28
        $g.DrawString("enlarged 8x (nearest neighbour): the pixels that ship", $title, $fg, [single]$pad, [single]($y1 - 4))
        $x = $pad
        foreach ($S in $zoomed) {
            $g.DrawImage($renders[$S], (New-Object System.Drawing.Rectangle($x, ($y1 + 24), ($S * 8), ($S * 8))), 0, 0, $S, $S, [System.Drawing.GraphicsUnit]::Pixel, $clamp)
            $g.DrawString("$S px", $font, $fg, [single]$x, [single]($y1 + 24 + $S * 8 + 4))
            $x += $S * 8 + $gap
        }
        $top += $panelH + 24
    }
    $g.Dispose()
    New-Item -ItemType Directory -Force (Split-Path -Parent $path) | Out-Null
    $sheet.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $sheet.Dispose()
    "wrote $path"
}
if ($Sheet) { Write-Sheet ([System.IO.Path]::GetFullPath($Sheet)) }
foreach ($b in $renders.Values) { $b.Dispose() }
