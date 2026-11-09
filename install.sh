#!/bin/bash
# kinect-hello installer for Arch and Debian/Ubuntu based systems.
#
#   ./install.sh              install packages, build as you, install system-wide (asks for sudo)
#   ./install.sh --no-deps    skip the package manager
#   ./install.sh --build-only install packages and build, change nothing system-wide
#
# Run it as your normal user; it uses sudo where needed. sudo ./uninstall.sh reverts it.
set -euo pipefail
cd "$(dirname "$(readlink -f "$0")")"
REPO=$PWD

say() { printf '\033[1m==> %s\033[0m\n' "$*"; }

LINE_TTY='auth       sufficient   pam_exec.so quiet stdout /usr/local/bin/kface verify'
LINE_GUI='auth       sufficient   pam_exec.so quiet /usr/local/bin/kface verify'

# ---------------------------------------------------------------- packages

as_user() {
    if [ "$(id -u)" = 0 ] && [ "$BUILD_USER" != root ]; then
        sudo -u "$BUILD_USER" -H env "PATH=/home/$BUILD_USER/.cargo/bin:$PATH" "$@"
    else
        "$@"
    fi
}

cargo_recent() { # cargo >= 1.78 (Cargo.lock v4, zbus 5)
    local v
    v=$(as_user bash -c 'PATH=$HOME/.cargo/bin:$PATH cargo --version 2>/dev/null' | awk '{print $2}')
    [ -n "$v" ] && [ "$(printf '%s\n1.78.0\n' "$v" | sort -V | head -1)" = 1.78.0 ]
}

deps() {
    # shellcheck disable=SC1091
    . /etc/os-release
    case " $ID ${ID_LIKE:-} " in
    *" arch "*)
        say "installing packages (pacman)"
        $SUDO pacman -S --needed --noconfirm base-devel git cmake curl pkgconf libusb libjpeg-turbo \
            opencv hdf5 fmt glew qt6-base qt6-declarative layer-shell-qt rust tpm2-tss fprintd libfido2
        ;;
    *" debian "* | *" ubuntu "*)
        say "installing packages (apt)"
        export DEBIAN_FRONTEND=noninteractive
        $SUDO apt-get update -qq
        $SUDO apt-get install -y -qq build-essential git cmake curl pkg-config libusb-1.0-0-dev \
            libturbojpeg0-dev libopencv-dev libtss2-dev fprintd libpam-fprintd
        # GUI and extras: names and availability differ between releases
        for p in qt6-base-dev qt6-declarative-dev qml6-module-qtquick qml6-module-qtquick-controls \
            qml6-module-qtquick-layouts qml6-module-qtquick-shapes qml6-module-qtquick-window \
            qml6-module-qtqml-workerscript qml6-module-qtquick-templates liblayershellqtinterface-dev fido2-tools; do
            $SUDO apt-get install -y -qq "$p" >/dev/null 2>&1 || echo "    optional package $p not available"
        done
        if ! cargo_recent; then
            $SUDO apt-get install -y -qq cargo >/dev/null 2>&1 || true
        fi
        if ! cargo_recent; then
            # distro Rust too old: use rustup's stable toolchain for this user
            if $SUDO apt-get install -y -qq rustup >/dev/null 2>&1; then
                as_user rustup default stable
            else
                echo "    Rust >= 1.78 not available: kface-fprintd and kface-fido will be skipped"
            fi
        fi
        ;;
    *)
        echo "Unknown distribution '$ID'. Install the dependencies listed in the README, then run: ./install.sh --no-deps"
        exit 1
        ;;
    esac
}

# ---------------------------------------------------------------- system install (root)

# add face auth as the first auth line; back up originals once
hook() { # file line [fallback-source]
    local f=$1 line=$2 src=${3:-}
    if [ ! -f "$f" ]; then
        if [ -z "$src" ] || [ ! -f "$src" ]; then return 0; fi
        cp "$src" "$f"
    fi
    if grep -q kface "$f"; then return 0; fi
    [ -f "$f.pre-kface" ] || cp -a "$f" "$f.pre-kface"
    awk -v l="$line" '!done && /^(-?auth[ \t]|@include[ \t]+common-auth)/ {print l; done=1} {print}' "$f" >"$f.new"
    if ! grep -q kface "$f.new"; then
        rm -f "$f.new"
        echo "    $f: no auth section found, skipped"
        return 0
    fi
    chmod 644 "$f.new"
    mv "$f.new" "$f"
    echo "    hooked $f"
}

unhook() { if [ -f "$1" ]; then sed -i '/kface/d' "$1"; fi; }

system_install() {
    [ "$(id -u)" = 0 ] || { echo "system install needs root"; exit 1; }
    local user=${SUDO_USER:-}
    [ -x build/kface ] || { echo "not built yet: run ./install.sh as your normal user"; exit 1; }

    say "installing files"
    install -d -m755 /usr/local/lib/kface /usr/local/share/kface /usr/local/bin
    cp -a prefix/lib/libfreenect2.so* /usr/local/lib/kface/
    install -m644 models/*.onnx /usr/local/share/kface/
    cmake --install build --prefix /usr/local >/dev/null
    install -d -m755 -o root -g root /etc/kface
    if [ -n "$user" ] && [ -f "data/$user.faces" ] && [ ! -f "/etc/kface/$user.faces" ]; then
        install -m640 -o root -g "$(id -gn "$user")" "data/$user.faces" "/etc/kface/$user.faces"
    fi
    install -m644 udev/70-kinect2-kface.rules /etc/udev/rules.d/70-kinect2-kface.rules
    udevadm control --reload-rules && udevadm trigger --subsystem-match=usb --attr-match=idVendor=045e || true

    # Desktop fingerprint integration (GNOME, KDE, Cinnamon, fprintd-* tools): an fprintd-compatible
    # service. pam_fprintd, its polkit actions and D-Bus policy come from the distro's fprintd
    # package; we only override the fprintd.service unit so D-Bus activation starts kface-fprintd.
    local pam_fprintd="" d
    for d in /usr/lib/security /usr/lib64/security /lib/security /usr/lib/*-linux-gnu/security /lib/*-linux-gnu/security; do
        if [ -f "$d/pam_fprintd.so" ]; then pam_fprintd=$d/pam_fprintd.so; break; fi
    done
    local fprint=0
    if [ -x fprint/target/release/kface-fprintd ] && [ -n "$pam_fprintd" ]; then
        say "fprintd emulation"
        install -m755 fprint/target/release/kface-fprintd /usr/local/bin/kface-fprintd
        install -m644 fprint/fprintd.service /etc/systemd/system/fprintd.service
        systemctl daemon-reload
        systemctl restart fprintd.service
        fprint=1
        echo "    active: desktops' fingerprint login now uses the face (real fprintd is overridden)"
    elif [ -x fprint/target/release/kface-fprintd ]; then
        echo "    note: install the fprintd package and re-run for GNOME/fingerprint integration"
    fi

    say "PAM"
    # Debian/Ubuntu add pam_fprintd to common-auth themselves (pam-auth-update); then sudo and
    # polkit already reach the face through fprintd and a direct hook would scan twice.
    if [ "$fprint" = 1 ] && grep -qs '^[^#].*pam_fprintd' /etc/pam.d/common-auth; then
        unhook /etc/pam.d/sudo
        unhook /etc/pam.d/polkit-1
        echo "    sudo/polkit use pam_fprintd from common-auth"
    else
        hook /etc/pam.d/sudo "$LINE_TTY"
        hook /etc/pam.d/polkit-1 "$LINE_GUI" /usr/lib/pam.d/polkit-1
    fi

    # KDE lock screen: kscreenlocker runs the "kde-fingerprint" stack in parallel with the password
    # field while the lock screen is visible. With the fprintd emulation the stock stack works as is.
    # Without it, the face check replaces pam_fprintd there ("required": this stack can only
    # succeed through a verified face).
    local f=/etc/pam.d/kde-fingerprint
    if [ "$fprint" = 1 ]; then
        if [ -f "$f" ] && grep -q kface "$f"; then
            if [ -f "$f.pre-kface" ]; then mv "$f.pre-kface" "$f"; else rm -f "$f"; fi
        fi
        unhook /etc/pam.d/kde
    elif [ -f /usr/lib/pam.d/kde-fingerprint ] || [ -f "$f" ]; then
        local src=/usr/lib/pam.d/kde-fingerprint
        if [ -f "$f" ] && ! grep -q kface "$f"; then
            [ -f "$f.pre-kface" ] || cp -a "$f" "$f.pre-kface"
            src=$f
        fi
        sed -E 's|^-?auth[[:space:]]+required[[:space:]]+pam_fprintd\.so.*|auth       required     pam_exec.so quiet stdout /usr/local/bin/kface verify|' "$src" >"$f.new"
        if grep -q '^auth .*required .*kface verify' "$f.new"; then
            chmod 644 "$f.new"
            mv "$f.new" "$f"
            echo "    hooked $f"
            unhook /etc/pam.d/kde # password field stays password-only
        else
            rm -f "$f.new"
            hook /etc/pam.d/kde "$LINE_GUI" /usr/lib/pam.d/kde
        fi
    else
        hook /etc/pam.d/kde "$LINE_GUI" /usr/lib/pam.d/kde
    fi
    hook /etc/pam.d/sddm "$LINE_GUI"

    if [ -x fido/target/release/kface-fido ] && [ -e /dev/tpmrm0 ]; then
        say "passkeys (kface-fido)"
        install -m755 fido/target/release/kface-fido /usr/local/bin/kface-fido
        install -m644 fido/kface-fido.service /etc/systemd/system/kface-fido.service
        systemctl daemon-reload
        systemctl enable kface-fido.service >/dev/null 2>&1
        systemctl restart kface-fido.service
        echo "    running"
    fi

    say "done"
    if [ -n "$user" ] && [ ! -f "/etc/kface/$user.faces" ]; then
        if [ -x /usr/local/bin/kface-setup ]; then
            echo "    Next: enroll your face with 'kface-setup' (\"Face Unlock\" in the app menu)"
        else
            echo "    Next: enroll your face with: sudo kface enroll $user"
        fi
        [ "$fprint" = 1 ] && echo "    (or through your desktop's fingerprint settings)"
    fi
    echo "    Test: sudo -k; sudo true"
}

# ---------------------------------------------------------------- main

NODEPS="" BUILDONLY=""
for a in "$@"; do
    case $a in
    --system) system_install; exit 0 ;;
    --no-deps) NODEPS=1 ;;
    --build-only) BUILDONLY=1 ;;
    *) echo "unknown option $a"; exit 64 ;;
    esac
done

if [ "$(id -u)" = 0 ]; then
    SUDO=""
    BUILD_USER=${SUDO_USER:-root}
    if [ "$BUILD_USER" = root ] && [ -z "${KFACE_BUILD_AS_ROOT:-}" ]; then
        echo "Run this as your normal user (./install.sh); it asks for sudo when needed."
        exit 1
    fi
else
    SUDO=sudo
    BUILD_USER=$(id -un)
fi

[ -n "$NODEPS" ] || deps
say "building"
as_user ./build.sh
[ -z "$BUILDONLY" ] || exit 0
if [ -z "$SUDO" ]; then
    SUDO_USER=$BUILD_USER system_install
else
    sudo "$REPO/install.sh" --system
fi
