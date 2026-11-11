<#
Builds DicingSplicing for Windows and gathers everything it needs to run into one folder: the
editor, its plugins, ffmpeg and ffprobe, and yt-dlp for the link plugin. Then zips the folder.

    scripts\package.cmd
    powershell -ExecutionPolicy Bypass -File scripts\package.ps1 [-FFmpegDir <folder>] [-YtDlp <file>] [-NoZip]

The result is dist\DicingSplicing-<version>\ and dist\DicingSplicing-<version>.zip. Unpacked anywhere,
the folder runs as it is: nothing has to be installed. The Telegram plugin's credentials.json is
never copied: it holds a bot's token, and each user brings their own.
#>
param(
    # A folder with ffmpeg.exe and ffprobe.exe. Found automatically when not given.
    [string]$FFmpegDir = "",
    # yt-dlp.exe, for the link plugin. Found automatically when not given; left out if not found.
    [string]$YtDlp = "",
    # Only the folder, without the zip.
    [switch]$NoZip
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $root

function Say($text) {
    Write-Host "== $text" -ForegroundColor Cyan
}

# A program itself, not a launcher that starts it from somewhere else: Chocolatey's and Scoop's
# launchers are small, and copied alone they start nothing.
function Test-Program($path) {
    return $path -and (Test-Path -LiteralPath $path -PathType Leaf) -and ((Get-Item -LiteralPath $path).Length -gt 5MB)
}

# Folders that match a wildcard, if any.
function Get-Folders($pattern) {
    return @(Get-Item -Path $pattern -ErrorAction SilentlyContinue | Where-Object { $_.PSIsContainer } | ForEach-Object { $_.FullName })
}

function Find-FFmpegFolder {
    if ($FFmpegDir) {
        if ((Test-Program (Join-Path $FFmpegDir "ffmpeg.exe")) -and (Test-Program (Join-Path $FFmpegDir "ffprobe.exe"))) {
            return $FFmpegDir
        }
        throw "no ffmpeg.exe and ffprobe.exe in $FFmpegDir"
    }
    $folders = @()
    $onPath = Get-Command ffmpeg.exe -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($onPath) {
        $folders += Split-Path -Parent $onPath.Source
    }
    # Where the usual ways of installing it keep the program itself.
    $choco = if ($env:ChocolateyInstall) { $env:ChocolateyInstall } else { Join-Path $env:ProgramData "chocolatey" }
    $folders += Get-Folders (Join-Path $choco "lib\ffmpeg*\tools\*\bin")
    $folders += Get-Folders (Join-Path $env:LOCALAPPDATA "Microsoft\WinGet\Packages\Gyan.FFmpeg*\*\bin")
    $folders += Get-Folders (Join-Path $env:USERPROFILE "scoop\apps\ffmpeg\current\bin")
    foreach ($folder in $folders) {
        if ((Test-Program (Join-Path $folder "ffmpeg.exe")) -and (Test-Program (Join-Path $folder "ffprobe.exe"))) {
            return $folder
        }
    }
    throw "ffmpeg was not found. Install it (winget install Gyan.FFmpeg) or say where it is: -FFmpegDir <folder with ffmpeg.exe>"
}

function Find-YtDlp {
    if ($YtDlp) {
        if (Test-Program $YtDlp) {
            return $YtDlp
        }
        throw "$YtDlp is not yt-dlp.exe"
    }
    $files = @()
    $onPath = Get-Command yt-dlp.exe -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($onPath) {
        $files += $onPath.Source
        # WinGet puts a link on the PATH: what it points to is the program.
        $target = (Get-Item -LiteralPath $onPath.Source).Target
        if ($target) {
            $files += @($target)[0]
        }
    }
    $choco = if ($env:ChocolateyInstall) { $env:ChocolateyInstall } else { Join-Path $env:ProgramData "chocolatey" }
    $files += @(Get-Item -Path (Join-Path $env:LOCALAPPDATA "Microsoft\WinGet\Packages\yt-dlp.yt-dlp*\yt-dlp.exe") -ErrorAction SilentlyContinue | ForEach-Object { $_.FullName })
    $files += Join-Path $choco "lib\yt-dlp\tools\yt-dlp.exe"
    $files += Join-Path $env:USERPROFILE "scoop\apps\yt-dlp\current\yt-dlp.exe"
    foreach ($file in $files) {
        if (Test-Program $file) {
            return $file
        }
    }
    return $null
}

# The licenses of what is built into the programs, in one file: every library as cargo names it,
# with its license and where its source is, and the license texts it brings in its own folder (the
# fonts of the interface are among them).
function Write-ThirdPartyLicenses($file) {
    $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE ".cargo" }
    $registries = @(Get-ChildItem -LiteralPath (Join-Path $cargoHome "registry\src") -Directory -ErrorAction SilentlyContinue)
    $lines = & cargo tree --workspace --prefix none -e normal --target x86_64-pc-windows-msvc --format "{p}|{l}|{r}"
    if ($LASTEXITCODE -ne 0) {
        throw "cargo tree failed"
    }
    $text = New-Object System.Text.StringBuilder
    [void]$text.AppendLine("Third-party software in DicingSplicing")
    [void]$text.AppendLine("======================================")
    [void]$text.AppendLine("")
    [void]$text.AppendLine("DicingSplicing itself is under the Apache License 2.0: see LICENSE and NOTICE.")
    [void]$text.AppendLine("")
    [void]$text.AppendLine("Programs that come with it in this folder, each under its own license:")
    [void]$text.AppendLine("")
    [void]$text.AppendLine("  ffmpeg.exe, ffprobe.exe")
    [void]$text.AppendLine("      FFmpeg, https://ffmpeg.org. Its license is in ffmpeg-LICENSE.txt; ffmpeg-README.txt")
    [void]$text.AppendLine("      names the build and the source it was made from.")
    [void]$text.AppendLine("  yt-dlp.exe")
    [void]$text.AppendLine("      yt-dlp, https://github.com/yt-dlp/yt-dlp. Its own code is under The Unlicense; the")
    [void]$text.AppendLine("      program as released also holds code of other projects, and as a whole is under the")
    [void]$text.AppendLine("      GNU GPL version 3 or later. Its source and the licenses of its parts are at that address.")
    [void]$text.AppendLine("")
    [void]$text.AppendLine("Libraries built into dicing_splicing.exe and its plugins follow, with the license each one")
    [void]$text.AppendLine("names and where its source is. Where a library brings the text of its license, it is given.")
    [void]$text.AppendLine("")
    $count = 0
    # A library met again in the tree is marked with a star: the same library, once.
    $lines = $lines | ForEach-Object { $_ -replace ' \(\*\)$', '' } | Sort-Object -Unique
    foreach ($line in $lines) {
        # Not the editor's own parts (they have a folder after the version), and not what only
        # helps to build it.
        if (-not $line -or $line -match '\((proc-macro|[A-Za-z]:\\|/)') {
            continue
        }
        $parts = $line.Split('|')
        if ($parts[0] -notmatch '^(\S+) v(\S+)') {
            continue
        }
        $library = $Matches[1]
        $libraryVersion = $Matches[2]
        $count += 1
        [void]$text.AppendLine(("-" * 78))
        [void]$text.AppendLine("$library $libraryVersion")
        [void]$text.AppendLine("License: $($parts[1])")
        if ($parts[2]) {
            [void]$text.AppendLine("Source:  $($parts[2])")
        }
        $folder = $registries | ForEach-Object { Join-Path $_.FullName "$library-$libraryVersion" } | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
        if (-not $folder) {
            continue
        }
        $texts = @(Get-ChildItem -LiteralPath $folder -File | Where-Object { $_.Name -match '^(LICEN[SC]E|COPYING|NOTICE|UNLICENSE|COPYRIGHT)' })
        $texts += @(Get-ChildItem -LiteralPath $folder -Directory | ForEach-Object { Get-ChildItem -LiteralPath $_.FullName -File } |
            Where-Object { $_.Name -match '(licen[sc]e|^OFL|^UFL)' -or ($_.Directory.Name -eq 'fonts' -and $_.Extension -eq '.txt') })
        foreach ($license in ($texts | Where-Object { $_.Length -lt 200KB } | Sort-Object FullName)) {
            [void]$text.AppendLine("")
            [void]$text.AppendLine("--- $($license.FullName.Substring($folder.Length + 1)) ---")
            [void]$text.AppendLine([IO.File]::ReadAllText($license.FullName).TrimEnd())
        }
        [void]$text.AppendLine("")
    }
    [IO.File]::WriteAllText($file, $text.ToString(), (New-Object System.Text.UTF8Encoding($false)))
    return $count
}

$version = (Select-String -LiteralPath (Join-Path $root "Cargo.toml") -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
$name = "DicingSplicing-$version"
$dist = Join-Path $root "dist"
$out = Join-Path $dist $name

# Before the long build: what has to come along is there.
$ffmpeg = Find-FFmpegFolder
$ytdlp = Find-YtDlp
Say "ffmpeg from $ffmpeg"
if ($ytdlp) {
    Say "yt-dlp from $ytdlp"
} else {
    Write-Warning "yt-dlp was not found: links will work only where it is installed (winget install yt-dlp.yt-dlp)"
}

# A target folder of its own: these flags would otherwise rebuild everything on every switch with
# an ordinary release build. The C runtime goes inside the programs, so that a computer without the
# Visual C++ Redistributable runs them too.
Say "Building $name"
$target = Join-Path $root "target\package"
& cargo build --release --workspace --target-dir $target --config "target.x86_64-pc-windows-msvc.rustflags=['-C','target-feature=+crt-static']"
if ($LASTEXITCODE -ne 0) {
    throw "the build failed"
}
$bin = Join-Path $target "release"

Say "Gathering into $out"
if (Test-Path -LiteralPath $out) {
    Remove-Item -LiteralPath $out -Recurse -Force
}
New-Item -ItemType Directory -Path $out | Out-Null
foreach ($program in "dicing_splicing.exe", "dicing_splicing-link.exe", "dicing_splicing-telegram.exe") {
    Copy-Item -LiteralPath (Join-Path $bin $program) -Destination $out
}
# Names and lines for the editor's error reports.
$symbols = Join-Path $bin "dicing_splicing.pdb"
if (Test-Path -LiteralPath $symbols) {
    Copy-Item -LiteralPath $symbols -Destination $out
}
foreach ($program in "ffmpeg.exe", "ffprobe.exe") {
    Copy-Item -LiteralPath (Join-Path $ffmpeg $program) -Destination $out
}
# ffmpeg's license and notes, from the folder above its programs.
foreach ($file in "LICENSE", "LICENSE.txt", "README.txt") {
    $source = Join-Path (Split-Path -Parent $ffmpeg) $file
    if (Test-Path -LiteralPath $source -PathType Leaf) {
        Copy-Item -LiteralPath $source -Destination (Join-Path $out ("ffmpeg-" + [IO.Path]::GetFileNameWithoutExtension($file) + ".txt"))
    }
}
if ($ytdlp) {
    # Through a link, to the program it points to.
    [IO.File]::Copy($ytdlp, (Join-Path $out "yt-dlp.exe"))
}
Copy-Item -LiteralPath (Join-Path $PSScriptRoot "README-dist.txt") -Destination (Join-Path $out "README.txt")
# The editor's own license, and those of everything built into it.
foreach ($file in "LICENSE", "NOTICE") {
    Copy-Item -LiteralPath (Join-Path $root $file) -Destination (Join-Path $out "$file.txt")
}
$libraries = Write-ThirdPartyLicenses (Join-Path $out "THIRD-PARTY-LICENSES.txt")
Say "Licenses of $libraries libraries written"

# Nothing secret goes out.
$secrets = @(Get-ChildItem -LiteralPath $out -Recurse -File | Where-Object { $_.Name -in @("credentials.json", "config.json") })
if ($secrets.Count -gt 0) {
    Remove-Item -LiteralPath $out -Recurse -Force
    throw "a file with secrets got into the folder, so it was removed: $($secrets.Name -join ', ')"
}

# Each program starts: the plugins say what they are, ffmpeg says its version.
Say "Checking"
foreach ($plugin in "dicing_splicing-link.exe", "dicing_splicing-telegram.exe") {
    $description = & (Join-Path $out $plugin) --describe | Out-String
    if ($LASTEXITCODE -ne 0 -or -not ($description | ConvertFrom-Json).protocol) {
        throw "$plugin does not describe itself"
    }
}
# All of its output first: cutting the pipe after one line would stop it with an error.
$ffmpegOutput = & (Join-Path $out "ffmpeg.exe") -hide_banner -version
if ($LASTEXITCODE -ne 0) {
    throw "ffmpeg.exe does not start"
}
Write-Host "   $(@($ffmpegOutput)[0])"

if (-not $NoZip) {
    $zip = Join-Path $dist "$name.zip"
    if (Test-Path -LiteralPath $zip) {
        Remove-Item -LiteralPath $zip -Force
    }
    Say "Zipping"
    Compress-Archive -LiteralPath $out -DestinationPath $zip -CompressionLevel Optimal
}

Say "Done"
Get-ChildItem -LiteralPath $out -File | ForEach-Object { "   {0,-28} {1,8:N1} MB" -f $_.Name, ($_.Length / 1MB) }
if (-not $NoZip) {
    "   {0,-28} {1,8:N1} MB" -f "$name.zip", ((Get-Item -LiteralPath $zip).Length / 1MB)
}
