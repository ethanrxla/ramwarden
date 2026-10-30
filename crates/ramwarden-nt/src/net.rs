//! Listening and established TCP sockets per process.
//!
//! Feeds the same "serving" signal as Linux: a process holding a listening
//! socket may have clients depending on it, so the ladder spares it unless the
//! user explicitly watchlisted it.

use std::collections::HashMap;
use std::ffi::c_void;

use windows::Win32::NetworkManagement::IpHelper::{GetExtendedTcpTable, TCP_TABLE_OWNER_PID_ALL};
use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6};

const MIB_TCP_STATE_LISTEN: u32 = 2;
const MIB_TCP_STATE_ESTAB: u32 = 5;

/// Sockets grouped by owning PID.
#[derive(Clone, Debug, Default)]
pub struct Sockets {
    listening: HashMap<u32, Vec<u16>>,
    established: HashMap<u32, u32>,
}

impl Sockets {
    pub fn ports_for(&self, pid: u32) -> &[u16] {
        self.listening.get(&pid).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn established_for(&self, pid: u32) -> u32 {
        self.established.get(&pid).copied().unwrap_or(0)
    }

    fn add(&mut self, state: u32, port_field: u32, pid: u32) {
        match state {
            MIB_TCP_STATE_LISTEN => {
                // The port is in network byte order in the low 16 bits.
                let port = u16::from_be((port_field & 0xFFFF) as u16);
                let ports = self.listening.entry(pid).or_default();
                if !ports.contains(&port) {
                    ports.push(port);
                }
            }
            MIB_TCP_STATE_ESTAB => *self.established.entry(pid).or_default() += 1,
            _ => {}
        }
    }
}

/// Every TCP socket on the machine, IPv4 and IPv6.
pub fn sockets() -> Sockets {
    let mut out = Sockets::default();
    if let Some(buf) = table(AF_INET.0 as u32) {
        // MIB_TCPTABLE_OWNER_PID: u32 count, then rows of 6 u32:
        // state, local addr, local port, remote addr, remote port, pid.
        let words: &[u32] = &buf;
        let n = words.first().copied().unwrap_or(0) as usize;
        for row in words[1..].chunks_exact(6).take(n) {
            out.add(row[0], row[2], row[5]);
        }
    }
    if let Some(buf) = table(AF_INET6.0 as u32) {
        // MIB_TCP6TABLE_OWNER_PID: u32 count, then rows of 14 u32:
        // local addr (4), scope, local port, remote addr (4), scope,
        // remote port, state, pid.
        let words: &[u32] = &buf;
        let n = words.first().copied().unwrap_or(0) as usize;
        for row in words[1..].chunks_exact(14).take(n) {
            out.add(row[12], row[5], row[13]);
        }
    }
    out
}

/// Fetch one address family's owner-PID table as raw 32-bit words.
fn table(family: u32) -> Option<Vec<u32>> {
    let mut size = 0u32;
    // SAFETY: sizing call with no buffer.
    unsafe {
        GetExtendedTcpTable(None, &mut size, false, family, TCP_TABLE_OWNER_PID_ALL, 0);
    }
    for _ in 0..4 {
        size += 4096;
        let mut buf = vec![0u32; size as usize / 4];
        // SAFETY: the buffer is `size` bytes and outlives the call.
        let rc = unsafe {
            GetExtendedTcpTable(
                Some(buf.as_mut_ptr() as *mut c_void),
                &mut size,
                false,
                family,
                TCP_TABLE_OWNER_PID_ALL,
                0,
            )
        };
        if rc == 0 {
            return Some(buf);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn a_listener_in_this_process_is_attributed_to_it() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let s = sockets();
        assert!(
            s.ports_for(std::process::id()).contains(&port),
            "port {port} not found for this process: {:?}",
            s.ports_for(std::process::id())
        );
    }

    #[test]
    fn listening_ports_are_decoded_from_network_order() {
        let mut s = Sockets::default();
        // Port 6463 (Discord RPC) as the API stores it.
        s.add(MIB_TCP_STATE_LISTEN, u16::to_be(6463) as u32, 42);
        assert_eq!(s.ports_for(42), &[6463]);
    }
}
