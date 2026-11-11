; Extra installer steps (see bundle > windows > nsis > installerHooks in tauri.conf.json).
;
; TV sharing listens on the home network (the web page and DLNA on a TCP port, SSDP on
; UDP 1900). Windows Firewall blocks that until it is allowed, so the installer adds one
; rule for memora.exe, for private and domain networks only: never on networks marked
; Public (cafes, hotels, airports). Sharing itself stays off until it is switched on in
; Settings. The rule is per program, so changing the port in Settings needs no new rule.

!define MEMORA_FIREWALL_RULE "Memora (TV sharing)"

!macro NSIS_HOOK_POSTINSTALL
  ; Remove a rule left by an earlier install first, so updates don't stack duplicates.
  nsExec::Exec 'netsh advfirewall firewall delete rule name="${MEMORA_FIREWALL_RULE}"'
  Pop $0
  nsExec::Exec 'netsh advfirewall firewall add rule name="${MEMORA_FIREWALL_RULE}" dir=in action=allow program="$INSTDIR\${MAINBINARYNAME}.exe" enable=yes profile=private,domain description="Lets TVs and phones on your home network open photos and videos shared by Memora."'
  Pop $0
  ${If} $0 != 0
    DetailPrint "Could not add the Windows Firewall rule for TV sharing (error $0)."
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::Exec 'netsh advfirewall firewall delete rule name="${MEMORA_FIREWALL_RULE}"'
  Pop $0
!macroend
