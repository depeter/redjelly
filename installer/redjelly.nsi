; redjelly installer (NSIS 3, Modern UI 2).
; Built by scripts/build-windows.sh:
;   makensis -DVERSION=0.1.0 -DSRC=<dist/redjelly> -DOUT=<setup.exe> installer/redjelly.nsi
;
; Per-user install (no admin / UAC prompt) into %LOCALAPPDATA%\Programs,
; with a Start menu entry, optional desktop shortcut, and an Apps & features
; entry with uninstaller.

Unicode true
SetCompressor /SOLID lzma

!include "MUI2.nsh"
!include "FileFunc.nsh"

!ifndef VERSION
  !error "pass -DVERSION=x.y.z"
!endif
!ifndef SRC
  !error "pass -DSRC=<folder with redjelly.exe>"
!endif
!ifndef OUT
  !define OUT "redjelly-setup-${VERSION}.exe"
!endif

!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\redjelly"

Name "redjelly"
OutFile "${OUT}"
InstallDir "$LOCALAPPDATA\Programs\redjelly"
InstallDirRegKey HKCU "Software\redjelly" "InstallDir"
RequestExecutionLevel user
BrandingText "redjelly ${VERSION}"

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "redjelly"
VIAddVersionKey "FileDescription" "redjelly setup"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "GPL-3.0-or-later"

!define MUI_ICON "redjelly.ico"
!define MUI_UNICON "redjelly.ico"
!define MUI_ABORTWARNING
!define MUI_WELCOMEFINISHPAGE_BITMAP "sidebar.bmp"
!define MUI_UNWELCOMEFINISHPAGE_BITMAP "sidebar.bmp"
!define MUI_HEADERIMAGE
!define MUI_HEADERIMAGE_RIGHT
!define MUI_HEADERIMAGE_BITMAP "header.bmp"
!define MUI_HEADERIMAGE_UNBITMAP "header.bmp"
!define MUI_WELCOMEPAGE_TEXT "This will install redjelly ${VERSION}, a Jellyfin client, for your Windows user.$\r$\n$\r$\nNo administrator rights are needed. If redjelly is already installed, it is updated and your settings are kept."
!define MUI_FINISHPAGE_RUN "$INSTDIR\redjelly.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Start redjelly"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "..\LICENSE"
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Section "redjelly" SecMain
  SectionIn RO
  SetOutPath "$INSTDIR"

  ; Updating while redjelly runs would fail half-way: ask to close it first.
  retry:
    ClearErrors
    IfFileExists "$INSTDIR\redjelly.exe" 0 copy
    Delete "$INSTDIR\redjelly.exe"
    IfErrors 0 copy
    MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "redjelly is still running.$\r$\nClose it, then press Retry." IDRETRY retry
    Abort
  copy:

  File "${SRC}\redjelly.exe"
  File "${SRC}\libmpv-2.dll"
  File "${SRC}\libc++.dll"
  File "${SRC}\libunwind.dll"
  File "${SRC}\README.txt"
  File "/oname=LICENSE.txt" "..\LICENSE"
  File "THIRD-PARTY.txt"

  WriteUninstaller "$INSTDIR\uninstall.exe"
  CreateShortcut "$SMPROGRAMS\redjelly.lnk" "$INSTDIR\redjelly.exe" "" "$INSTDIR\redjelly.exe" 0 SW_SHOWNORMAL "" "Jellyfin client"

  WriteRegStr HKCU "Software\redjelly" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayName" "redjelly"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINST_KEY}" "Publisher" "redjelly"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\redjelly.exe"
  WriteRegStr HKCU "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegStr HKCU "${UNINST_KEY}" "URLInfoAbout" "https://github.com/depeter/redjelly"
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "${UNINST_KEY}" "EstimatedSize" "$0"
SectionEnd

Section /o "Desktop shortcut" SecDesktop
  CreateShortcut "$DESKTOP\redjelly.lnk" "$INSTDIR\redjelly.exe" "" "$INSTDIR\redjelly.exe" 0 SW_SHOWNORMAL "" "Jellyfin client"
SectionEnd

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecMain} "The redjelly app with its video engine (mpv)."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDesktop} "Put a redjelly shortcut on the desktop."
!insertmacro MUI_FUNCTION_DESCRIPTION_END

Section "Uninstall"
  Delete "$INSTDIR\redjelly.exe"
  Delete "$INSTDIR\libmpv-2.dll"
  Delete "$INSTDIR\libc++.dll"
  Delete "$INSTDIR\libunwind.dll"
  Delete "$INSTDIR\README.txt"
  Delete "$INSTDIR\LICENSE.txt"
  Delete "$INSTDIR\THIRD-PARTY.txt"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"

  Delete "$SMPROGRAMS\redjelly.lnk"
  Delete "$DESKTOP\redjelly.lnk"
  DeleteRegKey HKCU "${UNINST_KEY}"
  DeleteRegKey HKCU "Software\redjelly"

  ; Settings, logins and AI models live outside the install folder; keep
  ; them unless asked (silent uninstalls always keep them).
  IfSilent done
  MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "Also remove your redjelly settings, key bindings, caches and downloaded AI models?$\r$\n$\r$\nThe AI models can take 1.5 GB or more. Choose No to keep them for a later reinstall." IDNO done
    RMDir /r "$LOCALAPPDATA\redjelly"
    RMDir /r "$APPDATA\redjelly"
  done:
SectionEnd
