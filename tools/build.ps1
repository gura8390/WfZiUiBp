[CmdletBinding()]
param(
    [string]$VisualStudioPath,
    [string]$SdkLibraryRoot,
    [string]$PackageDirectory,
    [switch]$RemapBuildPaths,
    [switch]$Package,
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if ($SkipBuild -and -not $Package) {
    throw '-SkipBuild requires -Package.'
}
if ($PackageDirectory -and -not $Package) {
    throw '-PackageDirectory requires -Package.'
}
if ($SkipBuild -and $RemapBuildPaths) {
    throw '-RemapBuildPaths requires a build.'
}

$projectRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
$savedPath = $env:PATH
$savedLib = $env:LIB
$savedRustFlags = $env:CARGO_ENCODED_RUSTFLAGS

if (-not $VisualStudioPath) {
    $programFilesX86 = [Environment]::GetFolderPath('ProgramFilesX86')
    $vswhere = Join-Path $programFilesX86 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path -LiteralPath $vswhere -PathType Leaf) {
        $VisualStudioPath = & $vswhere -latest -products '*' `
            -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath |
            Select-Object -First 1
    }
    if (-not $VisualStudioPath -and $env:VCINSTALLDIR) {
        $VisualStudioPath = Split-Path -Parent (Resolve-Path -LiteralPath $env:VCINSTALLDIR).Path
    }
    if (-not $VisualStudioPath -and (Test-Path -LiteralPath 'C:\BuildTools\VC\Auxiliary\Build\vcvars64.bat')) {
        $VisualStudioPath = 'C:\BuildTools'
    }
}

if (-not $VisualStudioPath) {
    throw 'Visual Studio C++ Build Tools were not found. Pass -VisualStudioPath to its installation root.'
}
$VisualStudioPath = (Resolve-Path -LiteralPath $VisualStudioPath).Path
$vcvars = Join-Path $VisualStudioPath 'VC\Auxiliary\Build\vcvars64.bat'

if (-not $SkipBuild) {
    if (-not (Test-Path -LiteralPath $vcvars -PathType Leaf)) {
        throw "Missing MSVC x64 environment script $vcvars."
    }
    if (-not $SdkLibraryRoot) {
        $localSdk = Join-Path $projectRoot 'verification\work\sdk\x64\c'
        if ((Test-Path -LiteralPath (Join-Path $localSdk 'um\x64\kernel32.lib')) -and
            (Test-Path -LiteralPath (Join-Path $localSdk 'ucrt\x64\ucrt.lib'))) {
            $SdkLibraryRoot = $localSdk
        }
    }

    Push-Location -LiteralPath $projectRoot
    try {
        if (Test-Path -LiteralPath (Join-Path $cargoBin 'cargo.exe') -PathType Leaf) {
            $env:PATH = "$cargoBin;$savedPath"
        }
        if (-not (Get-Command cargo -CommandType Application -ErrorAction SilentlyContinue)) {
            throw 'Rust cargo was not found. Install the x86_64-pc-windows-msvc toolchain.'
        }
        if ($SdkLibraryRoot) {
            $sdk = (Resolve-Path -LiteralPath $SdkLibraryRoot).Path
            if (-not (Test-Path -LiteralPath (Join-Path $sdk 'um\x64\kernel32.lib')) -or
                -not (Test-Path -LiteralPath (Join-Path $sdk 'ucrt\x64\ucrt.lib'))) {
                throw 'The SDK library root must contain um\x64\kernel32.lib and ucrt\x64\ucrt.lib.'
            }
            $env:LIB = "$sdk\um\x64;$sdk\ucrt\x64;$savedLib"
        }
        if ($RemapBuildPaths) {
            if ($env:RUSTFLAGS -and -not $savedRustFlags) {
                throw 'Pass custom compiler flags via CARGO_ENCODED_RUSTFLAGS when remapping build paths.'
            }
            $separator = [char]0x1f
            $flags = @()
            if ($savedRustFlags) {
                $flags += $savedRustFlags -split $separator
            }
            $roots = @(
                @{ Path = $projectRoot; Label = '/build/WfZiUiBp' },
                @{ Path = $env:USERPROFILE; Label = '/build/user' }
            )
            foreach ($root in $roots) {
                $spellings = @($root.Path, $root.Path.Replace('\', '/'), ('\\?\' + $root.Path))
                foreach ($spelling in $spellings) {
                    $flags += "--remap-path-prefix=$spelling=$($root.Label)"
                }
            }
            $env:CARGO_ENCODED_RUSTFLAGS = $flags -join $separator
        }
        $command = 'call "' + $vcvars + '" && cargo test --release --locked --target-dir target && cargo build --release --locked --target-dir target'
        & $env:ComSpec /d /c $command
        if ($LASTEXITCODE -ne 0) {
            throw "Release build or tests failed with exit code $LASTEXITCODE."
        }
    } finally {
        $env:PATH = $savedPath
        $env:LIB = $savedLib
        $env:CARGO_ENCODED_RUSTFLAGS = $savedRustFlags
        Pop-Location
    }
}

if (-not $Package) {
    return
}

$executable = Join-Path $projectRoot 'target\release\wf-zi-ui-bp.exe'
if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
    throw "The x64 release executable was not found at $executable."
}

$models = Join-Path $projectRoot 'models'
$requiredModels = @(
    'det.onnx', 'cls.onnx', 'det_v6_tiny.onnx', 'det_v6_tiny.yml', 'rec_v6_tiny.yml',
    'onnxruntime.dll', 'onnxruntime_providers_shared.dll',
    'model-provenance.json', 'runtime-provenance.json', 'v6-provenance.json',
    'ONNX-Runtime-LICENSE.txt', 'ONNX-Runtime-ThirdPartyNotices.txt', 'PaddleOCR-LICENSE.txt'
)
foreach ($profile in @('chinese', 'chinese_cht', 'cyrillic', 'en', 'japan', 'korean', 'v6_tiny')) {
    $requiredModels += "rec_$profile.onnx", "dict_$profile.txt"
}
foreach ($asset in $requiredModels) {
    if (-not (Test-Path -LiteralPath (Join-Path $models $asset) -PathType Leaf)) {
        throw "Missing runtime asset $asset."
    }
}
foreach ($provenanceFile in @('model-provenance.json', 'v6-provenance.json', 'runtime-provenance.json')) {
    $provenance = Get-Content -LiteralPath (Join-Path $models $provenanceFile) -Raw | ConvertFrom-Json
    $runtimeList = $provenanceFile -eq 'runtime-provenance.json'
    $files = if ($runtimeList) { $provenance.files } else { $provenance.assets }
    foreach ($file in $files.PSObject.Properties) {
        if ($file.Name -notin $requiredModels) {
            throw "Unexpected asset $($file.Name) in $provenanceFile."
        }
        $expected = if ($runtimeList) { $file.Value } else { $file.Value.sha256 }
        $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $models $file.Name)).Hash
        if ($actual -ne $expected) {
            throw "SHA256 mismatch for $($file.Name) against $provenanceFile."
        }
    }
}

# Use only the app-local redistributable files shipped with the selected C++ tools.
$crtNames = @('msvcp140.dll', 'msvcp140_1.dll', 'vcruntime140.dll', 'vcruntime140_1.dll')
$redist = Join-Path $VisualStudioPath 'VC\Redist\MSVC'
$crtFolder = Get-ChildItem -LiteralPath $redist -Directory |
    Sort-Object -Property Name -Descending |
    ForEach-Object { Join-Path $_.FullName 'x64\Microsoft.VC143.CRT' } |
    Where-Object { Test-Path -LiteralPath (Join-Path $_ 'msvcp140.dll') -PathType Leaf } |
    Select-Object -First 1
if (-not $crtFolder) {
    throw "The x64 Visual C++ redistributable directory was not found under $redist."
}
foreach ($runtime in $crtNames) {
    if (-not (Test-Path -LiteralPath (Join-Path $crtFolder $runtime) -PathType Leaf)) {
        throw "Missing app-local Microsoft runtime file $runtime."
    }
}

$manifest = Get-Content -LiteralPath (Join-Path $projectRoot 'Cargo.toml') -Raw
$match = [regex]::Match($manifest, '(?m)^version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"')
if (-not $match.Success) {
    throw 'The package version could not be read from Cargo.toml.'
}
$archiveName = "WfZiUiBp-$($match.Groups[1].Value)-win-x64.zip"
$dist = if (-not $PackageDirectory) {
    Join-Path $projectRoot 'dist'
} elseif ([IO.Path]::IsPathRooted($PackageDirectory)) {
    [IO.Path]::GetFullPath($PackageDirectory)
} else {
    [IO.Path]::GetFullPath((Join-Path $projectRoot $PackageDirectory))
}
$null = New-Item -ItemType Directory -Path $dist -Force
$distDirectory = Get-Item -LiteralPath $dist
if ($distDirectory.Attributes -band [IO.FileAttributes]::ReparsePoint) {
    throw "The package output directory cannot be a junction or symbolic link: $dist."
}
$dist = $distDirectory.FullName
$portable = Join-Path $dist 'WfZiUiBp'
$archive = Join-Path $dist $archiveName
$archiveHash = "$archive.sha256"
foreach ($outputPath in @($portable, $archive, $archiveHash)) {
    if (Test-Path -LiteralPath $outputPath) {
        throw "An existing package output is preserved at $outputPath. Move it before repackaging."
    }
}

$stage = Join-Path $dist ('.package-' + [guid]::NewGuid().ToString('N'))
$stagePortable = Join-Path $stage 'WfZiUiBp'
$stageModels = Join-Path $stagePortable 'models'
$stageArchive = Join-Path $stage $archiveName
$stageHash = "$stageArchive.sha256"
$null = New-Item -ItemType Directory -Path $stageModels
try {
    Copy-Item -LiteralPath $executable -Destination (Join-Path $stagePortable 'WfZiUiBp.exe')
    foreach ($asset in $requiredModels) {
        Copy-Item -LiteralPath (Join-Path $models $asset) -Destination $stageModels
    }
    foreach ($runtime in $crtNames) {
        Copy-Item -LiteralPath (Join-Path $crtFolder $runtime) -Destination $stagePortable
    }
    foreach ($document in @('README.md', 'Cargo.lock', 'LICENSE-UMI-OCR.txt', 'NOTICE.md', 'THIRD-PARTY-RUST.txt')) {
        $source = Join-Path $projectRoot $document
        if (Test-Path -LiteralPath $source -PathType Leaf) {
            Copy-Item -LiteralPath $source -Destination $stagePortable
        }
    }
    $docs = Join-Path $stagePortable 'docs'
    $null = New-Item -ItemType Directory -Path $docs
    foreach ($document in @('性能对照.md', '验证范围.md')) {
        $source = Join-Path (Join-Path $projectRoot 'docs') $document
        if (Test-Path -LiteralPath $source -PathType Leaf) {
            Copy-Item -LiteralPath $source -Destination $docs
        }
    }
    $installerReadme = Join-Path $projectRoot 'installer\README.md'
    if (Test-Path -LiteralPath $installerReadme -PathType Leaf) {
        $installerDocs = Join-Path $stagePortable 'installer'
        $null = New-Item -ItemType Directory -Path $installerDocs
        Copy-Item -LiteralPath $installerReadme -Destination $installerDocs
    }
    $evidence = Join-Path $stagePortable 'verification'
    $null = New-Item -ItemType Directory -Path $evidence
    foreach ($record in @('benchmark-v020-release-4threads.json', 'benchmark-v020-release-16threads.json', 'desktop-idle-v020-release.json', 'a11y-release.json', 'http-release.json')) {
        $source = Join-Path (Join-Path $projectRoot 'verification') $record
        if (Test-Path -LiteralPath $source -PathType Leaf) {
            Copy-Item -LiteralPath $source -Destination $evidence
        }
    }

    $checksums = Get-ChildItem -LiteralPath $stagePortable -Recurse -File |
        Sort-Object -Property FullName |
        ForEach-Object {
            $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $_.FullName).Hash.ToLowerInvariant()
            $relative = $_.FullName.Substring($stagePortable.Length + 1).Replace('\', '/')
            "$hash  $relative"
        }
    [IO.File]::WriteAllLines((Join-Path $stagePortable 'SHA256SUMS.txt'), [string[]]$checksums, [Text.UTF8Encoding]::new($false))
    Compress-Archive -LiteralPath $stagePortable -DestinationPath $stageArchive
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $stageArchive).Hash.ToLowerInvariant()
    "$hash  $archiveName" | Set-Content -LiteralPath $stageHash -Encoding ascii

    [IO.Directory]::Move($stagePortable, $portable)
    [IO.File]::Move($stageArchive, $archive)
    [IO.File]::Move($stageHash, $archiveHash)
    Write-Output "Portable directory: $portable"
    Write-Output "Archive: $archive"
    Write-Output "SHA256: $hash"
} finally {
    $resolvedStage = Get-Item -LiteralPath $stage -ErrorAction SilentlyContinue
    if ($resolvedStage -and
        $resolvedStage.FullName.StartsWith($dist + '\', [StringComparison]::OrdinalIgnoreCase) -and
        -not ($resolvedStage.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        Remove-Item -LiteralPath $resolvedStage.FullName -Recurse -Force
    }
}
