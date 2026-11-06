#requires -version 5.1
[CmdletBinding()]
param(
    [string]$Executable,
    [string]$Output,
    [string]$Fixture,
    [switch]$RequireTextEditing,
    [switch]$VerifyTrayIdleCpu
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($PSVersionTable.PSEdition -ne 'Desktop') {
    throw 'Use Windows PowerShell 5.1 (powershell.exe -NoProfile -MTA -File).'
}

Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class WfZiUiBpQaWindow {
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    public static extern IntPtr FindWindow(string className, string windowName);
    [DllImport("user32.dll")]
    public static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);
    [DllImport("user32.dll")]
    public static extern bool IsWindowVisible(IntPtr window);
    [StructLayout(LayoutKind.Sequential)]
    private struct TrayIdentifier {
        public uint Size;
        public IntPtr Window;
        public uint Id;
        public Guid GuidItem;
    }
    [StructLayout(LayoutKind.Sequential)]
    private struct Bounds { public int Left, Top, Right, Bottom; }
    [DllImport("shell32.dll")]
    private static extern int Shell_NotifyIconGetRect(ref TrayIdentifier icon, out Bounds bounds);
    public static int[] GetTrayBounds(IntPtr window) {
        var icon = new TrayIdentifier { Size = (uint)Marshal.SizeOf(typeof(TrayIdentifier)), Window = window, Id = 1 };
        Bounds bounds;
        if (Shell_NotifyIconGetRect(ref icon, out bounds) != 0 || bounds.Right <= bounds.Left || bounds.Bottom <= bounds.Top) { return null; }
        return new[] { bounds.Left, bounds.Top, bounds.Right, bounds.Bottom };
    }
}
'@

$project = Split-Path -Parent $PSScriptRoot
if (-not $Executable) { $Executable = Join-Path $project 'target\release\wf-zi-ui-bp.exe' }
if (-not $Fixture) { $Fixture = Join-Path $project 'verification\fixtures\compact.png' }
$Executable = (Resolve-Path -LiteralPath $Executable).Path
$Fixture = (Resolve-Path -LiteralPath $Fixture).Path
$outputFile = if ($Output) { $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Output) } else { $null }

$script:Process = $null
$script:Checks = New-Object 'System.Collections.Generic.List[object]'
$script:WindowCondition = $null
$script:Report = [ordered]@{
    executable_sha256 = (Get-FileHash -LiteralPath $Executable -Algorithm SHA256).Hash.ToLowerInvariant()
    fixture_sha256 = (Get-FileHash -LiteralPath $Fixture -Algorithm SHA256).Hash.ToLowerInvariant()
    method = 'Windows UI Automation, process-bound controls, fresh profile, semantic patterns and app CLI only'
    checks = $script:Checks
    success = $false
}

function Add-Check([string]$Name, $Detail) {
    $script:Checks.Add([ordered]@{ name = $Name; detail = $Detail })
}

function Get-ControlWindowOwner {
    $handle = [WfZiUiBpQaWindow]::FindWindow('WfZiUiBp_Desktop', 'WfZiUiBp_Desktop')
    if ($handle -eq [IntPtr]::Zero) { return 0 }
    [uint32]$owner = 0
    [void][WfZiUiBpQaWindow]::GetWindowThreadProcessId($handle, [ref]$owner)
    return $owner
}

function Start-ToolProcess([string[]]$Arguments, [switch]$Hidden) {
    $command = New-Object System.Diagnostics.ProcessStartInfo
    $command.FileName = $Executable
    $command.WorkingDirectory = Split-Path -Parent $Executable
    $command.Arguments = ($Arguments | ForEach-Object { '"' + $_ + '"' }) -join ' '
    $command.UseShellExecute = $false
    $command.CreateNoWindow = $true
    if ($Hidden) { $command.WindowStyle = [System.Diagnostics.ProcessWindowStyle]::Hidden }
    return [System.Diagnostics.Process]::Start($command)
}

function Send-AppCommand([string[]]$Arguments) {
    if ((Get-ControlWindowOwner) -ne $script:Process.Id) {
        throw 'CLI control window does not belong to the isolated test process.'
    }
    $client = Start-ToolProcess -Arguments $Arguments -Hidden
    try {
        if (-not $client.WaitForExit(5000)) {
            $client.Kill()
            throw 'The isolated app CLI command did not return.'
        }
        if ($client.ExitCode -ne 0) { throw "The isolated app CLI returned $($client.ExitCode)." }
    } finally { $client.Dispose() }
}

function Get-AppWindow([switch]$IncludeHidden) {
    if ($null -eq $script:Process -or $script:Process.HasExited) { return }
    $desktop = [System.Windows.Automation.AutomationElement]::RootElement
    $windows = $desktop.FindAll([System.Windows.Automation.TreeScope]::Children, $script:WindowCondition)
    foreach ($window in $windows) {
        try {
            $current = $window.Current
            if ($current.Name -match '文字识别.*WfZiUiBp' -and ($IncludeHidden -or -not $current.IsOffscreen)) {
                return $window
            }
        } catch [System.Windows.Automation.ElementNotAvailableException] {}
    }
}

function Get-AppControls {
    $window = Get-AppWindow
    if ($null -eq $window) { return }
    $window.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
}

function Get-Pattern($Element, $Pattern) {
    $provider = $null
    if ($Element.TryGetCurrentPattern($Pattern, [ref]$provider)) { return $provider }
}

function Find-Control([string]$Name, [switch]$Actionable) {
    foreach ($element in Get-AppControls) {
        try {
            if ($element.Current.Name -notmatch $Name -or $element.Current.IsOffscreen) { continue }
            if (-not $Actionable) { return $element }
            if (-not $element.Current.IsEnabled) { continue }
            foreach ($pattern in @([System.Windows.Automation.InvokePattern]::Pattern, [System.Windows.Automation.SelectionItemPattern]::Pattern, [System.Windows.Automation.TogglePattern]::Pattern)) {
                if ($null -ne (Get-Pattern $element $pattern)) { return $element }
            }
        } catch [System.Windows.Automation.ElementNotAvailableException] {}
    }
}

function Wait-For([scriptblock]$Probe, [string]$Description, [int]$Seconds = 12) {
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    while ($clock.Elapsed.TotalSeconds -lt $Seconds) {
        if ($script:Process.HasExited) { throw "The isolated process exited while waiting for $Description." }
        try {
            $value = & $Probe
            if ($value) { return $value }
        } catch [System.Windows.Automation.ElementNotAvailableException] {}
        Start-Sleep -Milliseconds 80
    }
    throw "Timed out waiting for $Description."
}

function Invoke-Control([string]$Name) {
    $control = Wait-For { Find-Control $Name -Actionable } "an enabled semantic control matching $Name"
    $invoke = Get-Pattern $control ([System.Windows.Automation.InvokePattern]::Pattern)
    $select = Get-Pattern $control ([System.Windows.Automation.SelectionItemPattern]::Pattern)
    $toggle = Get-Pattern $control ([System.Windows.Automation.TogglePattern]::Pattern)
    $label = $control.Current.Name
    if ($null -ne $invoke) { $invoke.Invoke(); $pattern = 'Invoke' }
    elseif ($null -ne $select) { $select.Select(); $pattern = 'SelectionItem' }
    elseif ($null -ne $toggle) { $toggle.Toggle(); $pattern = 'Toggle' }
    else { throw "Control $label lost its semantic pattern." }
    Add-Check "activate $label" $pattern
}

function Open-Page([string]$Name) {
    Invoke-Control '^(新标签页|新建标签页（Ctrl\+T）)$'
    [void](Wait-For { Find-Control '^功能页$' } 'the feature chooser')
    Invoke-Control ('^' + [regex]::Escape($Name) + '$')
}

function Check-NamedControls([string]$Section, [string[]]$Names) {
    $found = foreach ($name in $Names) {
        $control = Wait-For { Find-Control ('^' + [regex]::Escape($name) + '$') } "$Section control $name"
        [ordered]@{ name = $control.Current.Name; type = $control.Current.ControlType.ProgrammaticName }
    }
    Add-Check $Section @($found)
}

function Find-TextControl([string]$Contents) {
    foreach ($control in Get-AppControls) {
        try {
            if ($control.Current.IsOffscreen -or $control.Current.ControlType -ne [System.Windows.Automation.ControlType]::Edit) { continue }
            $pattern = Get-Pattern $control ([System.Windows.Automation.TextPattern]::Pattern)
            if ($null -eq $pattern) { continue }
            $text = $pattern.DocumentRange.GetText(-1)
            if (($text -replace '\s', '') -match $Contents) { return $control }
        } catch [System.Windows.Automation.ElementNotAvailableException] {}
    }
}

function Find-ValueEditor([string]$Name) {
    foreach ($control in Get-AppControls) {
        try {
            if ($control.Current.IsOffscreen -or $control.Current.ControlType -ne [System.Windows.Automation.ControlType]::Edit) { continue }
            if ($Name -and $control.Current.Name -notmatch $Name) { continue }
            $pattern = Get-Pattern $control ([System.Windows.Automation.ValuePattern]::Pattern)
            if ($null -ne $pattern -and -not $pattern.Current.IsReadOnly) { return $control }
        } catch [System.Windows.Automation.ElementNotAvailableException] {}
    }
}

function Write-TestPdf([string]$Path) {
    $stream = "BT /F1 23 Tf 30 145 Td (WF A11Y PDF TEXT 20261106) Tj ET`n"
    $objects = @(
        '<< /Type /Catalog /Pages 2 0 R >>',
        '<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
        '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 440 240] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>',
        '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',
        "<< /Length $($stream.Length) >>`nstream`n${stream}endstream"
    )
    $pdf = [System.Text.StringBuilder]::new("%PDF-1.4`n")
    $offsets = foreach ($index in 1..$objects.Count) {
        $pdf.Length
        [void]$pdf.Append("$index 0 obj`n$($objects[$index - 1])`nendobj`n")
    }
    $xref = $pdf.Length
    [void]$pdf.Append("xref`n0 $($objects.Count + 1)`n0000000000 65535 f `n")
    foreach ($offset in $offsets) { [void]$pdf.Append(($offset.ToString('D10') + " 00000 n `n")) }
    [void]$pdf.Append("trailer`n<< /Size $($objects.Count + 1) /Root 1 0 R >>`nstartxref`n$xref`n%%EOF`n")
    [System.IO.File]::WriteAllText($Path, $pdf.ToString(), [System.Text.Encoding]::ASCII)
}

function Save-Report {
    $json = $script:Report | ConvertTo-Json -Depth 9
    if ($outputFile) {
        [void][System.IO.Directory]::CreateDirectory([System.IO.Path]::GetDirectoryName($outputFile))
        [System.IO.File]::WriteAllText($outputFile, "$json`r`n", [System.Text.UTF8Encoding]::new($false))
    }
    $json
}

$testDir = Join-Path ([System.IO.Path]::GetTempPath()) ('wfziuibp-a11y-' + [guid]::NewGuid().ToString('N'))
try {
    if ((Get-ControlWindowOwner) -ne 0) { throw 'A WfZiUiBp desktop instance is already running. Close it before this isolated test.' }
    [void][System.IO.Directory]::CreateDirectory($testDir)
    $profile = Join-Path $testDir 'profile.json'
    [System.IO.File]::WriteAllText($profile, '{"hotkey":false,"auto_copy":false,"tray":true,"window_top":false,"tabs_locked":false,"startup_hidden":false}')
    $image = Join-Path $testDir 'a11y-fixture.png'
    [System.IO.File]::Copy($Fixture, $image)
    $pdf = Join-Path $testDir 'a11y-fixture.pdf'
    Write-TestPdf $pdf
    $script:Process = Start-ToolProcess -Arguments @('--show', '--profile', $profile, '--no-hotkey')
    $script:WindowCondition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ProcessIdProperty, [int]$script:Process.Id)

    [void](Wait-For { Get-AppWindow } 'the initial main window' 25)
    foreach ($name in @('窗口置顶', '锁定标签页', '截图OCR')) {
        [void](Wait-For { Find-Control ('^' + [regex]::Escape($name) + '$') -Actionable } "the initial $name control")
    }
    $plus = Wait-For { Find-Control '^(新标签页|新建标签页（Ctrl\+T）)$' -Actionable } 'the add-tab control'
    Add-Check 'initial tab strip' @('窗口置顶', '截图OCR', $plus.Current.Name, '锁定标签页')

    Open-Page '批量OCR'
    [void](Wait-For { Find-Control '^拖入图片或文件夹$' } 'the empty image queue')
    Send-AppCommand -Arguments @($image)
    Invoke-Control '^a11y-fixture\.png([，,].*)?$'
    Invoke-Control '^(开始识别列表中的文件|开始任务)$'
    $text = Wait-For { Find-TextControl '离线文字识别' } 'a batch result exposed as editable text' 25
    $textPattern = Get-Pattern $text ([System.Windows.Automation.TextPattern]::Pattern)
    Add-Check 'batch OCR text exposed through TextPattern' $textPattern.DocumentRange.GetText(-1)
    $text.SetFocus()
    $textPattern.DocumentRange.Select()
    $selection = Wait-For {
        $current = Find-TextControl '离线文字识别'
        if ($null -ne $current) {
            $pattern = Get-Pattern $current ([System.Windows.Automation.TextPattern]::Pattern)
            foreach ($range in $pattern.GetSelection()) {
                $content = $range.GetText(-1)
                if (($content -replace '\s', '') -match '离线文字识别') { return $content }
            }
        }
    } 'the selected OCR result via TextPattern'
    Add-Check 'select OCR result through TextPattern' $selection

    $resultEditor = if ($RequireTextEditing) {
        Wait-For { Find-ValueEditor '^识别文字' } 'the named writable OCR result editor'
    } else { Find-ValueEditor '^识别文字' }
    if ($null -ne $resultEditor) {
        (Get-Pattern $resultEditor ([System.Windows.Automation.ValuePattern]::Pattern)).SetValue('WfZiUiBp 已编辑识别记录')
        [void](Wait-For { Find-TextControl 'WfZiUiBp已编辑识别记录' } 'the OCR history value changed via UI Automation')
        Add-Check 'edit OCR history through ValuePattern' 'set and read back'
    }

    Open-Page '批量文档'
    [void](Wait-For { Find-Control '^拖入文档或文件夹$' } 'the empty document queue')
    Check-NamedControls 'PDF queue accessibility labels' @('文档识别进度', '文档', '状态', '页数', '打开文档', '设置', '记录')
    Send-AppCommand -Arguments @($pdf)
    Invoke-Control '^a11y-fixture\.pdf([，,].*)?$'
    Invoke-Control '^(开始识别列表中的文件|开始任务)$'
    $pdfText = Wait-For { Find-TextControl 'WFA11YPDFTEXT20261106' } 'the synthetic PDF result' 25
    Add-Check 'batch PDF staged, opened and processed' (Get-Pattern $pdfText ([System.Windows.Automation.TextPattern]::Pattern)).DocumentRange.GetText(-1)

    Open-Page '二维码'
    Check-NamedControls 'QR settings accessibility labels' @('设置', '记录', '生成', '扫描二维码 / 条形码', '生成二维码 / 条形码', '码制')
    Invoke-Control '^记录$'
    Check-NamedControls 'QR history accessibility labels' @('记录', '暂无记录')
    Invoke-Control '^生成$'
    [void](Wait-For { Find-Control '^刷新$' -Actionable } 'the QR generation panel')
    Check-NamedControls 'QR generation accessibility labels' @('自动刷新', '刷新', '二维码内容')
    $editor = if ($RequireTextEditing) {
        Wait-For { Find-ValueEditor '二维码|生成' } 'the named writable QR content editor'
    } else { Find-ValueEditor '二维码|生成' }
    if ($null -ne $editor) {
        $value = Get-Pattern $editor ([System.Windows.Automation.ValuePattern]::Pattern)
        $value.SetValue('WfZiUiBp UIA 文字输入验证')
        [void](Wait-For { Find-TextControl 'WfZiUiBpUIA文字输入验证' } 'the QR value changed via UI Automation')
        Invoke-Control '^刷新$'
        [void](Wait-For { Find-Control '^二维码预览$' } 'the generated QR image exposed to UI Automation')
        Add-Check 'edit QR content through ValuePattern' 'set, read back, refresh and locate generated QR preview'
    } else {
        Add-Check 'edit QR content through ValuePattern' 'not tested because the provider did not expose a named writable editor'
    }

    Open-Page '全局设置'
    foreach ($name in @('开机自启', '界面和外观', '窗口置顶', '高级', '重置')) {
        [void](Wait-For { Find-Control ('^' + [regex]::Escape($name) + '$') } "the settings label $name")
    }
    Invoke-Control '^高级$'
    [void](Wait-For { Find-Control '^OCR 推理$' } 'the advanced OCR settings group')
    Add-Check 'global settings and advanced disclosure' 'visible control names confirmed'

    $window = Get-AppWindow
    if ($VerifyTrayIdleCpu) {
        $mainWindow = [IntPtr]::new([long]$window.Current.NativeWindowHandle)
        if ($mainWindow -eq [IntPtr]::Zero -or -not [WfZiUiBpQaWindow]::IsWindowVisible($mainWindow)) {
            throw 'No visible native main window to verify before closing to the tray.'
        }
    }
    $windowPattern = Get-Pattern $window ([System.Windows.Automation.WindowPattern]::Pattern)
    if ($null -ne $windowPattern) {
        $windowPattern.Close()
        $closePattern = 'WindowPattern.Close'
    } else {
        Invoke-Control '^(Close|关闭)$'
        $closePattern = 'native title bar button InvokePattern'
    }
    [void](Wait-For { (-not (Get-AppWindow)) -and (Get-ControlWindowOwner) -eq $script:Process.Id } 'the closed window retained in the background')
    $trayBounds = Wait-For {
        $handle = [WfZiUiBpQaWindow]::FindWindow('WfZiUiBp_Desktop', 'WfZiUiBp_Desktop')
        if ($handle -ne [IntPtr]::Zero -and (Get-ControlWindowOwner) -eq $script:Process.Id) { [WfZiUiBpQaWindow]::GetTrayBounds($handle) }
    } 'the isolated process tray icon registered in the Windows shell'
    Add-Check 'close to tray' @{ action = $closePattern; shell_tray_bounds = $trayBounds }

    if ($VerifyTrayIdleCpu) {
        $nativeWindowHidden = {
            [uint32]$windowOwner = 0
            [void][WfZiUiBpQaWindow]::GetWindowThreadProcessId($mainWindow, [ref]$windowOwner)
            $windowOwner -eq $script:Process.Id -and -not [WfZiUiBpQaWindow]::IsWindowVisible($mainWindow)
        }
        [void](Wait-For $nativeWindowHidden 'the same native main window hidden to the tray')
        Start-Sleep -Milliseconds 500
        if ($script:Process.HasExited -or -not (& $nativeWindowHidden)) {
            throw 'The isolated process did not stay hidden to the tray before CPU sampling.'
        }
        $script:Process.Refresh()
        $startCpu = $script:Process.TotalProcessorTime
        $clock = [System.Diagnostics.Stopwatch]::StartNew()
        Start-Sleep -Seconds 5
        $script:Process.Refresh()
        if ($script:Process.HasExited -or -not (& $nativeWindowHidden)) {
            throw 'The isolated process did not stay hidden to the tray during CPU sampling.'
        }
        $cpuMs = ($script:Process.TotalProcessorTime - $startCpu).TotalMilliseconds
        $clock.Stop()
        $cpuRate = $cpuMs / $clock.Elapsed.TotalSeconds
        $limit = 150
        Add-Check 'idle CPU after closing to tray' ([ordered]@{
            cpu_ms = [Math]::Round($cpuMs, 3)
            wall_ms = [Math]::Round($clock.Elapsed.TotalMilliseconds, 3)
            cpu_ms_per_wall_second = [Math]::Round($cpuRate, 3)
            limit_cpu_ms_per_wall_second = $limit
            passed = $cpuRate -lt $limit
        })
        if ($cpuRate -ge $limit) {
            throw ('Tray idle CPU used {0:F1} ms per wall second, exceeding the {1} ms limit.' -f $cpuRate, $limit)
        }
    }

    Send-AppCommand -Arguments @('--show')
    [void](Wait-For { Get-AppWindow } 'the same process main window restored via CLI')
    [void](Wait-For { Find-Control '^全局设置$' -Actionable } 'the previously open settings tab after restore')
    Add-Check 'restore from tray' "same PID $($script:Process.Id)"
    $script:Report.success = $true
} catch {
    $script:Report.error = $_.Exception.Message
    try {
        $script:Report.controls = @(Get-AppControls | Select-Object -First 140 | ForEach-Object {
            try {
                [ordered]@{
                    name = $_.Current.Name
                    type = $_.Current.ControlType.ProgrammaticName
                    visible = -not $_.Current.IsOffscreen
                    patterns = @($_.GetSupportedPatterns() | ForEach-Object ProgrammaticName)
                }
            } catch [System.Windows.Automation.ElementNotAvailableException] {}
        })
    } catch { $script:Report.control_inventory_error = $_.Exception.Message }
    Save-Report
    throw
} finally {
    if ($null -ne $script:Process) {
        try {
            if (-not $script:Process.HasExited -and (Get-ControlWindowOwner) -eq $script:Process.Id) {
                Send-AppCommand -Arguments @('--quit')
            }
        } catch { Write-Warning $_.Exception.Message }
        if (-not $script:Process.WaitForExit(3000)) {
            $script:Process.Kill()
            [void]$script:Process.WaitForExit(3000)
        }
        $script:Process.Dispose()
    }
    if (Test-Path -LiteralPath $testDir -PathType Container) {
        Get-ChildItem -LiteralPath $testDir -File -Force | ForEach-Object { Remove-Item -LiteralPath $_.FullName }
        if (-not (Get-ChildItem -LiteralPath $testDir -Force)) { Remove-Item -LiteralPath $testDir }
    }
}
Save-Report
