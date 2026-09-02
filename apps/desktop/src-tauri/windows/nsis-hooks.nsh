; Tauri chooses or restores $INSTDIR in .onInit. NSIS invokes this function from
; .onGUIInit before displaying the directory page and again before a silent install.

Function TorbenSetDefaultInstallDirectory
  ; An explicit /D value is always authoritative.
  ClearErrors
  ${GetOptions} $CMDLINE "/D=" $R2
  ${IfNot} ${Errors}
    ClearErrors
    Return
  ${EndIf}
  ClearErrors

  ; Preserve Tauri's restored path only while the matching installation still exists.
  ; The manufacturer key can survive uninstall and must not make a fresh install stale.
  ReadRegStr $R0 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Torben App" "UninstallString"
  ${If} $R0 != ""
    Return
  ${EndIf}

  StrCpy $INSTDIR "$LOCALAPPDATA\Torben App"
  ReadEnvStr $R1 "SystemDrive"
  ${StrCase} $R1 $R1 "U"

  StrCpy $R3 "DEFGHIJKLMNOPQRSTUVWXYZ"
  torben_drive_loop:
    StrCpy $R4 $R3 1
    StrCpy $R3 $R3 "" 1
    StrCmp $R4 "" torben_drive_done
    StrCmp $R1 "$R4:" torben_drive_loop
    System::Call 'kernel32::GetDriveTypeW(w "$R4:\") i .r0'
    ${If} $0 = 3 ; DRIVE_FIXED
      StrCpy $INSTDIR "$R4:\TorbenApp"
      Return
    ${EndIf}
    Goto torben_drive_loop
  torben_drive_done:
FunctionEnd

!define MUI_CUSTOMFUNCTION_GUIINIT TorbenSetDefaultInstallDirectory

; Silent installers do not invoke .onGUIInit. Tauri has already called SetOutPath when
; this hook runs, so apply the selected directory again before any files are copied.
!macro NSIS_HOOK_PREINSTALL
  Call TorbenSetDefaultInstallDirectory
  SetOutPath $INSTDIR
!macroend
