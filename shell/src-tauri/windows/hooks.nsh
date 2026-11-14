!macro NUCLEOS_STOP_DAEMON
  nsExec::Exec '"$SYSDIR\schtasks.exe" /End /TN "NucleOS Daemon"'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /T /IM nucleos-core.exe'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /T /IM nucleos-browser.exe'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /T /IM nucleos-echo.exe'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /T /IM nucleos-email.exe'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /T /IM nucleos-quota.exe'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /T /IM nucleos-remote.exe'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /T /IM nucleos-telegram.exe'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /T /IM nucleos-web.exe'
  Pop $0
  ; Before the rename the sidecars were called <name>-sidecar.exe: stop any still running from
  ; an older install, then delete the leftovers so an upgrade does not strand them.
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /T /IM browser-sidecar.exe'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /T /IM echo-sidecar.exe'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /T /IM email-sidecar.exe'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /T /IM quota-sidecar.exe'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /T /IM remote-sidecar.exe'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /T /IM telegram-sidecar.exe'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /T /IM web-sidecar.exe'
  Pop $0
  Sleep 1000
  Delete "$INSTDIR\browser-sidecar.exe"
  Delete "$INSTDIR\echo-sidecar.exe"
  Delete "$INSTDIR\email-sidecar.exe"
  Delete "$INSTDIR\quota-sidecar.exe"
  Delete "$INSTDIR\remote-sidecar.exe"
  Delete "$INSTDIR\telegram-sidecar.exe"
  Delete "$INSTDIR\web-sidecar.exe"
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
