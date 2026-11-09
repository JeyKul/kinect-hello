# kinect-hello

3D face unlock and passkeys for Linux with a **Kinect v2** (Xbox One Kinect), in the style of Windows Hello.

- **Face unlock** for `sudo`, polkit prompts, the KDE lock screen and SDDM login, with your password as fallback. The lock screen scans by itself as soon as it appears, through KDE's parallel fingerprint stack, so there's no need to press Enter.
- **Liveness from real depth:** the Kinect's time-of-flight camera has to see a 3D face
- **Attention check:** your head has to point at the sensor for 0.6 s, so a sudo prompt can't pass while you look at another screen
- **Passkeys:** a virtual FIDO2 security key whose keys live in the TPM and are released only after face verification. Works in Firefox and Chrome for any site that supports security keys or passkeys.
- A setup app with live IR preview, and an animated overlay next to the mouse pointer while scanning
- Works in the dark (infrared)

## How it works

| Step | What happens |
|---|---|
| Capture | [libfreenect2](https://github.com/OpenKinect/libfreenect2) streams the 512×424 IR image and the pixel-aligned depth map |
| Detect | OpenCV [YuNet](https://github.com/opencv/opencv_zoo/tree/main/models/face_detection_yunet) finds the face and 5 landmarks in the IR image |
| Liveness | Depth at the landmarks: nose 8–60 mm in front of the eye/mouth plane, in front of both the nasal bridge and the cheeks (a printed photo can only bend one way), non-planar surface, interocular distance 48–80 mm, distance 30–130 cm |
| Attention | Head yaw from the landmarks ≤ 15°, held continuously for 0.6 s |
| Recognize | OpenCV [SFace](https://github.com/opencv/opencv_zoo/tree/main/models/face_recognition_sface) embedding of the IR face, cosine similarity ≥ 0.45 against your enrolled samples |

**What it blocks:** phone or monitor screens (they're black in infrared), flat or bent printed photos, and being merely in view.
**What it doesn't block:** a good 3D mask, or an identical twin. That puts it roughly at Windows Hello's level, below Face ID. Recognition itself runs on the 2D IR image; depth is used only for liveness.

### Passkeys (`kface-fido`)

A root service creates a virtual USB HID device via `/dev/uhid` that speaks CTAPHID + CTAP 2.0. Browsers see an ordinary security key; systemd grants the logged-in user access. Every credential is a P-256 key **created inside the TPM**, and only the TPM-wrapped blob is stored in `/var/lib/kface-fido`, so a copied disk is useless. Registration and every sign-in run `kface verify` for the user of the active seat, and the overlay shows which site is asking.

Limitations: browsers list it as a "security key", not as "this device". Passkeys are bound to this machine and aren't synced. **Clearing the TPM destroys them, so keep a second login method on every account.** The USB id `1209:4b46` is from the [pid.codes](https://pid.codes) open-source range and isn't registered (yet).

## Requirements

- Kinect v2 with the Windows adapter, on a USB 3 port
- Arch Linux, or similar: `cmake opencv qt6-declarative layer-shell-qt libusb glfw libjpeg-turbo` and `rust tpm2-tss` for passkeys
- KDE Plasma on Wayland for the overlay and lock-screen integration. The PAM part works anywhere.
- A TPM 2.0 for passkeys

## Build and install

```sh
./build.sh            # builds libfreenect2 (pinned), fetches models, builds everything, runs tests
sudo ./install.sh     # installs to /usr/local, hooks PAM (sudo, polkit-1, kde, sddm), starts kface-fido
kface-setup           # enroll your face ("Face Unlock" in the app menu)
```

`install.sh` backs up every PAM file it touches as `*.pre-kface`. For the lock screen it replaces `pam_fprintd` in `/etc/pam.d/kde-fingerprint` with the face check (so a real fingerprint reader and kface can't be used together on the lock screen yet). `sudo ./uninstall.sh` removes the hooks and binaries but keeps your face data in `/etc/kface`. SSH and `su` are never hooked.

Tuning goes in `/etc/kface/kface.conf`, one `key value` pair per line: `match_threshold`, `max_yaw_deg`, `attention_s`, `timeout_s`, `min_relief_mm`, `min_relief_v_mm`, `min_relief_h_mm`, `min_plane_rms_mm`.

## Command line

```
kface debug [n]          save n annotated IR/depth frames to ./debug and print all metrics
kface pose               guided head-pose recording, for tuning the attention check
kface verify <user>      exit 0 on a live, attentive, matching face (this is what PAM runs)
kface enroll <user>      enroll from the terminal (root)
```

## Security notes

- Face data (128-float embeddings, no images) is stored in `/etc/kface/<user>.faces`, owned by root and readable by that user so the lock screen (which runs as the user) can use it. Users can't modify it. `kface-setup` installs new data via `pkexec kface import`, which accepts only a file owned by the calling user and validates every embedding.
- Unprivileged callers can only verify themselves.
- Face unlock is a convenience, not a second factor. KWallet still needs your password after an SDDM face login.

## Tests

`./build/kface_tests` checks liveness against synthetic depth maps (flat, tilted, bent photos, wrong size or distance, missing depth), the attention logic, storage and input validation. `cargo test` in `fido/` covers CTAPHID framing and full register/sign-in flows, including signature verification and rejection cases.

## License

MIT. libfreenect2 (Apache 2.0 / GPL 2) and the OpenCV Zoo models (Apache 2.0 / MIT) are downloaded at build time and aren't included.
