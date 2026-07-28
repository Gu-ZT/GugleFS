!include LogicLib.nsh

!macro NSIS_HOOK_POSTINSTALL
  ; WinFsp is a system dependency shared by all GugleFS installations.
  ClearErrors
  SetRegView 64
  ReadRegStr $0 HKLM "SOFTWARE\WinFsp" "InstallDir"
  ${If} $0 == ""
    SetRegView 32
    ReadRegStr $0 HKLM "SOFTWARE\WinFsp" "InstallDir"
  ${EndIf}

  ${If} $0 == ""
    ${If} ${FileExists} "$INSTDIR\resources\winfsp-2.1.25156.msi"
      DetailPrint "Installing WinFsp..."
      CopyFiles "$INSTDIR\resources\winfsp-2.1.25156.msi" "$TEMP\winfsp-2.1.25156.msi"
      ExecWait '"$SYSDIR\msiexec.exe" /i "$TEMP\winfsp-2.1.25156.msi" /qn /norestart INSTALLLEVEL=1000' $1
      Delete "$TEMP\winfsp-2.1.25156.msi"
      ${If} $1 != 0
        ${If} $1 != 1638
          ${If} $1 != 3010
            MessageBox MB_ICONSTOP|MB_OK "WinFsp installation failed (code $1). GugleFS cannot mount drives without WinFsp."
            Abort
          ${EndIf}
        ${EndIf}
      ${EndIf}
    ${Else}
      MessageBox MB_ICONSTOP|MB_OK "The bundled WinFsp installer is missing. GugleFS cannot mount drives."
      Abort
    ${EndIf}
  ${EndIf}

  Delete "$INSTDIR\resources\winfsp-2.1.25156.msi"
!macroend
