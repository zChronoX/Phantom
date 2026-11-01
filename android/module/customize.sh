# Installazione del modulo Phantom F0 (eseguito dal manager SukiSU/KernelSU/Magisk).
# shellcheck disable=SC2034  # letto dal manager
SKIPUNZIP=0

if [ "$ARCH" != "arm64" ]; then
    abort "! Phantom F0 richiede un telefono arm64 (rilevato: $ARCH)"
fi

set_perm_recursive "$MODPATH/bin" 0 0 0755 0755
set_perm "$MODPATH/service.sh" 0 0 0755
set_perm "$MODPATH/action.sh" 0 0 0755
set_perm "$MODPATH/uninstall.sh" 0 0 0755

mkdir -p /data/adb/phantom-f0
ui_print "- Phantom F0 installato."
ui_print "- La sonda parte da sola al prossimo riavvio."
ui_print "- Per avviarla subito: tocca \"Azione\" sul modulo."
ui_print "- Se il telefono si è bloccato: WebUI › Post-mortem."
ui_print "- Video scrcpy: parti dal profilo 1 della scala nella WebUI."
