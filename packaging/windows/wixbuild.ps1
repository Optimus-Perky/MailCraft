param([Parameter(Mandatory = $true)][string]$Version)
$ErrorActionPreference = "Stop"
$d = "C:\mailcraftbuild"
Set-Location $d
# The program must carry this version, or an upgrade would keep the old file (see build-installer.sh).
$has = (Get-Item "$d\MailCraft.exe").VersionInfo.FileVersion
if ($has -ne "$Version.0") { throw "MailCraft.exe is version '$has', not $Version.0: build it with MAILCRAFT_VERSION=$Version" }
# The installer's logo: the stamp, small.
Add-Type -AssemblyName System.Drawing
$src = [System.Drawing.Image]::FromFile("$d\mailcraft-256.png")
$logo = New-Object System.Drawing.Bitmap 64, 64
$g = [System.Drawing.Graphics]::FromImage($logo)
$g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
$g.DrawImage($src, 0, 0, 64, 64); $logo.Save("$d\logo.png"); $g.Dispose(); $src.Dispose()

& C:\wix314\candle.exe -nologo -arch x64 "-dVersion=$Version" "-dBuildDir=$d" -ext WixUtilExtension -out mailcraft.wixobj mailcraft.wxs
if ($LASTEXITCODE -ne 0) { throw "candle (msi) failed" }
# ICE61: the same-version upgrade (deliberate: a rebuild of one commit replaces itself).
# ICE57 and ICE43: per-user data in a per-machine component whenever it owns a shortcut (ICE43
# wants a HKCU key path for a non-advertised one); neither can see that InstallScope="perMachine"
# makes the Start menu and desktop the All Users ones.
& C:\wix314\light.exe -nologo -ext WixUtilExtension -sice:ICE43 -sice:ICE57 -sice:ICE61 -out mailcraft.msi mailcraft.wixobj
if ($LASTEXITCODE -ne 0) { throw "light (msi) failed" }
& C:\wix314\candle.exe -nologo -arch x64 "-dVersion=$Version" "-dBuildDir=$d" -ext WixBalExtension -ext WixUtilExtension -out bundle.wixobj bundle.wxs
if ($LASTEXITCODE -ne 0) { throw "candle (bundle) failed" }
& C:\wix314\light.exe -nologo -ext WixBalExtension -ext WixUtilExtension -out MailCraft-setup.exe bundle.wixobj
if ($LASTEXITCODE -ne 0) { throw "light (bundle) failed" }
# The program and the installer have the same name; the installer goes out under it.
Move-Item -Force MailCraft.exe MailCraft-program.exe
Move-Item -Force MailCraft-setup.exe MailCraft.exe
$v = (Get-Item "$d\MailCraft.exe").VersionInfo
"  built MailCraft.exe (installer) {0:N0} bytes, file version {1}, product {2}" -f (Get-Item "$d\MailCraft.exe").Length, $v.FileVersion, $v.ProductName
