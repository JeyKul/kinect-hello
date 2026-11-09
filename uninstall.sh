#!/bin/bash
# remove kface PAM hooks (keeps /etc/kface face data)
set -euo pipefail
[ "$(id -u)" = 0 ] || { echo "run with sudo"; exit 1; }
for f in /etc/pam.d/sudo /etc/pam.d/polkit-1 /etc/pam.d/kde /etc/pam.d/sddm; do
    [ -f "$f" ] && sed -i '/kface/d' "$f" && echo "unhooked $f"
done
rm -f /usr/local/bin/kface
rm -rf /usr/local/lib/kface /usr/local/share/kface
