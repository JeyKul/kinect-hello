//! kface-fido: a virtual FIDO2 security key. Browsers see a USB security key; passkeys are TPM
//! keys and every use requires face verification through `kface verify`.
//!
//!   kface-fido [--soft] [--store DIR] [--no-uv]
//!
//!   --soft      software keys instead of the TPM (testing only)
//!   --store     credential directory (default /var/lib/kface-fido)
//!   --no-uv     skip face verification (testing only)

mod ctap2;
mod ctaphid;
mod store;
mod uhid;

use ctaphid::{Assembler, Feed};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const KFACE: &str = "/usr/local/bin/kface";
const VENDOR: u32 = 0x1209; // pid.codes open source vendor id
const PRODUCT: u32 = 0x4b46;

// FIDO usage page, 64 byte input and output reports
const REPORT_DESCRIPTOR: [u8; 34] = [
    0x06, 0xd0, 0xf1, // usage page (FIDO alliance)
    0x09, 0x01, // usage (CTAPHID)
    0xa1, 0x01, // collection (application)
    0x09, 0x20, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x40, 0x81, 0x02, // input, 64 bytes
    0x09, 0x21, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x40, 0x91, 0x02, // output, 64 bytes
    0xc0, // end collection
];

type Writer = Arc<Mutex<uhid::Device>>;

fn send(w: &Writer, cid: u32, cmd: u8, data: &[u8]) {
    let mut dev = w.lock().unwrap();
    for p in ctaphid::frame(cid, cmd, data) {
        if let Err(e) = dev.input(&p) {
            eprintln!("kface-fido: write: {e}");
            return;
        }
    }
}

/// user owning the active session on seat0
fn seat_user() -> Option<String> {
    let seat = std::fs::read_to_string("/run/systemd/seats/seat0").ok()?;
    let uid: u32 = seat.lines().find_map(|l| l.strip_prefix("ACTIVE_UID="))?.trim().parse().ok()?;
    let pw = unsafe { libc::getpwuid(uid) };
    if pw.is_null() {
        return None;
    }
    let name = unsafe { std::ffi::CStr::from_ptr((*pw).pw_name) };
    name.to_str().ok().map(str::to_string)
}

/// run kface verify while keeping the browser informed; abort on CTAPHID_CANCEL
fn face_verify(user: &str, reason: &str, cancel: &AtomicBool, keepalive: &dyn Fn()) -> ctap2::Uv {
    let child = Command::new(KFACE)
        .arg("verify")
        .arg(user)
        .env_clear()
        .env("KFACE_REASON", reason)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            eprintln!("kface-fido: cannot run {KFACE}: {e}");
            return ctap2::Uv::Denied;
        }
    };
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return if status.success() { ctap2::Uv::Ok } else { ctap2::Uv::Denied },
            Ok(None) => {}
            Err(_) => return ctap2::Uv::Denied,
        }
        if cancel.load(Ordering::SeqCst) {
            // SIGTERM lets kface close the Kinect cleanly
            unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
            let _ = child.wait();
            return ctap2::Uv::Cancelled;
        }
        keepalive();
        thread::sleep(Duration::from_millis(100));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let soft = args.iter().any(|a| a == "--soft");
    let no_uv = args.iter().any(|a| a == "--no-uv");
    let dir = args
        .iter()
        .position(|a| a == "--store")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/lib/kface-fido"));

    let backend = if soft {
        eprintln!("kface-fido: WARNING software keys, for testing only");
        store::Backend::Soft
    } else {
        match store::tpm::Tpm::open() {
            Ok(t) => store::Backend::Tpm(t),
            Err(e) => {
                eprintln!("kface-fido: {e}");
                std::process::exit(1);
            }
        }
    };
    if no_uv {
        eprintln!("kface-fido: WARNING face verification disabled, for testing only");
    }
    let auth = Arc::new(Mutex::new(ctap2::Authenticator::new(store::Store::new(dir, backend))));

    let mut dev = match uhid::Device::create("kface virtual FIDO2 key", VENDOR, PRODUCT, &REPORT_DESCRIPTOR) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("kface-fido: cannot create /dev/uhid device: {e}");
            std::process::exit(1);
        }
    };
    let writer: Writer = Arc::new(Mutex::new(dev.try_clone().expect("dup uhid fd")));

    // a CBOR request runs on a worker thread so we can keep reading CANCEL packets
    let busy: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
    let cancel = Arc::new(AtomicBool::new(false));
    let mut asm = Assembler::default();
    let mut next_cid: u32 = 1;
    eprintln!("kface-fido: ready");

    loop {
        let data = match dev.read() {
            Ok(uhid::Event::Output(d)) => d,
            Ok(uhid::Event::Other) => continue,
            Err(e) => {
                eprintln!("kface-fido: read: {e}");
                break;
            }
        };
        // hidraw may prepend the (zero) report id
        let packet = if data.len() == ctaphid::PACKET + 1 { &data[1..] } else { &data[..] };

        match asm.feed(packet) {
            Feed::Pending => {}
            Feed::Error { cid, code } => send(&writer, cid, ctaphid::ERROR, &[code]),
            Feed::Cancel { cid } => {
                if *busy.lock().unwrap() == Some(cid) {
                    cancel.store(true, Ordering::SeqCst);
                }
            }
            Feed::Message { cid, cmd, data } => match cmd {
                ctaphid::INIT => {
                    if data.len() != 8 {
                        send(&writer, cid, ctaphid::ERROR, &[ctaphid::ERR_INVALID_LEN]);
                        continue;
                    }
                    let new_cid = if cid == ctaphid::BROADCAST {
                        next_cid = next_cid.wrapping_add(1).max(1);
                        next_cid
                    } else {
                        cid
                    };
                    let mut resp = data.clone();
                    resp.extend_from_slice(&new_cid.to_be_bytes());
                    // protocol 2, version 0.1.0, capabilities: WINK | CBOR | NMSG
                    resp.extend_from_slice(&[2, 0, 1, 0, 0x01 | 0x04 | 0x08]);
                    send(&writer, cid, ctaphid::INIT, &resp);
                }
                _ if cid == ctaphid::BROADCAST || cid == 0 => {
                    send(&writer, cid, ctaphid::ERROR, &[ctaphid::ERR_INVALID_CHANNEL])
                }
                ctaphid::PING => send(&writer, cid, ctaphid::PING, &data),
                ctaphid::WINK => send(&writer, cid, ctaphid::WINK, &[]),
                ctaphid::CBOR => {
                    let mut b = busy.lock().unwrap();
                    if b.is_some() {
                        send(&writer, cid, ctaphid::ERROR, &[ctaphid::ERR_CHANNEL_BUSY]);
                        continue;
                    }
                    *b = Some(cid);
                    cancel.store(false, Ordering::SeqCst);
                    let (auth, writer, busy, cancel) = (auth.clone(), writer.clone(), busy.clone(), cancel.clone());
                    thread::spawn(move || {
                        let keepalive = || send(&writer, cid, ctaphid::KEEPALIVE, &[ctaphid::KEEPALIVE_UPNEEDED]);
                        let mut verify = |user: &str, reason: &str| {
                            if no_uv {
                                ctap2::Uv::Ok
                            } else {
                                face_verify(user, reason, &cancel, &keepalive)
                            }
                        };
                        let mut req = ctap2::Request { user: seat_user(), verify: &mut verify };
                        let resp = auth.lock().unwrap().handle(&data, &mut req);
                        send(&writer, cid, ctaphid::CBOR, &resp);
                        *busy.lock().unwrap() = None;
                    });
                }
                _ => send(&writer, cid, ctaphid::ERROR, &[ctaphid::ERR_INVALID_CMD]), // incl. U2F MSG (0x83): we are CTAP2 only
            },
        }
    }
    dev.destroy();
}
