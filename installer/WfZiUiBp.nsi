Unicode true
RequestExecutionLevel user
ManifestDPIAware true
SetCompressor /SOLID zlib
CRCCheck force

!include "LogicLib.nsh"
!include "MUI2.nsh"
!include "WinVer.nsh"
!include "x64.nsh"
!include "${PAYLOAD_INCLUDE}"

!define APP_NAME "文字识别 WfZiUiBp"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\WfZiUiBp"
!define RUN_KEY "Software\Microsoft\Windows\CurrentVersion\Run"
!define MAIN_CLASS "WfZiUiBp_Desktop"

Name "${APP_NAME}"
OutFile "${INSTALLER_OUTPUT}"
InstallDir "$LOCALAPPDATA\Programs\WfZiUiBp"
VIProductVersion "${APP_VERSION}.0"
VIAddVersionKey /LANG=2052 "ProductName" "${APP_NAME}"
VIAddVersionKey /LANG=2052 "FileDescription" "${APP_NAME} 安装程序"
VIAddVersionKey /LANG=2052 "FileVersion" "${APP_VERSION}"
VIAddVersionKey /LANG=2052 "ProductVersion" "${APP_VERSION}"
VIAddVersionKey /LANG=2052 "LegalCopyright" "WfZiUiBp contributors"

Var StagePath
Var BackupPath

!define MUI_ICON "${APP_ICON}"
!define MUI_UNICON "${APP_ICON}"
!define MUI_ABORTWARNING
!define MUI_WELCOMEPAGE_TEXT "本向导为当前 Windows 用户安装文字识别。应用文件放在当前用户的 Programs\WfZiUiBp 目录，安装结束后可从开始菜单手动打开。"
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "SimpChinese"

!macro CheckAppClosed
    FindWindow $0 "${MAIN_CLASS}"
    ${If} $0 != 0
        MessageBox MB_ICONEXCLAMATION|MB_OK "请先从托盘退出文字识别，再继续安装或卸载。" /SD IDOK
        SetErrorLevel 1
        Abort
    ${EndIf}
!macroend

!macro CheckInstallTarget
    System::Call 'kernel32::GetFileAttributesW(w "$INSTDIR") i .r0'
    IntOp $0 $0 & 0x400
    ${If} $0 != 0
        MessageBox MB_ICONSTOP|MB_OK "安装目录不可访问或为文件系统链接。请恢复常规目录后重试。" /SD IDOK
        SetErrorLevel 1
        Abort
    ${EndIf}
!macroend

!macro LockInstaller
    System::Call 'kernel32::CreateMutexW(p 0, i 0, w "Local\WfZiUiBp.Setup") p .r9 ?e'
    Pop $0
    ${If} $9 == 0
    ${OrIf} $0 == 183
        MessageBox MB_ICONEXCLAMATION|MB_OK "另一个文字识别安装或卸载程序正在运行，请稍后重试。" /SD IDOK
        SetErrorLevel 1
        Abort
    ${EndIf}
!macroend

Function .onInit
    ${IfNot} ${IsNativeAMD64}
    ${OrIfNot} ${AtLeastWin10}
        MessageBox MB_ICONSTOP|MB_OK "此版本需要 Windows 10 或更高版本的 x64 系统。" /SD IDOK
        SetErrorLevel 1
        Abort
    ${EndIf}
    !insertmacro LockInstaller
    SetShellVarContext current
    SetRegView 64
    StrCpy $INSTDIR "$LOCALAPPDATA\Programs\WfZiUiBp"
FunctionEnd

Section "文字识别应用和开始菜单" Main
    SectionIn RO
    !insertmacro CheckAppClosed

    ${If} ${FileExists} "$INSTDIR\*.*"
        !insertmacro CheckInstallTarget
        ReadRegStr $0 HKCU "${UNINSTALL_KEY}" "InstallLocation"
        ${If} $0 != $INSTDIR
            RMDir "$INSTDIR"
            ${If} ${FileExists} "$INSTDIR\*.*"
                MessageBox MB_ICONSTOP|MB_OK "安装目录已存在且包含其他文件。请先移走该目录后重试。$\r$\n$INSTDIR" /SD IDOK
                SetErrorLevel 1
                Abort
            ${EndIf}
        ${EndIf}
    ${EndIf}

    ClearErrors
    CreateDirectory "$LOCALAPPDATA\Programs"
    GetTempFileName $StagePath "$LOCALAPPDATA\Programs"
    IfErrors stageFailed
    Delete "$StagePath"
    IfErrors stageFailed
    CreateDirectory "$StagePath"
    IfErrors stageFailed
    !insertmacro InstallPayload "$StagePath"
    IfErrors stageFailed
    WriteUninstaller "$StagePath\Uninstall.exe"
    IfErrors stageFailed

    SetOutPath "$LOCALAPPDATA\Programs"
    StrCpy $BackupPath ""
    ${If} ${FileExists} "$INSTDIR\*.*"
        GetTempFileName $BackupPath "$LOCALAPPDATA\Programs"
        IfErrors stageFailed
        Delete "$BackupPath"
        IfErrors stageFailed
        Rename "$INSTDIR" "$BackupPath"
        IfErrors stageFailed
    ${EndIf}
    Rename "$StagePath" "$INSTDIR"
    IfErrors restorePrevious

    SetOutPath "$INSTDIR"
    ClearErrors
    CreateDirectory "$SMPROGRAMS\${APP_NAME}"
    CreateShortCut "$SMPROGRAMS\${APP_NAME}\文字识别.lnk" "$INSTDIR\WfZiUiBp.exe"
    CreateShortCut "$SMPROGRAMS\${APP_NAME}\卸载文字识别.lnk" "$INSTDIR\Uninstall.exe"
    WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${APP_NAME}"
    WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${APP_VERSION}"
    WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" '$\"$INSTDIR\WfZiUiBp.exe$\",0'
    WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
    WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '$\"$INSTDIR\Uninstall.exe$\"'
    WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '$\"$INSTDIR\Uninstall.exe$\" /S'
    WriteRegDWORD HKCU "${UNINSTALL_KEY}" "EstimatedSize" ${PAYLOAD_KIB}
    WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
    WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
    ${If} ${Errors}
        MessageBox MB_ICONEXCLAMATION|MB_OK "应用文件已安装，但开始菜单或卸载登记未完整写入。可以从以下目录直接运行或卸载。$\r$\n$INSTDIR" /SD IDOK
    ${EndIf}

    ${If} $BackupPath != ""
        !insertmacro RemovePayload "$BackupPath"
        Delete "$BackupPath\Uninstall.exe"
        RMDir "$BackupPath"
        ${If} ${FileExists} "$BackupPath\*.*"
            DetailPrint "原安装目录中的其他文件已保留在 $BackupPath"
        ${EndIf}
    ${EndIf}
    Goto installed

restorePrevious:
    ${If} $BackupPath != ""
        ClearErrors
        Rename "$BackupPath" "$INSTDIR"
        ${If} ${Errors}
            MessageBox MB_ICONEXCLAMATION|MB_OK "原安装文件保留在以下位置，可手动移回。$\r$\n$BackupPath" /SD IDOK
        ${EndIf}
    ${EndIf}
stageFailed:
    ${If} $StagePath != ""
        SetOutPath "$LOCALAPPDATA\Programs"
        !insertmacro RemovePayload "$StagePath"
        Delete "$StagePath\Uninstall.exe"
        RMDir "$StagePath"
    ${EndIf}
    MessageBox MB_ICONSTOP|MB_OK "未能写入完整的应用文件。请确认当前用户有写入权限，并退出所有正在使用的文字识别窗口。" /SD IDOK
    SetErrorLevel 1
    Abort
installed:
SectionEnd

Section /o "创建桌面快捷方式" DesktopLink
    CreateShortCut "$DESKTOP\${APP_NAME}.lnk" "$INSTDIR\WfZiUiBp.exe"
SectionEnd

Function un.onInit
    !insertmacro LockInstaller
    SetShellVarContext current
    SetRegView 64
    ${If} $INSTDIR != "$LOCALAPPDATA\Programs\WfZiUiBp"
        MessageBox MB_ICONSTOP|MB_OK "请从应用的原安装目录运行卸载程序。" /SD IDOK
        SetErrorLevel 1
        Abort
    ${EndIf}
    !insertmacro CheckAppClosed
    !insertmacro CheckInstallTarget
FunctionEnd

Section "Uninstall"
    SetOutPath "$LOCALAPPDATA\Programs"
    !insertmacro RemovePayload "$INSTDIR"
    ${If} ${FileExists} "$INSTDIR\WfZiUiBp.exe"
        MessageBox MB_ICONEXCLAMATION|MB_OK "程序文件仍存在。请检查文件占用或子目录中的文件系统链接后重试卸载。" /SD IDOK
        SetErrorLevel 1
        Abort
    ${EndIf}
    Delete "$DESKTOP\${APP_NAME}.lnk"
    Delete "$SMPROGRAMS\${APP_NAME}\文字识别.lnk"
    Delete "$SMPROGRAMS\${APP_NAME}\卸载文字识别.lnk"
    RMDir "$SMPROGRAMS\${APP_NAME}"
    ReadRegStr $0 HKCU "${RUN_KEY}" "WfZiUiBp"
    ${If} $0 == '$\"$INSTDIR\WfZiUiBp.exe$\" --hide'
        DeleteRegValue HKCU "${RUN_KEY}" "WfZiUiBp"
    ${EndIf}
    DeleteRegKey HKCU "${UNINSTALL_KEY}"
    Delete "$INSTDIR\Uninstall.exe"
    RMDir "$INSTDIR"
SectionEnd
