<#
  Regenerates logo.png (256x256, the "m on a waterline" app icon: dark squircle, cyan->violet->pink
  stroke) with System.Drawing. The geometry is the same as `Gfx::logo` in the island
  (moku/src/scene/gfx.rs), so the settings window, tray and island show the same mark.
  Usage: powershell -File make-logo.ps1 [-Out logo.png] [-Size 256]
#>
param([string]$Out = (Join-Path $PSScriptRoot "logo.png"), [int]$Size = 256)
Add-Type -AssemblyName System.Drawing
$S = $Size
$bmp = New-Object System.Drawing.Bitmap($S, $S, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
$g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
$g.Clear([System.Drawing.Color]::Transparent)

function RoundRect([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = 2 * $r
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    return $p
}

# Squircle body: deep navy with a soft magenta glow in the top-right corner.
$inset = $S * 0.02
$body = RoundRect $inset $inset ($S - 2 * $inset) ($S - 2 * $inset) ($S * 0.24)
$bg = New-Object System.Drawing.Drawing2D.LinearGradientBrush((New-Object System.Drawing.PointF(0, 0)), (New-Object System.Drawing.PointF($S, $S)), [System.Drawing.Color]::FromArgb(255, 16, 20, 40), [System.Drawing.Color]::FromArgb(255, 10, 10, 22))
$g.FillPath($bg, $body)
$g.SetClip($body)
$glow = New-Object System.Drawing.Drawing2D.GraphicsPath
$glow.AddEllipse([float]($S * 0.45), [float](-$S * 0.45), [float]($S * 0.9), [float]($S * 0.9))
$pgb = New-Object System.Drawing.Drawing2D.PathGradientBrush($glow)
$pgb.CenterColor = [System.Drawing.Color]::FromArgb(150, 200, 50, 110)
$pgb.SurroundColors = @([System.Drawing.Color]::FromArgb(0, 200, 50, 110))
$g.FillPath($pgb, $glow)
$g.ResetClip()
$edge = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(40, 255, 255, 255), [float]($S * 0.006))
$g.DrawPath($edge, $body)

# The m: same 100-unit geometry as Gfx::logo, scaled 1.85x and centred.

$k = $S * 0.0086
$tx = $S / 2 - 50 * $k
$ty = $S / 2 - 54.25 * $k
function P([double]$x, [double]$y) { return New-Object System.Drawing.PointF([float]($tx + $x * $k), [float]($ty + $y * $k)) }
$m = New-Object System.Drawing.Drawing2D.GraphicsPath
$m.StartFigure()
$a = P 10 70; $b = P 25 70
$m.AddLine($a, $b)
$m.AddLine((P 25 70), (P 25 51))
$m.AddArc([float]($tx + 25 * $k), [float]($ty + 38.5 * $k), [float](25 * $k), [float](25 * $k), 180, 180)
$m.AddLine((P 50 51), (P 50 70))
$m.AddLine((P 50 70), (P 50 51))
$m.AddArc([float]($tx + 50 * $k), [float]($ty + 38.5 * $k), [float](25 * $k), [float](25 * $k), 180, 180)
$m.AddLine((P 75 51), (P 75 70))
$m.AddLine((P 75 70), (P 90 70))
$x0 = $tx + 5.2 * $k; $x1 = $tx + 94.8 * $k
$gr = New-Object System.Drawing.Drawing2D.LinearGradientBrush((New-Object System.Drawing.PointF([float]$x0, 0)), (New-Object System.Drawing.PointF([float]$x1, 0)), [System.Drawing.Color]::FromArgb(255, 0x4F, 0xD8, 0xFF), [System.Drawing.Color]::FromArgb(255, 0xFF, 0x5F, 0xA2))
$blend = New-Object System.Drawing.Drawing2D.ColorBlend(3)
$blend.Colors = @([System.Drawing.Color]::FromArgb(255, 0x4F, 0xD8, 0xFF), [System.Drawing.Color]::FromArgb(255, 0x7C, 0x6B, 0xFF), [System.Drawing.Color]::FromArgb(255, 0xFF, 0x5F, 0xA2))
$blend.Positions = @([single]0, [single]0.5, [single]1)
$gr.InterpolationColors = $blend
$pen = New-Object System.Drawing.Pen($gr, [float](9.5 * $k))
$pen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
$pen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
$pen.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
$g.DrawPath($pen, $m)

$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
"wrote $Out"
