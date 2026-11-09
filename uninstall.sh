#!/bin/bash
# remove kface PAM hooks (keeps /etc/kface face data)
set -euo pipefail
[ "$(id -u)" = 0 ] || { echo "run with sudo"; exit 1; }
for f in /etc/pam.d/sudo /etc/pam.d/polkit-1 /etc/pam.d/kde /etc/pam.d/sddm; do
    [ -f "$f" ] && sed -i '/kface/d' "$f" && echo "unhooked $f"
done
rm -f /usr/local/bin/kface /usr/local/bin/kface-setup /usr/local/bin/kface-overlay
rm -f /usr/local/share/applications/kface-setup.desktop /etc/xdg/autostart/kface-overlay.desktop
rm -rf /usr/local/lib/kface /usr/local/share/kface
