//! CTAP 2.0 authenticator commands: getInfo, makeCredential, getAssertion, getNextAssertion.
//! User presence and user verification are both provided by face verification.

use crate::store::{random_bytes, Credential, KeyBlob, PublicKey, Store};
use ciborium::value::Value;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAKE_CREDENTIAL: u8 = 0x01;
pub const GET_ASSERTION: u8 = 0x02;
pub const GET_INFO: u8 = 0x04;
pub const CLIENT_PIN: u8 = 0x06;
pub const RESET: u8 = 0x07;
pub const GET_NEXT_ASSERTION: u8 = 0x08;
pub const SELECTION: u8 = 0x0b;

pub const OK: u8 = 0x00;
pub const ERR_INVALID_COMMAND: u8 = 0x01;
pub const ERR_INVALID_CBOR: u8 = 0x12;
pub const ERR_MISSING_PARAMETER: u8 = 0x14;
pub const ERR_CREDENTIAL_EXCLUDED: u8 = 0x19;
pub const ERR_UNSUPPORTED_ALGORITHM: u8 = 0x26;
pub const ERR_OPERATION_DENIED: u8 = 0x27;
pub const ERR_KEY_STORE_FULL: u8 = 0x28;
pub const ERR_INVALID_OPTION: u8 = 0x2c;
pub const ERR_KEEPALIVE_CANCEL: u8 = 0x2d;
pub const ERR_NO_CREDENTIALS: u8 = 0x2e;
pub const ERR_NOT_ALLOWED: u8 = 0x30;
pub const ERR_PIN_AUTH_INVALID: u8 = 0x33;
pub const ERR_PIN_NOT_SET: u8 = 0x35;
pub const ERR_OTHER: u8 = 0x7f;

const FLAG_UP: u8 = 0x01;
const FLAG_UV: u8 = 0x04;
const FLAG_AT: u8 = 0x40;
const ES256: i64 = -7;
const MAX_CREDENTIALS: usize = 500;

/// kface-fido AAGUID (random, fixed)
pub const AAGUID: [u8; 16] = [
    0x6b, 0x66, 0x61, 0x63, 0x65, 0x2d, 0x4b, 0x69, 0x6e, 0x65, 0x63, 0x74, 0x76, 0x32, 0x00, 0x01,
];

pub enum Uv {
    Ok,
    Denied,
    Cancelled,
}

/// Who is asking and how to verify them.
pub struct Request<'a> {
    /// user of the active seat; credentials are stored per user
    pub user: Option<String>,
    /// face verification with a human readable reason for the overlay
    pub verify: &'a mut dyn FnMut(&str, &str) -> Uv,
}

struct Pending {
    user: String,
    client_data_hash: Vec<u8>,
    flags: u8,
    creds: Vec<Credential>,
}

pub struct Authenticator {
    pub store: Store,
    next: Option<Pending>,
}

type Map = Vec<(Value, Value)>;

fn int(i: i64) -> Value {
    Value::Integer(i.into())
}
fn text(s: &str) -> Value {
    Value::Text(s.to_string())
}
fn get<'a>(m: &'a Map, key: i64) -> Option<&'a Value> {
    m.iter().find(|(k, _)| k.as_integer() == Some(key.into())).map(|(_, v)| v)
}
fn get_text<'a>(m: &'a Map, key: &str) -> Option<&'a Value> {
    m.iter().find(|(k, _)| k.as_text() == Some(key)).map(|(_, v)| v)
}
fn opt_bool(m: Option<&Value>, key: &str) -> Option<bool> {
    m.and_then(|v| v.as_map()).and_then(|o| get_text(o, key)).and_then(|v| v.as_bool())
}
fn encode(v: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    ciborium::ser::into_writer(v, &mut out).expect("cbor encode");
    out
}

fn auth_data(rp_id: &str, flags: u8, attested: Option<(&[u8], &PublicKey)>) -> Vec<u8> {
    let mut d = Sha256::digest(rp_id.as_bytes()).to_vec();
    d.push(flags | if attested.is_some() { FLAG_AT } else { 0 });
    d.extend_from_slice(&0u32.to_be_bytes()); // no signature counter
    if let Some((id, pk)) = attested {
        d.extend_from_slice(&AAGUID);
        d.extend_from_slice(&(id.len() as u16).to_be_bytes());
        d.extend_from_slice(id);
        let cose = Value::Map(vec![
            (int(1), int(2)),      // kty: EC2
            (int(3), int(ES256)),  // alg
            (int(-1), int(1)),     // crv: P-256
            (int(-2), Value::Bytes(pk.x.to_vec())),
            (int(-3), Value::Bytes(pk.y.to_vec())),
        ]);
        d.extend(encode(&cose));
    }
    d
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// the pin/uv auth parameter: we have no PIN; an empty one is a platform probe for a touch
fn check_pin_auth(v: Option<&Value>) -> Result<(), u8> {
    match v.and_then(|v| v.as_bytes()) {
        None => Ok(()),
        Some(b) if b.is_empty() => Err(ERR_PIN_NOT_SET),
        Some(_) => Err(ERR_PIN_AUTH_INVALID),
    }
}

impl Authenticator {
    pub fn new(store: Store) -> Authenticator {
        Authenticator { store, next: None }
    }

    /// Process one CTAP2 request (command byte + CBOR); returns status byte + CBOR.
    pub fn handle(&mut self, msg: &[u8], req: &mut Request) -> Vec<u8> {
        let Some((&cmd, body)) = msg.split_first() else {
            return vec![ERR_INVALID_COMMAND];
        };
        if cmd != GET_NEXT_ASSERTION {
            self.next = None;
        }
        let params = || -> Result<Map, u8> {
            let v: Value = ciborium::de::from_reader(body).map_err(|_| ERR_INVALID_CBOR)?;
            v.into_map().map_err(|_| ERR_INVALID_CBOR)
        };
        let result = match cmd {
            GET_INFO => Ok(self.get_info()),
            MAKE_CREDENTIAL => params().and_then(|p| self.make_credential(&p, req)),
            GET_ASSERTION => params().and_then(|p| self.get_assertion(&p, req)),
            GET_NEXT_ASSERTION => self.get_next_assertion(),
            SELECTION => Ok(Vec::new()),
            RESET => Err(ERR_NOT_ALLOWED),
            CLIENT_PIN => Err(ERR_INVALID_COMMAND),
            _ => Err(ERR_INVALID_COMMAND),
        };
        match result {
            Ok(body) => [vec![OK], body].concat(),
            Err(code) => vec![code],
        }
    }

    fn get_info(&self) -> Vec<u8> {
        encode(&Value::Map(vec![
            (int(1), Value::Array(vec![text("FIDO_2_0")])),
            (int(3), Value::Bytes(AAGUID.to_vec())),
            (
                int(4),
                Value::Map(vec![
                    (text("rk"), Value::Bool(true)),
                    (text("up"), Value::Bool(true)),
                    (text("uv"), Value::Bool(true)),
                    (text("plat"), Value::Bool(false)),
                ]),
            ),
            (int(5), int(1200)),
            (int(9), Value::Array(vec![text("usb")])),
            (
                int(10),
                Value::Array(vec![Value::Map(vec![
                    (text("alg"), int(ES256)),
                    (text("type"), text("public-key")),
                ])]),
            ),
        ]))
    }

    fn make_credential(&mut self, p: &Map, req: &mut Request) -> Result<Vec<u8>, u8> {
        let cdh = get(p, 1).and_then(|v| v.as_bytes()).ok_or(ERR_MISSING_PARAMETER)?.clone();
        let rp = get(p, 2).and_then(|v| v.as_map()).ok_or(ERR_MISSING_PARAMETER)?;
        let rp_id = get_text(rp, "id").and_then(|v| v.as_text()).ok_or(ERR_MISSING_PARAMETER)?.to_string();
        let user = get(p, 3).and_then(|v| v.as_map()).ok_or(ERR_MISSING_PARAMETER)?;
        let user_id = get_text(user, "id").and_then(|v| v.as_bytes()).ok_or(ERR_MISSING_PARAMETER)?.clone();
        let user_name = get_text(user, "name").and_then(|v| v.as_text()).unwrap_or("").to_string();
        let display_name = get_text(user, "displayName").and_then(|v| v.as_text()).unwrap_or("").to_string();
        let algs = get(p, 4).and_then(|v| v.as_array()).ok_or(ERR_MISSING_PARAMETER)?;
        check_pin_auth(get(p, 8))?;

        let es256 = algs.iter().filter_map(|a| a.as_map()).any(|a| {
            get_text(a, "alg").and_then(|v| v.as_integer()) == Some(ES256.into())
                && get_text(a, "type").and_then(|v| v.as_text()) == Some("public-key")
        });
        if !es256 {
            return Err(ERR_UNSUPPORTED_ALGORITHM);
        }
        let options = get(p, 7);
        if opt_bool(options, "up") == Some(false) {
            return Err(ERR_INVALID_OPTION);
        }
        let resident = opt_bool(options, "rk").unwrap_or(false);

        let account = req.user.clone().ok_or(ERR_OPERATION_DENIED)?;
        let mut creds = self.store.load(&account);

        let excluded = get(p, 5).and_then(|v| v.as_array()).is_some_and(|list| {
            list.iter().filter_map(|c| c.as_map()).filter_map(|c| get_text(c, "id")?.as_bytes()).any(|id| {
                creds.iter().any(|c| c.rp_id == rp_id && &c.id_bytes() == id)
            })
        });

        let reason = if excluded { format!("Already registered: {rp_id}") } else { format!("Create passkey: {rp_id}") };
        match (req.verify)(&account, &reason) {
            Uv::Ok => {}
            Uv::Denied => return Err(ERR_OPERATION_DENIED),
            Uv::Cancelled => return Err(ERR_KEEPALIVE_CANCEL),
        }
        if excluded {
            return Err(ERR_CREDENTIAL_EXCLUDED);
        }
        if creds.len() >= MAX_CREDENTIALS {
            return Err(ERR_KEY_STORE_FULL);
        }

        let (key, pk) = self.store.new_key().map_err(|e| {
            eprintln!("kface-fido: {e}");
            ERR_OTHER
        })?;
        let id = random_bytes(32);
        let ad = auth_data(&rp_id, FLAG_UP | FLAG_UV, Some((&id, &pk)));

        // a new discoverable credential replaces the old one for the same account
        if resident {
            creds.retain(|c| !(c.resident && c.rp_id == rp_id && c.user_id_bytes() == user_id));
        }
        creds.push(Credential {
            id: hex::encode(&id),
            rp_id,
            user_id: hex::encode(&user_id),
            user_name,
            display_name,
            resident,
            created: now(),
            key,
        });
        self.store.save(&account, &creds).map_err(|e| {
            eprintln!("kface-fido: saving credentials: {e}");
            ERR_OTHER
        })?;

        // packed self-attestation: the new credential key signs authData || clientDataHash
        let blob = creds.last().unwrap().key.clone();
        let sig = self.store.sign(&blob, &[ad.as_slice(), &cdh].concat()).map_err(|e| {
            eprintln!("kface-fido: {e}");
            ERR_OTHER
        })?;
        Ok(encode(&Value::Map(vec![
            (int(1), text("packed")),
            (int(2), Value::Bytes(ad)),
            (int(3), Value::Map(vec![(text("alg"), int(ES256)), (text("sig"), Value::Bytes(sig))])),
        ])))
    }

    fn get_assertion(&mut self, p: &Map, req: &mut Request) -> Result<Vec<u8>, u8> {
        let rp_id = get(p, 1).and_then(|v| v.as_text()).ok_or(ERR_MISSING_PARAMETER)?.to_string();
        let cdh = get(p, 2).and_then(|v| v.as_bytes()).ok_or(ERR_MISSING_PARAMETER)?.clone();
        check_pin_auth(get(p, 6))?;
        let options = get(p, 5);
        let up = opt_bool(options, "up").unwrap_or(true);
        let uv = opt_bool(options, "uv").unwrap_or(false);

        let account = req.user.clone().ok_or(ERR_OPERATION_DENIED)?;
        let all = self.store.load(&account);
        let allow: Option<Vec<Vec<u8>>> = get(p, 3).and_then(|v| v.as_array()).map(|list| {
            list.iter().filter_map(|c| c.as_map()).filter_map(|c| get_text(c, "id")?.as_bytes().cloned()).collect()
        });
        let mut creds: Vec<Credential> = all
            .into_iter()
            .filter(|c| c.rp_id == rp_id)
            .filter(|c| match &allow {
                Some(ids) if !ids.is_empty() => ids.contains(&c.id_bytes()),
                _ => c.resident,
            })
            .collect();
        if creds.is_empty() {
            return Err(ERR_NO_CREDENTIALS);
        }
        creds.sort_by(|a, b| b.created.cmp(&a.created)); // most recent first

        let mut flags = 0;
        if up || uv {
            match (req.verify)(&account, &format!("Sign in: {rp_id}")) {
                Uv::Ok => flags = FLAG_UP | FLAG_UV,
                Uv::Denied => return Err(ERR_OPERATION_DENIED),
                Uv::Cancelled => return Err(ERR_KEEPALIVE_CANCEL),
            }
        }

        let with_list = matches!(&allow, Some(ids) if !ids.is_empty());
        let total = creds.len();
        let first = creds.remove(0);
        let out = self.assertion(&first, &cdh, flags, if with_list { None } else { Some(total) }, !with_list)?;
        if !with_list && !creds.is_empty() {
            self.next = Some(Pending { user: account, client_data_hash: cdh, flags, creds });
        }
        Ok(out)
    }

    fn get_next_assertion(&mut self) -> Result<Vec<u8>, u8> {
        let mut pending = self.next.take().ok_or(ERR_NOT_ALLOWED)?;
        if pending.creds.is_empty() {
            return Err(ERR_NOT_ALLOWED);
        }
        let cred = pending.creds.remove(0);
        let _ = &pending.user;
        let out = self.assertion(&cred, &pending.client_data_hash, pending.flags, None, true)?;
        if !pending.creds.is_empty() {
            self.next = Some(pending);
        }
        Ok(out)
    }

    fn assertion(&mut self, c: &Credential, cdh: &[u8], flags: u8, count: Option<usize>, with_user: bool) -> Result<Vec<u8>, u8> {
        let ad = auth_data(&c.rp_id, flags, None);
        let signed = [ad.as_slice(), cdh].concat();
        let key: &KeyBlob = &c.key;
        let sig = self.store.sign(key, &signed).map_err(|e| {
            eprintln!("kface-fido: {e}");
            ERR_OTHER
        })?;
        let mut m = vec![
            (
                int(1),
                Value::Map(vec![(text("id"), Value::Bytes(c.id_bytes())), (text("type"), text("public-key"))]),
            ),
            (int(2), Value::Bytes(ad)),
            (int(3), Value::Bytes(sig)),
        ];
        if with_user && c.resident {
            let mut u = vec![(text("id"), Value::Bytes(c.user_id_bytes()))];
            if !c.user_name.is_empty() {
                u.push((text("name"), text(&c.user_name)));
            }
            if !c.display_name.is_empty() {
                u.push((text("displayName"), text(&c.display_name)));
            }
            m.push((int(4), Value::Map(u)));
        }
        if let Some(n) = count.filter(|&n| n > 1) {
            m.push((int(5), int(n as i64)));
        }
        Ok(encode(&Value::Map(m)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Backend;
    use p256::ecdsa::{signature::Verifier, Signature, VerifyingKey};

    fn authenticator() -> (Authenticator, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("kface-fido-test-{}-{}", std::process::id(), hex::encode(random_bytes(4))));
        (Authenticator::new(Store::new(dir.clone(), Backend::Soft)), dir)
    }

    fn run(a: &mut Authenticator, cmd: u8, params: Value, face: bool) -> (u8, Option<Map>) {
        let mut verify = |_: &str, _: &str| if face { Uv::Ok } else { Uv::Denied };
        let mut req = Request { user: Some("alice".into()), verify: &mut verify };
        let msg = [vec![cmd], encode(&params)].concat();
        let out = a.handle(&msg, &mut req);
        let body = (out.len() > 1).then(|| ciborium::de::from_reader::<Value, _>(&out[1..]).unwrap().into_map().unwrap());
        (out[0], body)
    }

    fn make_params(rk: bool, exclude: Vec<Vec<u8>>) -> Value {
        let mut m = vec![
            (int(1), Value::Bytes(vec![7; 32])),
            (int(2), Value::Map(vec![(text("id"), text("example.com"))])),
            (
                int(3),
                Value::Map(vec![(text("id"), Value::Bytes(vec![1, 2, 3])), (text("name"), text("alice@example.com"))]),
            ),
            (
                int(4),
                Value::Array(vec![Value::Map(vec![(text("alg"), int(-7)), (text("type"), text("public-key"))])]),
            ),
        ];
        if !exclude.is_empty() {
            let list = exclude
                .into_iter()
                .map(|id| Value::Map(vec![(text("id"), Value::Bytes(id)), (text("type"), text("public-key"))]))
                .collect();
            m.push((int(5), Value::Array(list)));
        }
        m.push((int(7), Value::Map(vec![(text("rk"), Value::Bool(rk))])));
        Value::Map(m)
    }

    // parse attested credential data out of authData: (credential id, verifying key)
    fn attested(ad: &[u8]) -> (Vec<u8>, VerifyingKey) {
        assert_eq!(ad[32] & (FLAG_UP | FLAG_UV | FLAG_AT), FLAG_UP | FLAG_UV | FLAG_AT);
        let n = u16::from_be_bytes([ad[53], ad[54]]) as usize;
        let id = ad[55..55 + n].to_vec();
        let cose: Value = ciborium::de::from_reader(&ad[55 + n..]).unwrap();
        let cose = cose.into_map().unwrap();
        let x = get(&cose, -2).unwrap().as_bytes().unwrap().clone();
        let y = get(&cose, -3).unwrap().as_bytes().unwrap().clone();
        let point = p256::EncodedPoint::from_affine_coordinates(x.as_slice().into(), y.as_slice().into(), false);
        (id, VerifyingKey::from_encoded_point(&point).unwrap())
    }

    #[test]
    fn register_then_sign_in_with_passkey() {
        let (mut a, dir) = authenticator();
        let (st, body) = run(&mut a, MAKE_CREDENTIAL, make_params(true, vec![]), true);
        assert_eq!(st, OK);
        let body = body.unwrap();
        assert_eq!(get(&body, 1).unwrap().as_text(), Some("packed"));
        let (id, vk) = attested(get(&body, 2).unwrap().as_bytes().unwrap());
        // self attestation verifies with the credential's own key
        let att = get(&body, 3).unwrap().as_map().unwrap();
        let att_sig = Signature::from_der(get_text(att, "sig").unwrap().as_bytes().unwrap()).unwrap();
        let att_data = [get(&body, 2).unwrap().as_bytes().unwrap().as_slice(), &[7u8; 32]].concat();
        assert!(vk.verify(&att_data, &att_sig).is_ok());

        // discoverable: no allow list
        let params = Value::Map(vec![(int(1), text("example.com")), (int(2), Value::Bytes(vec![9; 32]))]);
        let (st, body) = run(&mut a, GET_ASSERTION, params, true);
        assert_eq!(st, OK);
        let body = body.unwrap();
        let cred = get(&body, 1).unwrap().as_map().unwrap();
        assert_eq!(get_text(cred, "id").unwrap().as_bytes().unwrap(), &id);
        let ad = get(&body, 2).unwrap().as_bytes().unwrap();
        assert_eq!(ad[32], FLAG_UP | FLAG_UV);
        let sig = Signature::from_der(get(&body, 3).unwrap().as_bytes().unwrap()).unwrap();
        let signed = [ad.as_slice(), &[9u8; 32]].concat();
        assert!(vk.verify(&signed, &sig).is_ok());
        let user = get(&body, 4).unwrap().as_map().unwrap();
        assert_eq!(get_text(user, "id").unwrap().as_bytes().unwrap(), &vec![1, 2, 3]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn face_rejected_means_no_credential_and_no_signature() {
        let (mut a, dir) = authenticator();
        assert_eq!(run(&mut a, MAKE_CREDENTIAL, make_params(true, vec![]), false).0, ERR_OPERATION_DENIED);
        assert!(a.store.load("alice").is_empty());

        assert_eq!(run(&mut a, MAKE_CREDENTIAL, make_params(true, vec![]), true).0, OK);
        let params = Value::Map(vec![(int(1), text("example.com")), (int(2), Value::Bytes(vec![9; 32]))]);
        let (st, body) = run(&mut a, GET_ASSERTION, params, false);
        assert_eq!(st, ERR_OPERATION_DENIED);
        assert!(body.is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn non_resident_needs_allow_list_and_other_sites_see_nothing() {
        let (mut a, dir) = authenticator();
        let (_, body) = run(&mut a, MAKE_CREDENTIAL, make_params(false, vec![]), true);
        let (id, _) = attested(get(&body.unwrap(), 2).unwrap().as_bytes().unwrap());

        let discover = Value::Map(vec![(int(1), text("example.com")), (int(2), Value::Bytes(vec![0; 32]))]);
        assert_eq!(run(&mut a, GET_ASSERTION, discover, true).0, ERR_NO_CREDENTIALS);

        let allow = |rp: &str| {
            Value::Map(vec![
                (int(1), text(rp)),
                (int(2), Value::Bytes(vec![0; 32])),
                (
                    int(3),
                    Value::Array(vec![Value::Map(vec![(text("id"), Value::Bytes(id.clone())), (text("type"), text("public-key"))])]),
                ),
            ])
        };
        assert_eq!(run(&mut a, GET_ASSERTION, allow("example.com"), true).0, OK);
        assert_eq!(run(&mut a, GET_ASSERTION, allow("evil.example"), true).0, ERR_NO_CREDENTIALS);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn exclude_list_and_multiple_accounts() {
        let (mut a, dir) = authenticator();
        let (_, body) = run(&mut a, MAKE_CREDENTIAL, make_params(true, vec![]), true);
        let (id, _) = attested(get(&body.unwrap(), 2).unwrap().as_bytes().unwrap());
        assert_eq!(run(&mut a, MAKE_CREDENTIAL, make_params(true, vec![id]), true).0, ERR_CREDENTIAL_EXCLUDED);

        // second account on the same site
        let mut p = make_params(true, vec![]).into_map().unwrap();
        p[2].1 = Value::Map(vec![(text("id"), Value::Bytes(vec![4, 5, 6])), (text("name"), text("bob"))]);
        assert_eq!(run(&mut a, MAKE_CREDENTIAL, Value::Map(p), true).0, OK);

        let discover = Value::Map(vec![(int(1), text("example.com")), (int(2), Value::Bytes(vec![0; 32]))]);
        let (st, body) = run(&mut a, GET_ASSERTION, discover, true);
        assert_eq!(st, OK);
        assert_eq!(get(&body.unwrap(), 5).unwrap().as_integer(), Some(2.into()));
        let (st, body) = run(&mut a, GET_NEXT_ASSERTION, Value::Null, true);
        assert_eq!(st, OK);
        assert!(get(&body.unwrap(), 4).is_some());
        assert_eq!(run(&mut a, GET_NEXT_ASSERTION, Value::Null, true).0, ERR_NOT_ALLOWED);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn only_es256_and_no_pin() {
        let (mut a, dir) = authenticator();
        let mut p = make_params(true, vec![]).into_map().unwrap();
        p[3].1 = Value::Array(vec![Value::Map(vec![(text("alg"), int(-257)), (text("type"), text("public-key"))])]);
        assert_eq!(run(&mut a, MAKE_CREDENTIAL, Value::Map(p), true).0, ERR_UNSUPPORTED_ALGORITHM);

        let mut p = make_params(true, vec![]).into_map().unwrap();
        p.push((int(8), Value::Bytes(vec![])));
        assert_eq!(run(&mut a, MAKE_CREDENTIAL, Value::Map(p), true).0, ERR_PIN_NOT_SET);
        std::fs::remove_dir_all(dir).ok();
    }
}
