param([string]$OutDir)
# Draws the Pods4 app icon (two earbuds on a blue→violet rounded square) at every
# standard Windows icon size and packs them into a PNG-compressed .ico.
Add-Type -AssemblyName System.Drawing
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $OutDir | Out-Null

function New-RoundRect([single]$x, [single]$y, [single]$w, [single]$h, [single]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = 2 * $r
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    return $p
}

function Draw-Bud($g, [single]$s, [single]$cx, [single]$cy, [single]$angle, [bool]$mirror) {
    # One earbud in a 256-unit design space: round head with a darker tip, plus a tapered stem.
    $state = $g.Save()
    $g.TranslateTransform($cx * $s, $cy * $s)
    $g.RotateTransform($angle)
    if ($mirror) { $g.ScaleTransform(-1, 1) }
    $white = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 250, 251, 255))
    $shade = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 222, 229, 246))
    $tip   = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 186, 200, 236))
    # Ear tip pokes out of the inner side of the head (drawn first so the head overlaps it).
    $g.FillEllipse($tip, (16 * $s), (-26 * $s), (28 * $s), (26 * $s))
    # Stem: long rounded blade hanging from the head.
    $stem = New-RoundRect (-14 * $s) (-6 * $s) (28 * $s) (100 * $s) (14 * $s)
    $g.FillPath($shade, $stem)
    $g.FillEllipse($white, (-36 * $s), (-40 * $s), (66 * $s), (60 * $s))
    $g.Restore($state)
}

$sizes = 16, 20, 24, 32, 40, 48, 64, 256
$pngs = @{}
foreach ($size in $sizes) {
    $bmp = New-Object System.Drawing.Bitmap $size, $size, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $g.InterpolationMode = 'HighQualityBicubic'
    $g.PixelOffsetMode = 'HighQuality'
    $g.Clear([System.Drawing.Color]::Transparent)
    $s = $size / 256.0

    $bg = New-RoundRect (8 * $s) (8 * $s) (240 * $s) (240 * $s) (56 * $s)
    $grad = New-Object System.Drawing.Drawing2D.LinearGradientBrush (
        (New-Object System.Drawing.PointF 0, 0),
        (New-Object System.Drawing.PointF $size, $size),
        ([System.Drawing.Color]::FromArgb(255, 0x38, 0x87, 0xF5)),
        ([System.Drawing.Color]::FromArgb(255, 0x6C, 0x4D, 0xE6)))
    $g.FillPath($grad, $bg)

    Draw-Bud $g $s 76 100 -9 $false
    Draw-Bud $g $s 180 100 9 $true

    $g.Dispose()
    $path = Join-Path $OutDir "pods4-$size.png"
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    $pngs[$size] = [System.IO.File]::ReadAllBytes($path)
}

# ICO container: ICONDIR + one ICONDIRENTRY per image, images stored as PNG.
$ms = New-Object System.IO.MemoryStream
$w = New-Object System.IO.BinaryWriter $ms
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
foreach ($size in $sizes) {
    $dim = if ($size -ge 256) { 0 } else { $size }
    $w.Write([byte]$dim); $w.Write([byte]$dim); $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([uint16]1); $w.Write([uint16]32)
    $w.Write([uint32]$pngs[$size].Length); $w.Write([uint32]$offset)
    $offset += $pngs[$size].Length
}
foreach ($size in $sizes) { $w.Write($pngs[$size]) }
$w.Flush()
[System.IO.File]::WriteAllBytes((Join-Path $OutDir 'pods4.ico'), $ms.ToArray())

# Keep only the sizes the app embeds directly; the rest live inside the .ico.
foreach ($size in $sizes) { if ($size -ne 256 -and $size -ne 64) { Remove-Item (Join-Path $OutDir "pods4-$size.png") } }
Get-ChildItem $OutDir | Select-Object Name, Length
