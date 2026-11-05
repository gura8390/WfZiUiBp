[CmdletBinding()]
param(
    [string]$PayloadRoot,
    [string]$CompilerZip,
    [string]$SevenZip,
    [string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$project = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$work = Join-Path $project 'verification\work'
if (-not $PayloadRoot) { $PayloadRoot = Join-Path $project 'dist\WfZiUiBp' }
if (-not $CompilerZip) { $CompilerZip = Join-Path $work 'installer-tools\nsis-3.12.zip' }
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $project 'dist' }
if (-not $SevenZip) {
    $sevenCommand = Get-Command '7z.exe' -CommandType Application -ErrorAction SilentlyContinue
    $SevenZip = if ($sevenCommand) { $sevenCommand.Source } else {
        Join-Path $env:ProgramFiles '7-Zip\7z.exe'
    }
}

$payload = (Resolve-Path -LiteralPath $PayloadRoot).Path.TrimEnd('\')
$compilerArchive = (Resolve-Path -LiteralPath $CompilerZip).Path
$SevenZip = (Resolve-Path -LiteralPath $SevenZip).Path
$compilerDigest = '56581f90db321581c5381193d796fffcf2d24b2f8fed2160a6c6a3baa67f2c4f'
if ((Get-FileHash -LiteralPath $compilerArchive -Algorithm SHA256).Hash -ne $compilerDigest) {
    throw 'NSIS compiler archive differs from the official pinned NSIS 3.12 ZIP.'
}

$versionText = Get-Content -LiteralPath (Join-Path $project 'Cargo.toml') -Raw
$versionMatch = [regex]::Match($versionText, '(?m)^version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"')
if (-not $versionMatch.Success) { throw 'Cannot read the package version from Cargo.toml.' }
$version = $versionMatch.Groups[1].Value
$setupName = "WfZiUiBp-$version-setup-x64.exe"
$null = New-Item -ItemType Directory -Path $OutputDirectory -Force
$output = Get-Item -LiteralPath $OutputDirectory
if ($output.Attributes -band [IO.FileAttributes]::ReparsePoint) {
    throw 'Installer output directory cannot be a reparse point.'
}
$setup = Join-Path $output.FullName $setupName
if ((Test-Path -LiteralPath $setup) -or (Test-Path -LiteralPath "$setup.sha256")) {
    throw "Installer output already exists. Move the previous output first: $setup"
}

$payloadDirectory = Get-Item -LiteralPath $payload
$items = @(Get-ChildItem -LiteralPath $payload -Recurse -Force)
if (($payloadDirectory.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
    ($items | Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint })) {
    throw 'The portable payload cannot contain symbolic links or junctions.'
}

$expected = @{}
$checksumFile = Join-Path $payload 'SHA256SUMS.txt'
foreach ($line in Get-Content -LiteralPath $checksumFile -Encoding utf8) {
    if ($line -notmatch '^([A-Fa-f0-9]{64})  (.+)$') { throw 'Invalid portable checksum entry.' }
    $relative = $Matches[2].Replace('/', '\')
    if ([IO.Path]::IsPathRooted($relative) -or
        ($relative.Split('\') | Where-Object { $_ -eq '.' -or $_ -eq '..' -or -not $_ }) -or
        $relative -match '["$:\x00-\x1f]' -or
        $expected.ContainsKey($relative)) {
        throw "Invalid portable checksum path: $relative"
    }
    $expected[$relative] = $Matches[1]
}

if (-not $expected.ContainsKey('WfZiUiBp.exe') -or $expected.ContainsKey('SHA256SUMS.txt') -or
    $expected.ContainsKey('Uninstall.exe')) {
    throw 'Unexpected application, checksum or uninstaller entries in the portable payload.'
}
$expected['SHA256SUMS.txt'] = (Get-FileHash -LiteralPath $checksumFile -Algorithm SHA256).Hash
$files = @($items | Where-Object { -not $_.PSIsContainer } | Sort-Object FullName)
if ($files.Count -ne $expected.Count) {
    throw 'The portable payload and its SHA256SUMS.txt contain different file counts.'
}
foreach ($file in $files) {
    $relative = $file.FullName.Substring($payload.Length + 1)
    if (-not $expected.ContainsKey($relative) -or
        (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash -ne $expected[$relative]) {
        throw "Portable payload checksum mismatch: $relative"
    }
}

$app = Join-Path $payload 'WfZiUiBp.exe'
$pe = [IO.BinaryReader]::new([IO.File]::OpenRead($app))
try {
    if ($pe.ReadUInt16() -ne 0x5a4d) { throw 'The portable executable is not a Windows PE.' }
    $pe.BaseStream.Position = 0x3c
    $pe.BaseStream.Position = $pe.ReadUInt32()
    if ($pe.ReadUInt32() -ne 0x4550 -or $pe.ReadUInt16() -ne 0x8664) {
        throw 'The portable executable is not Windows x64.'
    }
} finally {
    $pe.Dispose()
}

$stageRoot = Join-Path $work 'installer-build'
$null = New-Item -ItemType Directory -Path $stageRoot -Force
$stageParent = Get-Item -LiteralPath $stageRoot
if ($stageParent.Attributes -band [IO.FileAttributes]::ReparsePoint) {
    throw 'Installer working directory cannot be a reparse point.'
}
$stage = Join-Path $stageParent.FullName ([guid]::NewGuid().ToString('N'))
if ($stage -match '["$]' -or $project -match '["$]' -or $payload -match '["$]') {
    throw 'NSIS build paths cannot contain quotes or dollar signs.'
}
$null = New-Item -ItemType Directory -Path $stage

try {
    $tools = Join-Path $stage 'tools'
    Expand-Archive -LiteralPath $compilerArchive -DestinationPath $tools
    $compiler = Join-Path $tools 'nsis-3.12\makensis.exe'
    $compilerVersion = (& $compiler /VERSION | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $compilerVersion -ne 'v3.12') {
        throw "Unexpected NSIS compiler version: $compilerVersion"
    }

    $install = [Collections.Generic.List[string]]::new()
    $remove = [Collections.Generic.List[string]]::new()
    $guard = [Collections.Generic.List[string]]::new()
    $directories = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $lastDirectory = $null
    foreach ($file in $files) {
        $relative = $file.FullName.Substring($payload.Length + 1)
        $directory = [IO.Path]::GetDirectoryName($relative)
        if ($directory -cne $lastDirectory) {
            $target = if ($directory) { '${Destination}\' + $directory } else { '${Destination}' }
            $install.Add('    SetOutPath "' + $target + '"')
            $lastDirectory = $directory
        }
        $install.Add('    File "' + $file.FullName + '"')
        $remove.Add('        Delete "${Destination}\' + $relative + '"')
        while ($directory) {
            $null = $directories.Add($directory)
            $directory = [IO.Path]::GetDirectoryName($directory)
        }
    }
    $guard.Add('    StrCpy $R0 1')
    foreach ($directory in $directories | Sort-Object) {
        $guard.Add('    System::Call ''kernel32::GetFileAttributesW(w "${Destination}\' +
            $directory + '") i .r0''')
        $guard.Add('    ${If} $0 != -1')
        $guard.Add('        IntOp $0 $0 & 0x400')
        $guard.Add('        ${If} $0 != 0')
        $guard.Add('            StrCpy $R0 0')
        $guard.Add('        ${EndIf}')
        $guard.Add('    ${EndIf}')
    }
    foreach ($directory in $directories | Sort-Object { $_.Length } -Descending) {
        $remove.Add('        RMDir "${Destination}\' + $directory + '"')
    }
    $payloadSize = [int][math]::Ceiling(($files | Measure-Object -Property Length -Sum).Sum / 1KB)
    $include = Join-Path $stage 'payload.nsh'
    $lines = @(
        "!define PAYLOAD_KIB $payloadSize"
        '!macro InstallPayload Destination'
        $install
        '!macroend'
        '!macro RemovePayload Destination'
        $guard
        '    ${If} $R0 == 1'
        $remove
        '    ${EndIf}'
        '!macroend'
    )
    [IO.File]::WriteAllLines($include, [string[]]$lines, [Text.UTF8Encoding]::new($true))
    $stagedSetup = Join-Path $stage $setupName
    $definition = Join-Path $project 'installer\WfZiUiBp.nsi'
    $icon = Join-Path $project 'assets\umiocr.ico'
    & $compiler /V3 /WX /INPUTCHARSET UTF8 "/DAPP_VERSION=$version" "/DAPP_ICON=$icon" `
        "/DPAYLOAD_INCLUDE=$include" "/DINSTALLER_OUTPUT=$stagedSetup" $definition
    if ($LASTEXITCODE -ne 0) { throw "NSIS compilation failed with exit code $LASTEXITCODE." }

    # 7-Zip reads and extracts the NSIS data archive without running the installer.
    $extract = Join-Path $stage 'static-extract'
    & $SevenZip x -y -bd -bso0 -bsp0 "-o$extract" $stagedSetup
    if ($LASTEXITCODE -ne 0) { throw "Static NSIS extraction failed with exit code $LASTEXITCODE." }
    $embedded = @{}
    foreach ($file in Get-ChildItem -LiteralPath $extract -Recurse -File) {
        $relative = $file.FullName.Substring($extract.Length + 1)
        if ($relative.StartsWith('$PLUGINSDIR\', [StringComparison]::OrdinalIgnoreCase)) { continue }
        # 7-Zip exposes the NSIS staging variable as a synthetic root directory.
        if ($relative -match '^\$_[0-9]+_\\(.+)$') { $relative = $Matches[1] }
        if ($embedded.ContainsKey($relative)) {
            throw "Duplicate static NSIS payload path: $relative"
        }
        $embedded[$relative] = $file.FullName
    }
    if ($embedded.Count -ne $expected.Count) {
        throw "NSIS payload count differs from portable source: $($embedded.Count) vs $($expected.Count)."
    }
    foreach ($relative in $expected.Keys) {
        if (-not $embedded.ContainsKey($relative) -or
            (Get-FileHash -LiteralPath $embedded[$relative] -Algorithm SHA256).Hash -ne $expected[$relative]) {
            throw "Static NSIS payload checksum mismatch: $relative"
        }
    }

    $sha = (Get-FileHash -LiteralPath $stagedSetup -Algorithm SHA256).Hash.ToLowerInvariant()
    $stagedHash = "$stagedSetup.sha256"
    "$sha  $setupName" | Set-Content -LiteralPath $stagedHash -Encoding ascii
    [IO.File]::Move($stagedSetup, $setup)
    [IO.File]::Move($stagedHash, "$setup.sha256")
    Write-Output "Setup: $setup"
    Write-Output "SHA256: $sha"
    Write-Output "Static portable payload matches: $($expected.Count) files"
} finally {
    $resolved = Get-Item -LiteralPath $stage -ErrorAction SilentlyContinue
    if ($resolved -and
        $resolved.FullName.StartsWith($stageParent.FullName + '\', [StringComparison]::OrdinalIgnoreCase) -and
        -not ($resolved.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        Remove-Item -LiteralPath $resolved.FullName -Recurse -Force
    }
}
