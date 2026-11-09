//! kface-fprintd: speaks the fprintd D-Bus API (net.reactivated.Fprint) and answers with face
//! verification, so pam_fprintd and every desktop's fingerprint support use the Kinect.
//!
//! The enrolled "finger" is the face stored by kface in /etc/kface/<user>.faces; the finger name
//! chosen at enrollment is remembered in /etc/kface/<user>.finger.
//!
//!   kface-fprintd            system service (root)
//!   kface-fprintd --session  development: session bus, no polkit, KFACE_BIN/KFACE_DATA honoured

use futures_util::StreamExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, Value};
use zbus::{fdo, interface, Connection, DBusError};

const SERVICE: &str = "net.reactivated.Fprint";
const MANAGER_PATH: &str = "/net/reactivated/Fprint/Manager";
const DEVICE_PATH: &str = "/net/reactivated/Fprint/Device/0";
const ENROLL_STAGES: i32 = 15;
const DEFAULT_FINGER: &str = "right-index-finger";
const FINGERS: [&str; 10] = [
    "left-thumb",
    "left-index-finger",
    "left-middle-finger",
    "left-ring-finger",
    "left-little-finger",
    "right-thumb",
    "right-index-finger",
    "right-middle-finger",
    "right-ring-finger",
    "right-little-finger",
];

#[derive(Debug, DBusError)]
#[zbus(prefix = "net.reactivated.Fprint.Error")]
enum Error {
    #[zbus(error)]
    ZBus(zbus::Error),
    PermissionDenied(String),
    AlreadyInUse(String),
    Internal(String),
    ClaimDevice(String),
    NoEnrolledPrints(String),
    NoActionInProgress(String),
    InvalidFingername(String),
    PrintsNotDeleted(String),
}

type Result<T> = std::result::Result<T, Error>;

struct Config {
    session: bool,
    kface: String,
    data: PathBuf,
}

#[derive(PartialEq, Clone, Copy)]
enum Action {
    Idle,
    Verify,
    Enroll,
}

struct State {
    claim: Option<(String, String)>, // (unique bus name, user)
    action: Action,
    child: Option<u32>,
    generation: u64,
}

impl State {
    /// stop a running scan; its completion task sees the generation change and stays silent
    fn stop(&mut self) {
        self.generation += 1;
        self.action = Action::Idle;
        if let Some(pid) = self.child.take() {
            // SIGTERM: kface closes the Kinect cleanly
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        }
    }
}

struct Manager;

#[interface(name = "net.reactivated.Fprint.Manager")]
impl Manager {
    fn get_devices(&self) -> Vec<OwnedObjectPath> {
        vec![OwnedObjectPath::try_from(DEVICE_PATH).unwrap()]
    }

    fn get_default_device(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from(DEVICE_PATH).unwrap()
    }
}

struct Device {
    cfg: Arc<Config>,
    state: Arc<Mutex<State>>,
}

fn user_name(uid: u32) -> Option<String> {
    let pw = unsafe { libc::getpwuid(uid) };
    if pw.is_null() {
        return None;
    }
    unsafe { std::ffi::CStr::from_ptr((*pw).pw_name) }.to_str().ok().map(str::to_string)
}

fn valid_user(u: &str) -> bool {
    !u.is_empty()
        && u.len() <= 64
        && !u.starts_with('.')
        && u.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

impl Device {
    fn faces_file(&self, user: &str) -> PathBuf {
        self.cfg.data.join(format!("{user}.faces"))
    }
    fn finger_file(&self, user: &str) -> PathBuf {
        self.cfg.data.join(format!("{user}.finger"))
    }
    fn enrolled(&self, user: &str) -> bool {
        std::fs::metadata(self.faces_file(user)).map(|m| m.len() > 0).unwrap_or(false)
    }
    fn finger(&self, user: &str) -> String {
        std::fs::read_to_string(self.finger_file(user))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|f| FINGERS.contains(&f.as_str()))
            .unwrap_or_else(|| DEFAULT_FINGER.to_string())
    }

    async fn caller(&self, conn: &Connection, hdr: &Header<'_>) -> Result<(String, u32)> {
        let sender = hdr.sender().ok_or_else(|| Error::Internal("no sender".into()))?.to_string();
        let uid = fdo::DBusProxy::new(conn)
            .await?
            .get_connection_unix_user(sender.as_str().try_into().map_err(zbus::Error::from)?)
            .await
            .map_err(zbus::Error::from)?;
        Ok((sender, uid))
    }

    /// polkit check for the calling bus name; root always passes
    async fn authorize(&self, conn: &Connection, sender: &str, uid: u32, action: &str) -> Result<()> {
        if uid == 0 || self.cfg.session {
            return Ok(());
        }
        let subject: (&str, HashMap<&str, Value>) = ("system-bus-name", HashMap::from([("name", Value::from(sender))]));
        let details: HashMap<&str, &str> = HashMap::new();
        let reply = conn
            .call_method(
                Some("org.freedesktop.PolicyKit1"),
                "/org/freedesktop/PolicyKit1/Authority",
                Some("org.freedesktop.PolicyKit1.Authority"),
                "CheckAuthorization",
                &(subject, action, details, 1u32, ""),
            )
            .await?;
        let (authorized, _challenge, _details): (bool, bool, HashMap<String, String>) = reply.body().deserialize()?;
        if authorized {
            Ok(())
        } else {
            Err(Error::PermissionDenied(format!("Not authorized: {action}")))
        }
    }

    /// resolve the user a request is about: "" means the caller; others need setusername
    async fn target_user(&self, conn: &Connection, sender: &str, uid: u32, username: &str) -> Result<String> {
        let me = user_name(uid).ok_or_else(|| Error::Internal("unknown caller".into()))?;
        let user = if username.is_empty() { me.clone() } else { username.to_string() };
        if !valid_user(&user) {
            return Err(Error::Internal("invalid user name".into()));
        }
        if user != me {
            self.authorize(conn, sender, uid, "net.reactivated.fprint.device.setusername").await?;
        }
        Ok(user)
    }

    fn claimed_by(&self, sender: &str) -> Result<String> {
        match &self.state.lock().unwrap().claim {
            Some((s, user)) if s == sender => Ok(user.clone()),
            Some(_) => Err(Error::AlreadyInUse("Device already in use by another user".into())),
            None => Err(Error::ClaimDevice("Device was not claimed before use".into())),
        }
    }

    fn delete(&self, user: &str) -> Result<()> {
        let mut ok = true;
        for f in [self.faces_file(user), self.finger_file(user)] {
            if let Err(e) = std::fs::remove_file(&f) {
                ok &= e.kind() == std::io::ErrorKind::NotFound;
            }
        }
        if ok {
            Ok(())
        } else {
            Err(Error::PrintsNotDeleted("Failed to delete face data".into()))
        }
    }

    fn begin(&self, action: Action) -> Result<u64> {
        let mut st = self.state.lock().unwrap();
        if st.action != Action::Idle {
            return Err(Error::AlreadyInUse("Another operation is in progress".into()));
        }
        st.action = action;
        st.generation += 1;
        Ok(st.generation)
    }

    /// record the child, unless the operation was stopped in the meantime
    fn attach(&self, gen: u64, pid: Option<u32>) -> bool {
        let mut st = self.state.lock().unwrap();
        if st.generation != gen {
            if let Some(pid) = pid {
                unsafe { libc::kill(pid as i32, libc::SIGTERM) };
            }
            return false;
        }
        st.child = pid;
        true
    }

    /// clear the action when it finished on its own; false if it was stopped
    fn finish(&self, gen: u64) -> bool {
        let mut st = self.state.lock().unwrap();
        if st.generation != gen {
            return false;
        }
        st.action = Action::Idle;
        st.child = None;
        true
    }
}

#[interface(name = "net.reactivated.Fprint.Device")]
impl Device {
    async fn list_enrolled_fingers(
        &self,
        username: &str,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> Result<Vec<String>> {
        let (sender, uid) = self.caller(conn, &hdr).await?;
        let user = self.target_user(conn, &sender, uid, username).await?;
        if !self.enrolled(&user) {
            return Err(Error::NoEnrolledPrints("No face enrolled".into()));
        }
        Ok(vec![self.finger(&user)])
    }

    async fn delete_enrolled_fingers(
        &self,
        username: &str,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> Result<()> {
        let (sender, uid) = self.caller(conn, &hdr).await?;
        let user = self.target_user(conn, &sender, uid, username).await?;
        self.authorize(conn, &sender, uid, "net.reactivated.fprint.device.enroll").await?;
        self.delete(&user)
    }

    async fn delete_enrolled_fingers2(&self, #[zbus(connection)] conn: &Connection, #[zbus(header)] hdr: Header<'_>) -> Result<()> {
        let (sender, uid) = self.caller(conn, &hdr).await?;
        let user = self.claimed_by(&sender)?;
        self.authorize(conn, &sender, uid, "net.reactivated.fprint.device.enroll").await?;
        self.delete(&user)
    }

    async fn delete_enrolled_finger(
        &self,
        finger_name: &str,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> Result<()> {
        let (sender, uid) = self.caller(conn, &hdr).await?;
        let user = self.claimed_by(&sender)?;
        if !FINGERS.contains(&finger_name) {
            return Err(Error::InvalidFingername("Invalid finger name".into()));
        }
        self.authorize(conn, &sender, uid, "net.reactivated.fprint.device.enroll").await?;
        if !self.enrolled(&user) || self.finger(&user) != finger_name {
            return Err(Error::NoEnrolledPrints("Finger not enrolled".into()));
        }
        self.delete(&user)
    }

    async fn claim(&self, username: &str, #[zbus(connection)] conn: &Connection, #[zbus(header)] hdr: Header<'_>) -> Result<()> {
        let (sender, uid) = self.caller(conn, &hdr).await?;
        let user = self.target_user(conn, &sender, uid, username).await?;
        self.authorize(conn, &sender, uid, "net.reactivated.fprint.device.verify").await?;
        let mut st = self.state.lock().unwrap();
        if st.claim.is_some() {
            return Err(Error::AlreadyInUse("Device was already claimed".into()));
        }
        st.claim = Some((sender, user));
        Ok(())
    }

    async fn release(&self, #[zbus(header)] hdr: Header<'_>) -> Result<()> {
        let sender = hdr.sender().map(|s| s.to_string()).unwrap_or_default();
        self.claimed_by(&sender)?;
        let mut st = self.state.lock().unwrap();
        st.stop();
        st.claim = None;
        Ok(())
    }

    async fn verify_start(
        &self,
        finger_name: &str,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<()> {
        let sender = hdr.sender().map(|s| s.to_string()).unwrap_or_default();
        let user = self.claimed_by(&sender)?;
        if !self.enrolled(&user) {
            return Err(Error::NoEnrolledPrints("No face enrolled".into()));
        }
        if finger_name != "any" && !FINGERS.contains(&finger_name) {
            return Err(Error::InvalidFingername("Invalid finger name".into()));
        }
        let gen = self.begin(Action::Verify)?;
        let finger = self.finger(&user);
        Device::verify_finger_selected(&emitter, if finger_name == "any" { "any" } else { &finger }).await?;

        let child = Command::new(&self.cfg.kface)
            .args(["verify", &user, "--wait"])
            .env("KFACE_REASON", "Authentication")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(false)
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                self.finish(gen);
                Device::verify_status(&emitter, "verify-disconnected", true).await?;
                return Err(Error::Internal(format!("cannot run kface: {e}")));
            }
        };
        if !self.attach(gen, child.id()) {
            return Ok(());
        }

        let (state, conn) = (self.state.clone(), conn.clone());
        let device = Device { cfg: self.cfg.clone(), state: state.clone() };
        tokio::spawn(async move {
            let status = child.wait().await;
            if !device.finish(gen) {
                return; // stopped: no result is reported
            }
            let Ok(emitter) = SignalEmitter::new(&conn, DEVICE_PATH) else { return };
            let result = match status.ok().and_then(|s| s.code()) {
                Some(0) => "verify-match",
                Some(1) => "verify-no-match",
                Some(2) => "verify-disconnected",
                _ => "verify-unknown-error",
            };
            if result == "verify-match" {
                let _ = Device::verify_finger_matched(&emitter, &finger).await;
            }
            let _ = Device::verify_status(&emitter, result, true).await;
        });
        Ok(())
    }

    async fn verify_stop(&self, #[zbus(header)] hdr: Header<'_>) -> Result<()> {
        let sender = hdr.sender().map(|s| s.to_string()).unwrap_or_default();
        self.claimed_by(&sender)?;
        let mut st = self.state.lock().unwrap();
        if st.action == Action::Enroll {
            return Err(Error::NoActionInProgress("No verification in progress".into()));
        }
        // a finished verification needs no stop; accept it like fprintd's clients expect
        st.stop();
        Ok(())
    }

    async fn enroll_start(
        &self,
        finger_name: &str,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<()> {
        let (sender, uid) = self.caller(conn, &hdr).await?;
        let user = self.claimed_by(&sender)?;
        if !FINGERS.contains(&finger_name) {
            return Err(Error::InvalidFingername("Invalid finger name".into()));
        }
        self.authorize(conn, &sender, uid, "net.reactivated.fprint.device.enroll").await?;
        let gen = self.begin(Action::Enroll)?;
        let _ = &emitter;

        let child = Command::new(&self.cfg.kface)
            .args(["enroll", &user, &ENROLL_STAGES.to_string(), "--replace"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                self.finish(gen);
                return Err(Error::Internal(format!("cannot run kface: {e}")));
            }
        };
        if !self.attach(gen, child.id()) {
            return Ok(());
        }

        let (conn, finger) = (conn.clone(), finger_name.to_string());
        let device = Device { cfg: self.cfg.clone(), state: self.state.clone() };
        let stdout = child.stdout.take();
        tokio::spawn(async move {
            let Ok(emitter) = SignalEmitter::new(&conn, DEVICE_PATH) else { return };
            if let Some(out) = stdout {
                let mut lines = BufReader::new(out).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    // "  sample 3/15"
                    if line.trim_start().starts_with("sample ") && device.state.lock().unwrap().generation == gen {
                        let _ = Device::enroll_status(&emitter, "enroll-stage-passed", false).await;
                    }
                }
            }
            let status = child.wait().await;
            if !device.finish(gen) {
                return;
            }
            let ok = status.map(|s| s.success()).unwrap_or(false);
            if ok {
                let _ = std::fs::write(device.finger_file(&user), format!("{finger}\n"));
            }
            let _ = Device::enroll_status(&emitter, if ok { "enroll-completed" } else { "enroll-failed" }, true).await;
        });
        Ok(())
    }

    async fn enroll_stop(&self, #[zbus(header)] hdr: Header<'_>) -> Result<()> {
        let sender = hdr.sender().map(|s| s.to_string()).unwrap_or_default();
        self.claimed_by(&sender)?;
        let mut st = self.state.lock().unwrap();
        if st.action == Action::Verify {
            return Err(Error::NoActionInProgress("No enrollment in progress".into()));
        }
        st.stop();
        Ok(())
    }

    #[zbus(signal)]
    async fn verify_finger_selected(emitter: &SignalEmitter<'_>, finger_name: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn verify_finger_matched(emitter: &SignalEmitter<'_>, finger_name: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn verify_status(emitter: &SignalEmitter<'_>, result: &str, done: bool) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn enroll_status(emitter: &SignalEmitter<'_>, result: &str, done: bool) -> zbus::Result<()>;

    #[zbus(property, name = "name")]
    fn name(&self) -> String {
        "Kinect v2 face (kface)".into()
    }
    #[zbus(property, name = "num-enroll-stages")]
    fn num_enroll_stages(&self) -> i32 {
        ENROLL_STAGES
    }
    #[zbus(property, name = "scan-type")]
    fn scan_type(&self) -> String {
        "press".into()
    }
    #[zbus(property, name = "finger-present")]
    fn finger_present(&self) -> bool {
        false
    }
    #[zbus(property, name = "finger-needed")]
    fn finger_needed(&self) -> bool {
        self.state.lock().unwrap().action != Action::Idle
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> zbus::Result<()> {
    let session = std::env::args().any(|a| a == "--session");
    let env = |k: &str, default: &str| {
        if session {
            std::env::var(k).unwrap_or_else(|_| default.to_string())
        } else {
            default.to_string()
        }
    };
    let cfg = Arc::new(Config {
        session,
        kface: env("KFACE_BIN", "/usr/local/bin/kface"),
        data: PathBuf::from(env("KFACE_DATA", "/etc/kface")),
    });
    let state = Arc::new(Mutex::new(State { claim: None, action: Action::Idle, child: None, generation: 0 }));

    let builder = if session { zbus::connection::Builder::session()? } else { zbus::connection::Builder::system()? };
    let conn = builder
        .name(SERVICE)?
        .serve_at(MANAGER_PATH, Manager)?
        .serve_at(DEVICE_PATH, Device { cfg: cfg.clone(), state: state.clone() })?
        .build()
        .await?;
    eprintln!("kface-fprintd: ready on the {} bus", if session { "session" } else { "system" });

    // pam_fprintd does not Release after a match, it just disconnects: free the claim then
    let dbus = fdo::DBusProxy::new(&conn).await?;
    let mut owners = dbus.receive_name_owner_changed().await?;
    while let Some(sig) = owners.next().await {
        let Ok(args) = sig.args() else { continue };
        if args.new_owner().is_some() {
            continue;
        }
        let gone = args.name().to_string();
        let mut st = state.lock().unwrap();
        if st.claim.as_ref().is_some_and(|(s, _)| *s == gone) {
            st.stop();
            st.claim = None;
        }
    }
    Ok(())
}
