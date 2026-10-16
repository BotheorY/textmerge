# SPDX-License-Identifier: MIT
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$assetDirectory = Join-Path $PSScriptRoot '..\assets'
$sizes = @(16,20,24,30,32,36,40,48,60,64,72,80,96,128,256)
$images = [System.Collections.Generic.List[byte[]]]::new()
foreach ($size in $sizes) {
    $bitmap = [System.Drawing.Bitmap]::new($size,$size,[System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $graphics.ScaleTransform($size/256.0,$size/256.0)
    $graphics.Clear([System.Drawing.Color]::Transparent)
    # Two documents converge into one: simple shapes remain legible at 16 pixels.
    $blue = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(255,37,99,235))
    $teal = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(255,13,148,136))
    $white = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(255,248,250,252))
    $shadow = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(40,15,23,42))
    $graphics.FillRectangle($shadow,30,34,104,166)
    $graphics.FillRectangle($blue,24,24,100,164)
    $graphics.FillRectangle($shadow,132,62,104,166)
    $graphics.FillRectangle($teal,128,52,100,164)
    $pen = [System.Drawing.Pen]::new([System.Drawing.Color]::White,11)
    $pen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $pen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    $graphics.DrawLine($pen,48,64,96,64)
    $graphics.DrawLine($pen,48,94,96,94)
    $graphics.DrawLine($pen,152,92,204,92)
    $graphics.DrawLine($pen,152,122,204,122)
    $graphics.FillEllipse($white,84,148,88,88)
    $arrow = [System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(255,30,64,175),12)
    $arrow.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $arrow.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    $graphics.DrawLine($arrow,108,174,128,194)
    $graphics.DrawLine($arrow,148,174,128,194)
    $graphics.DrawLine($arrow,128,194,128,214)
    $stream = [System.IO.MemoryStream]::new()
    $bitmap.Save($stream,[System.Drawing.Imaging.ImageFormat]::Png)
    $images.Add($stream.ToArray())
    if ($size -eq 256) {$bitmap.Save((Join-Path $assetDirectory 'textmerge.png'),[System.Drawing.Imaging.ImageFormat]::Png)}
    $stream.Dispose(); $pen.Dispose(); $arrow.Dispose()
    $blue.Dispose(); $teal.Dispose(); $white.Dispose(); $shadow.Dispose(); $graphics.Dispose(); $bitmap.Dispose()
}
$file = [System.IO.File]::Create((Join-Path $assetDirectory 'textmerge.ico'))
$writer = [System.IO.BinaryWriter]::new($file)
$writer.Write([uint16]0);$writer.Write([uint16]1);$writer.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($index=0;$index -lt $sizes.Count;$index++) {
    $dimension = if ($sizes[$index] -eq 256) {0} else {$sizes[$index]}
    $writer.Write([byte]$dimension);$writer.Write([byte]$dimension)
    $writer.Write([byte]0);$writer.Write([byte]0);$writer.Write([uint16]1);$writer.Write([uint16]32)
    $writer.Write([uint32]$images[$index].Length);$writer.Write([uint32]$offset)
    $offset += $images[$index].Length
}
foreach ($bytes in $images) {$writer.Write($bytes)}
$writer.Dispose();$file.Dispose()
