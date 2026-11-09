//! CTAPHID transport framing (FIDO CTAP 2.0, section 8.1).

pub const PACKET: usize = 64;
pub const BROADCAST: u32 = 0xffff_ffff;

pub const PING: u8 = 0x81;
pub const INIT: u8 = 0x86;
pub const WINK: u8 = 0x88;
pub const CBOR: u8 = 0x90;
pub const CANCEL: u8 = 0x91;
pub const KEEPALIVE: u8 = 0xbb;
pub const ERROR: u8 = 0xbf;

pub const ERR_INVALID_CMD: u8 = 0x01;
pub const ERR_INVALID_LEN: u8 = 0x03;
pub const ERR_INVALID_SEQ: u8 = 0x04;
pub const ERR_CHANNEL_BUSY: u8 = 0x06;
pub const ERR_INVALID_CHANNEL: u8 = 0x0b;

pub const KEEPALIVE_UPNEEDED: u8 = 0x02;

const MAX_MESSAGE: usize = 57 + 128 * 59;

/// Split a message into CTAPHID packets.
pub fn frame(cid: u32, cmd: u8, data: &[u8]) -> Vec<[u8; PACKET]> {
    let mut out = Vec::new();
    let mut p = [0u8; PACKET];
    p[0..4].copy_from_slice(&cid.to_be_bytes());
    p[4] = cmd;
    p[5] = (data.len() >> 8) as u8;
    p[6] = data.len() as u8;
    let n = data.len().min(PACKET - 7);
    p[7..7 + n].copy_from_slice(&data[..n]);
    out.push(p);
    let mut off = n;
    let mut seq = 0u8;
    while off < data.len() {
        let mut p = [0u8; PACKET];
        p[0..4].copy_from_slice(&cid.to_be_bytes());
        p[4] = seq;
        let n = (data.len() - off).min(PACKET - 5);
        p[5..5 + n].copy_from_slice(&data[off..off + n]);
        out.push(p);
        off += n;
        seq += 1;
    }
    out
}

#[derive(Debug, PartialEq)]
pub enum Feed {
    Pending,
    Message { cid: u32, cmd: u8, data: Vec<u8> },
    Cancel { cid: u32 },
    Error { cid: u32, code: u8 },
}

/// Reassembles one message at a time from incoming packets.
#[derive(Default)]
pub struct Assembler {
    active: bool,
    cid: u32,
    cmd: u8,
    need: usize,
    seq: u8,
    buf: Vec<u8>,
}

impl Assembler {
    pub fn feed(&mut self, p: &[u8]) -> Feed {
        if p.len() < PACKET {
            return Feed::Pending;
        }
        let cid = u32::from_be_bytes(p[0..4].try_into().unwrap());
        if p[4] & 0x80 != 0 {
            let cmd = p[4];
            if cmd == CANCEL {
                return Feed::Cancel { cid };
            }
            if self.active && self.cid != cid {
                return Feed::Error { cid, code: ERR_CHANNEL_BUSY };
            }
            if self.active && cmd != INIT {
                // a new init packet in the middle of a message on the same channel
                self.active = false;
                return Feed::Error { cid, code: ERR_INVALID_SEQ };
            }
            let len = (p[5] as usize) << 8 | p[6] as usize;
            if len > MAX_MESSAGE {
                self.active = false;
                return Feed::Error { cid, code: ERR_INVALID_LEN };
            }
            self.cid = cid;
            self.cmd = cmd;
            self.need = len;
            self.seq = 0;
            self.buf = p[7..7 + len.min(PACKET - 7)].to_vec();
        } else {
            if !self.active || cid != self.cid {
                return Feed::Pending; // stray continuation packet
            }
            if p[4] != self.seq {
                self.active = false;
                return Feed::Error { cid, code: ERR_INVALID_SEQ };
            }
            self.seq += 1;
            let n = (self.need - self.buf.len()).min(PACKET - 5);
            self.buf.extend_from_slice(&p[5..5 + n]);
        }
        if self.buf.len() >= self.need {
            self.active = false;
            return Feed::Message { cid: self.cid, cmd: self.cmd, data: std::mem::take(&mut self.buf) };
        }
        self.active = true;
        Feed::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_long_message() {
        let data: Vec<u8> = (0..300u32).map(|i| i as u8).collect();
        let packets = frame(0x01020304, CBOR, &data);
        assert_eq!(packets.len(), 1 + (300 - 57 + 58) / 59);
        let mut a = Assembler::default();
        let mut result = Feed::Pending;
        for p in &packets {
            result = a.feed(p);
        }
        assert_eq!(result, Feed::Message { cid: 0x01020304, cmd: CBOR, data });
    }

    #[test]
    fn short_message_and_busy_channel() {
        let mut a = Assembler::default();
        let p = frame(7, PING, &[1, 2, 3]);
        assert_eq!(a.feed(&p[0]), Feed::Message { cid: 7, cmd: PING, data: vec![1, 2, 3] });

        let long = frame(7, CBOR, &[0u8; 100]);
        assert_eq!(a.feed(&long[0]), Feed::Pending);
        let other = frame(8, PING, &[9]);
        assert_eq!(a.feed(&other[0]), Feed::Error { cid: 8, code: ERR_CHANNEL_BUSY });
        assert!(matches!(a.feed(&long[1]), Feed::Message { cid: 7, .. }));
    }

    #[test]
    fn bad_sequence() {
        let mut a = Assembler::default();
        let long = frame(5, CBOR, &[0u8; 200]);
        a.feed(&long[0]);
        assert_eq!(a.feed(&long[2]), Feed::Error { cid: 5, code: ERR_INVALID_SEQ });
    }
}
