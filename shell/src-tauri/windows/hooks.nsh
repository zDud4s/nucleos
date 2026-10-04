!macro NUCLEOS_STOP_DAEMON
  nsExec::Exec '"$SYSDIR\schtasks.exe" /End /TN "NucleOS Daemon"'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /T /IM nucleos-core.exe'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /T /IM browser-sidecar.exe'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /T /IM echo-sidecar.exe'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /T /IM email-sidecar.exe'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /T /IM quota-sidecar.exe'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /T /IM telegram-sidecar.exe'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /T /IM web-sidecar.exe'
  Pop $0
  Sleep 1000
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro NUCLEOS_STOP_DAEMON
!macroend

!macro NSIS_HOOK_POSTINSTALL
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro NUCLEOS_STOP_DAEMON
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  nsExec::Exec '"$SYSDIR\schtasks.exe" /Delete /TN "NucleOS Daemon" /F'
  Pop $0
!macroend
