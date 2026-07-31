# Generate TabVoice logo: rounded pill + mic glyph.
# Output: media/logo.png (512), src/tray_icon.png (256), src/tray_icon.ico (256, PNG-embedded)
Add-Type -AssemblyName System.Drawing

function New-LogoBitmap([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.Clear([System.Drawing.Color]::Transparent)

    $s = $size / 256.0  # scale factor relative to 256 design

    # --- Pill background ---
    $pill = New-Object System.Drawing.RectangleF((28*$s), (40*$s), (200*$s), (176*$s))
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $r = 52*$s
    $d = $r * 2
    $path.AddArc($pill.X, $pill.Y, $d, $d, 180, 90)
    $path.AddArc($pill.Right - $d, $pill.Y, $d, $d, 270, 90)
    $path.AddArc($pill.Right - $d, $pill.Bottom - $d, $d, $d, 0, 90)
    $path.AddArc($pill.X, $pill.Bottom - $d, $d, $d, 90, 90)
    $path.CloseFigure()

    $rect = New-Object System.Drawing.RectangleF($pill.X, $pill.Y, $pill.Width, $pill.Height)
    $grad = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
        $rect,
        [System.Drawing.Color]::FromArgb(255, 59, 130, 246),   # #3B82F6
        [System.Drawing.Color]::FromArgb(255, 29, 78, 216),    # #1D4ED8
        [System.Drawing.Drawing2D.LinearGradientMode]::Vertical)
    $g.FillPath($grad, $path)

    # Subtle inner highlight ring
    $penRing = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(70, 255, 255, 255), (3*$s))
    $g.DrawPath($penRing, $path)

    # --- Mic glyph (white) ---
    $white = [System.Drawing.Brushes]::White
    # Capsule body
    $cap = New-Object System.Drawing.RectangleF((97*$s), (50*$s), (62*$s), (98*$s))
    $capPath = New-Object System.Drawing.Drawing2D.GraphicsPath
    $cr = 27*$s
    $cd = $cr * 2
    $capPath.AddArc($cap.X, $cap.Y, $cd, $cd, 180, 90)
    $capPath.AddArc($cap.Right - $cd, $cap.Y, $cd, $cd, 270, 90)
    $capPath.AddArc($cap.Right - $cd, $cap.Bottom - $cd, $cd, $cd, 0, 90)
    $capPath.AddArc($cap.X, $cap.Bottom - $cd, $cd, $cd, 90, 90)
    $capPath.CloseFigure()
    $g.FillPath($white, $capPath)

    # Stem
    $stem = New-Object System.Drawing.RectangleF((120*$s), (148*$s), (16*$s), (26*$s))
    $g.FillRectangle($white, $stem)

    # Holder base (rounded)
    $base = New-Object System.Drawing.RectangleF((85*$s), (172*$s), (86*$s), (16*$s))
    $basePath = New-Object System.Drawing.Drawing2D.GraphicsPath
    $br = 7*$s
    $bd = $br * 2
    $basePath.AddArc($base.X, $base.Y, $bd, $bd, 180, 90)
    $basePath.AddArc($base.Right - $bd, $base.Y, $bd, $bd, 270, 90)
    $basePath.AddArc($base.Right - $bd, $base.Bottom - $bd, $bd, $bd, 0, 90)
    $basePath.AddArc($base.X, $base.Bottom - $bd, $bd, $bd, 90, 90)
    $basePath.CloseFigure()
    $g.FillPath($white, $basePath)

    # Sound wave arcs (right side)
    $wavePen = New-Object System.Drawing.Pen($white, (6*$s))
    $wavePen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $wavePen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    # outer arc
    $g.DrawArc($wavePen, (172*$s), (70*$s), (36*$s), (58*$s), -50, 100)
    # inner arc
    $g.DrawArc($wavePen, (188*$s), (86*$s), (26*$s), (42*$s), -50, 100)

    $penRing.Dispose(); $grad.Dispose(); $wavePen.Dispose()
    $path.Dispose(); $capPath.Dispose(); $basePath.Dispose()
    $g.Dispose()
    return $bmp
}

function Write-IcoFromPng([string]$pngPath, [string]$icoPath) {
    $pngBytes = [System.IO.File]::ReadAllBytes($pngPath)
    $fs = [System.IO.File]::Create($icoPath)
    $w = New-Object System.IO.BinaryWriter($fs)
    # ICONDIR
    $w.Write([uint16]0)   # reserved
    $w.Write([uint16]1)   # type: icon
    $w.Write([uint16]1)   # count
    # ICONDIRENTRY
    $w.Write([byte]0)     # width 256 (0 = 256)
    $w.Write([byte]0)     # height 256
    $w.Write([byte]0)     # color count
    $w.Write([byte]0)     # reserved
    $w.Write([uint16]1)   # planes
    $w.Write([uint16]32)  # bit count
    $w.Write([uint32]$pngBytes.Length)
    $w.Write([uint32]22)  # offset (6 + 16)
    $w.Write($pngBytes)
    $w.Close()
}

$root = Split-Path -Parent $PSScriptRoot
$mediaDir = Join-Path $root "media"
$srcDir = Join-Path $root "src"
New-Item -ItemType Directory -Force -Path $mediaDir | Out-Null

$big = New-LogoBitmap 512
$big.Save((Join-Path $mediaDir "logo.png"), [System.Drawing.Imaging.ImageFormat]::Png)
$big.Dispose()

$icon256 = New-LogoBitmap 256
$iconPng = Join-Path $srcDir "tray_icon.png"
$icon256.Save($iconPng, [System.Drawing.Imaging.ImageFormat]::Png)
Write-IcoFromPng $iconPng (Join-Path $srcDir "tray_icon.ico")
$icon256.Dispose()

Write-Output "OK: media/logo.png, src/tray_icon.png, src/tray_icon.ico"
