#!/bin/bash
# kface installer - run as root from ~/kinect-hello
set -euo pipefail
cd "$(dirname "$0")"
[ "$(id -u)" = 0 ] || { echo "run with sudo"; exit 1; }
USER_NAME=${SUDO_USER:-jey}
LINE_TTY='auth       sufficient   pam_exec.so quiet stdout /usr/local/bin/kface verify'
LINE_GUI='auth       sufficient   pam_exec.so quiet /usr/local/bin/kface verify'

install -d -m755 /usr/local/lib/kface /usr/local/share/kface /usr/local/bin
cp -a prefix/lib/libfreenect2.so* /usr/local/lib/kface/
install -m644 models/*.onnx /usr/local/share/kface/
cmake --install build --prefix /usr/local >/dev/null
install -d -m755 -o root -g root /etc/kface
if [ -f "data/$USER_NAME.faces" ] && [ ! -f "/etc/kface/$USER_NAME.faces" ]; then
    install -m640 -o root -g "$(id -gn "$USER_NAME")" "data/$USER_NAME.faces" "/etc/kface/$USER_NAME.faces"
fi

# add face auth as first auth line; back up originals once
hook() { # file line [source-if-missing]
    local f=$1 line=$2 src=${3:-}
    [ -f "$f" ] || cp "$src" "$f"
    grep -q kface "$f" && return
    [ -f "$f.pre-kface" ] || cp -a "$f" "$f.pre-kface"
    awk -v l="$line" '!done && /^-?auth/ {print l; done=1} {print}' "$f" > "$f.new"
    chmod 644 "$f.new"; mv "$f.new" "$f"
    echo "hooked $f"
}
hook /etc/pam.d/sudo "$LINE_TTY"
hook /etc/pam.d/polkit-1 "$LINE_GUI" /usr/lib/pam.d/polkit-1
hook /etc/pam.d/kde "$LINE_GUI" /usr/lib/pam.d/kde
hook /etc/pam.d/sddm "$LINE_GUI"
echo "done. test: sudo -k; sudo true"
