//! Minimal /dev/uhid wrapper: creates a HID device whose reports we produce and consume.
//! Layout follows <linux/uhid.h> (struct uhid_event is packed).

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};

const UHID_DESTROY: u32 = 1;
const UHID_OUTPUT: u32 = 6;
const UHID_GET_REPORT: u32 = 9;
const UHID_GET_REPORT_REPLY: u32 = 10;
const UHID_CREATE2: u32 = 11;
const UHID_INPUT2: u32 = 12;
const UHID_SET_REPORT: u32 = 13;
const UHID_SET_REPORT_REPLY: u32 = 14;

const EVENT_SIZE: usize = 4 + 4372; // type + largest union member (uhid_create2_req)
const BUS_USB: u16 = 0x03;

pub enum Event {
    /// host wrote an output report (CTAPHID packet)
    Output(Vec<u8>),
    Other,
}

pub struct Device {
    file: File,
}

impl Device {
    pub fn create(name: &str, vendor: u32, product: u32, descriptor: &[u8]) -> io::Result<Device> {
        let mut file = OpenOptions::new().read(true).write(true).open("/dev/uhid")?;
        let mut ev = vec![0u8; EVENT_SIZE];
        ev[0..4].copy_from_slice(&UHID_CREATE2.to_ne_bytes());
        let req = &mut ev[4..];
        let n = name.len().min(127);
        req[..n].copy_from_slice(&name.as_bytes()[..n]);
        req[128..128 + 10].copy_from_slice(b"kface-fido");
        req[256..258].copy_from_slice(&(descriptor.len() as u16).to_ne_bytes());
        req[258..260].copy_from_slice(&BUS_USB.to_ne_bytes());
        req[260..264].copy_from_slice(&vendor.to_ne_bytes());
        req[264..268].copy_from_slice(&product.to_ne_bytes());
        req[268..272].copy_from_slice(&1u32.to_ne_bytes()); // version
        req[276..276 + descriptor.len()].copy_from_slice(descriptor);
        file.write_all(&ev)?;
        Ok(Device { file })
    }

    pub fn try_clone(&self) -> io::Result<Device> {
        Ok(Device { file: self.file.try_clone()? })
    }

    pub fn read(&mut self) -> io::Result<Event> {
        let mut ev = vec![0u8; EVENT_SIZE];
        let n = self.file.read(&mut ev)?;
        if n < 4 {
            return Ok(Event::Other);
        }
        let kind = u32::from_ne_bytes(ev[0..4].try_into().unwrap());
        match kind {
            UHID_OUTPUT => {
                // struct uhid_output_req { __u8 data[4096]; __u16 size; __u8 rtype; }
                let size = u16::from_ne_bytes(ev[4 + 4096..4 + 4098].try_into().unwrap()) as usize;
                Ok(Event::Output(ev[4..4 + size.min(4096)].to_vec()))
            }
            UHID_GET_REPORT | UHID_SET_REPORT => {
                // we have no feature reports; answer with an error so the caller does not hang
                let id = u32::from_ne_bytes(ev[4..8].try_into().unwrap());
                let reply = if kind == UHID_GET_REPORT { UHID_GET_REPORT_REPLY } else { UHID_SET_REPORT_REPLY };
                let mut r = vec![0u8; EVENT_SIZE];
                r[0..4].copy_from_slice(&reply.to_ne_bytes());
                r[4..8].copy_from_slice(&id.to_ne_bytes());
                r[8..10].copy_from_slice(&(libc::EIO as u16).to_ne_bytes());
                self.file.write_all(&r)?;
                Ok(Event::Other)
            }
            _ => Ok(Event::Other),
        }
    }

    /// send an input report (one 64 byte CTAPHID packet)
    pub fn input(&mut self, data: &[u8]) -> io::Result<()> {
        let mut ev = vec![0u8; 4 + 2 + data.len()];
        ev[0..4].copy_from_slice(&UHID_INPUT2.to_ne_bytes());
        ev[4..6].copy_from_slice(&(data.len() as u16).to_ne_bytes());
        ev[6..].copy_from_slice(data);
        self.file.write_all(&ev)
    }

    pub fn destroy(&mut self) {
        let mut ev = vec![0u8; 4];
        ev[0..4].copy_from_slice(&UHID_DESTROY.to_ne_bytes());
        let _ = self.file.write_all(&ev);
    }
}
