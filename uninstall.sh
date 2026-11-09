#!/bin/bash
# remove kface PAM hooks (keeps /etc/kface face data)
set -euo pipefail
[ "$(id -u)" = 0 ] || { echo "run with sudo"; exit 1; }
# give D-Bus activation back to the real fprintd
if grep -qs kface /etc/systemd/system/fprintd.service; then
    systemctl stop fprintd.service 2>/dev/null || true
    rm -f /etc/systemd/system/fprintd.service /usr/local/bin/kface-fprintd
    systemctl daemon-reload
    echo "removed fprintd override"
fi
# our kde-fingerprint is a full replacement: restore or delete it, never leave it without a check
f=/etc/pam.d/kde-fingerprint
if [ -f "$f" ] && grep -q kface "$f"; then
    if [ -f "$f.pre-kface" ]; then mv "$f.pre-kface" "$f"; else rm -f "$f"; fi
    echo "restored $f"
fi
for f in /etc/pam.d/sudo /etc/pam.d/polkit-1 /etc/pam.d/kde /etc/pam.d/sddm; do
    [ -f "$f" ] && sed -i '/kface/d' "$f" && echo "unhooked $f"
done
systemctl disable --now kface-fido.service 2>/dev/null || true
rm -f /etc/systemd/system/kface-fido.service /usr/local/bin/kface-fido
rm -f /usr/local/bin/kface /usr/local/bin/kface-setup /usr/local/bin/kface-overlay
rm -f /usr/local/share/applications/kface-setup.desktop /etc/xdg/autostart/kface-overlay.desktop
rm -rf /usr/local/lib/kface /usr/local/share/kface
