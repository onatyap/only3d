; The Windows installer: per user (no administrator rights), into
; %LOCALAPPDATA%\Programs\only3d, with Start menu and desktop shortcuts that open the window.
; Built by CI: makensis /DVERSION=0.1.0 /DSRC=<folder with the exes> /DOUT=<setup.exe> only3d.nsi
Unicode true
!include "MUI2.nsh"

!ifndef VERSION
  !define VERSION "0.0.0"
!endif
!define APP "only3d"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP}"

Name "${APP}"
OutFile "${OUT}"
InstallDir "$LOCALAPPDATA\Programs\${APP}"
InstallDirRegKey HKCU "Software\${APP}" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma

!define MUI_ICON "..\icons\only3d.ico"
!define MUI_UNICON "..\icons\only3d.ico"
!define MUI_FINISHPAGE_RUN "$INSTDIR\only3d-app.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Open only3d"
!insertmacro MUI_PAGE_LICENSE "..\..\LICENSE"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${APP}"
VIAddVersionKey "FileDescription" "${APP} installer"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "AGPL-3.0-or-later"

Section "only3d"
  SetOutPath "$INSTDIR"
  File "${SRC}\only3d.exe"
  File "${SRC}\only3d-app.exe"
  File "..\icons\only3d.ico"
  File "..\..\LICENSE"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  CreateShortcut "$SMPROGRAMS\${APP}.lnk" "$INSTDIR\only3d-app.exe" "" "$INSTDIR\only3d.ico"
  CreateShortcut "$DESKTOP\${APP}.lnk" "$INSTDIR\only3d-app.exe" "" "$INSTDIR\only3d.ico"
  WriteRegStr HKCU "Software\${APP}" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayName" "${APP}"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\only3d.ico"
  WriteRegStr HKCU "${UNINST_KEY}" "Publisher" "only3d"
  WriteRegStr HKCU "${UNINST_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
SectionEnd

Section "Uninstall"
  Delete "$SMPROGRAMS\${APP}.lnk"
  Delete "$DESKTOP\${APP}.lnk"
  Delete "$INSTDIR\only3d.exe"
  Delete "$INSTDIR\only3d-app.exe"
  Delete "$INSTDIR\only3d.ico"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "${UNINST_KEY}"
  DeleteRegKey HKCU "Software\${APP}"
SectionEnd
