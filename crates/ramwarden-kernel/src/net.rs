//! Which processes are serving, and which are merely connected.
//!
//! A listening socket is the strongest "do not touch" signal RamWarden has that
//! does not depend on a hardcoded name. v1's great realisation was that a dev
//! server nobody wrote down is still a server: suspend it and every client on
//! the other end hangs. This module reproduces that signal without `psutil`.
//!
//! # Why only TCP listeners count
//!
//! `/proc/net/udp` reports unconnected UDP sockets in state `07`, and almost
//! every process on a desktop holds one for DNS. Treating those as "serving"
//! would mark nearly the whole system protected and leave the ladder with
//! nothing to reclaim. Only TCP `LISTEN` is a reliable indicator that something
//! is waiting to be depended upon.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use crate::{Error, Result, Root};

/// TCP state values as `/proc/net/tcp` spells them.
const TCP_ESTABLISHED: &str = "01";
const TCP_LISTEN: &str = "0A";

/// Per-process socket facts for one tick.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sockets {
    /// pid → ports it is listening on, sorted and deduplicated.
    pub listening: HashMap<i32, Vec<u16>>,
    /// pid → number of established connections. A weaker signal than listening:
    /// it says something is talking, not that anything depends on it.
    pub established: HashMap<i32, u32>,
}

impl Sockets {
    pub fn ports_for(&self, pid: i32) -> &[u16] {
        self.listening.get(&pid).map_or(&[], |v| v.as_slice())
    }

    pub fn established_for(&self, pid: i32) -> u32 {
        self.established.get(&pid).copied().unwrap_or(0)
    }

    pub fn is_serving(&self, pid: i32) -> bool {
        self.listening.contains_key(&pid)
    }
}

/// One row's inode and, for listeners, the local port.
#[derive(Debug, PartialEq, Eq)]
struct Row {
    inode: u64,
    port: u16,
    listening: bool,
}

/// Parse `/proc/net/tcp` or `/proc/net/tcp6`.
fn parse_table(text: &str, path: &Path) -> Result<Vec<Row>> {
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        // sl local rem st tx:rx tr:when retrnsmt uid timeout inode
        if f.len() < 10 {
            continue;
        }
        let state = f[3];
        let listening = state == TCP_LISTEN;
        if !listening && state != TCP_ESTABLISHED {
            continue;
        }

        let Some((_, port_hex)) = f[1].rsplit_once(':') else {
            continue;
        };
        let Ok(port) = u16::from_str_radix(port_hex, 16) else {
            continue;
        };
        let inode: u64 = f[9]
            .parse()
            .map_err(|_| Error::parse(path, format!("inode {:?} is not an integer", f[9])))?;

        out.push(Row {
            inode,
            port,
            listening,
        });
    }
    Ok(out)
}

/// Map socket inodes to the processes holding them.
///
/// Walking every process's file descriptors is the expensive part of this
/// module, so only inodes we actually care about are looked up, and the caller
/// is expected to cache the result across several ticks — listening ports change
/// far more slowly than memory pressure does.
fn owners(root: &Root, wanted: &HashSet<u64>) -> HashMap<u64, i32> {
    let mut out = HashMap::new();
    if wanted.is_empty() {
        return out;
    }

    let Ok(entries) = fs::read_dir(root.join("proc")) else {
        return out;
    };

    for e in entries.flatten() {
        let name = e.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.parse::<i32>().ok()) else {
            continue;
        };
        // Another user's process, or one that just exited: not ours to inspect.
        let Ok(fds) = fs::read_dir(root.proc_pid(pid, "fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            let Ok(target) = fs::read_link(fd.path()) else {
                continue;
            };
            let Some(inode) = socket_inode(&target.to_string_lossy()) else {
                continue;
            };
            if wanted.contains(&inode) {
                out.insert(inode, pid);
            }
        }
    }
    out
}

/// `"socket:[42623807]"` -> `42623807`.
fn socket_inode(link: &str) -> Option<u64> {
    link.strip_prefix("socket:[")?.strip_suffix(']')?.parse().ok()
}

/// Collect listening ports and established counts for every process.
pub fn sockets(root: &Root) -> Result<Sockets> {
    let mut rows = Vec::new();
    // tcp6 is absent on an IPv6-less kernel; a missing table is not an error.
    for table in ["proc/net/tcp", "proc/net/tcp6"] {
        let path = root.join(table);
        match fs::read_to_string(&path) {
            Ok(text) => rows.extend(parse_table(&text, &path)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(Error::io(&path, e)),
        }
    }

    let wanted: HashSet<u64> = rows.iter().map(|r| r.inode).collect();
    let owner = owners(root, &wanted);

    let mut out = Sockets::default();
    for r in rows {
        let Some(&pid) = owner.get(&r.inode) else {
            // A socket in no process's fd table: a kernel-side TIME_WAIT
            // remnant, or one owned by another user.
            continue;
        };
        if r.listening {
            out.listening.entry(pid).or_default().push(r.port);
        } else {
            *out.established.entry(pid).or_default() += 1;
        }
    }

    for ports in out.listening.values_mut() {
        ports.sort_unstable();
        ports.dedup();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    /// Verbatim header and rows shaped like /proc/net/tcp on the target machine.
    const TCP: &str = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode                                                     
   0: 0100007F:DECF 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 42623807 1 0000000000000000 100 0 0 10 0                  
   1: 0100007F:1E93 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 60881301 2 0000000000000000 100 0 0 10 0                  
   2: 0100007F:A1B2 0100007F:1E93 01 00000000:00000000 00:00000000 00000000  1000        0 60881302 1 0000000000000000 20 4 30 10 -1                 
   3: 0100007F:A1B3 0100007F:1E93 01 00000000:00000000 00:00000000 00000000  1000        0 60881303 1 0000000000000000 20 4 30 10 -1                 
   4: 0100007F:A1B4 0100007F:1E93 06 00000000:00000000 00:00000000 00000000  1000        0 60881304 1 0000000000000000 20 4 30 10 -1                 
";

    fn p() -> PathBuf {
        PathBuf::from("/proc/net/tcp")
    }

    #[test]
    fn parses_listeners_and_established_but_skips_other_states() {
        let rows = parse_table(TCP, &p()).unwrap();
        // The TIME_WAIT row (state 06) is dropped.
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0], Row { inode: 42_623_807, port: 0xDECF, listening: true });
        assert_eq!(rows[1].port, 0x1E93);
        assert!(rows[1].listening);
        assert!(!rows[2].listening);
    }

    /// The queue and timer columns are colon-joined pairs, so `inode` sits at
    /// index 9. Writing them as separate columns shifts `inode` to the uid and
    /// the row silently loses its owner — a fixture bug that looks like a pass.
    #[test]
    fn inode_is_read_from_the_tenth_whitespace_column() {
        let row = TCP.lines().nth(1).unwrap();
        let f: Vec<&str> = row.split_whitespace().collect();
        assert_eq!(f[4], "00000000:00000000", "tx_queue:rx_queue is one column");
        assert_eq!(f[5], "00:00000000", "tr:tm->when is one column");
        assert_eq!(f[9], "42623807", "inode");
    }

    #[test]
    fn decodes_the_hex_port() {
        let rows = parse_table(TCP, &p()).unwrap();
        assert_eq!(rows[1].port, 7827, "0x1E93");
    }

    #[test]
    fn skips_the_header_and_tolerates_short_lines() {
        let rows = parse_table("sl local rem st\ngarbage\n   0: x\n", &p()).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn extracts_a_socket_inode_from_an_fd_link() {
        assert_eq!(socket_inode("socket:[42623807]"), Some(42_623_807));
        assert_eq!(socket_inode("/home/user/file.txt"), None);
        assert_eq!(socket_inode("pipe:[12345]"), None);
        assert_eq!(socket_inode("socket:[notanumber]"), None);
    }

    /// Build a synthetic /proc where pid 300 serves on two ports and pid 301
    /// merely holds two established connections.
    fn fixture() -> (tempfile::TempDir, Root) {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        fs::create_dir_all(root.join("proc/net")).unwrap();
        fs::write(root.join("proc/net/tcp"), TCP).unwrap();

        let link = |pid: i32, fd: &str, target: &str| {
            let d = root.join(format!("proc/{pid}/fd"));
            fs::create_dir_all(&d).unwrap();
            symlink(target, d.join(fd)).unwrap();
        };

        link(300, "3", "socket:[42623807]");
        link(300, "4", "socket:[60881301]");
        link(300, "5", "/home/user/notes.txt");
        link(301, "3", "socket:[60881302]");
        link(301, "4", "socket:[60881303]");
        link(301, "5", "pipe:[999]");
        (dir, root)
    }

    #[test]
    fn attributes_listening_ports_to_the_owning_process() {
        let (_d, root) = fixture();
        let s = sockets(&root).unwrap();
        assert_eq!(s.ports_for(300), &[0x1E93, 0xDECF], "sorted");
        assert!(s.is_serving(300));
    }

    #[test]
    fn counts_established_connections_without_calling_them_serving() {
        let (_d, root) = fixture();
        let s = sockets(&root).unwrap();
        assert_eq!(s.established_for(301), 2);
        assert!(!s.is_serving(301), "connected is not the same as serving");
        assert_eq!(s.ports_for(301), &[] as &[u16]);
    }

    #[test]
    fn a_process_holding_no_sockets_reports_nothing() {
        let (_d, root) = fixture();
        let s = sockets(&root).unwrap();
        assert!(!s.is_serving(999));
        assert_eq!(s.established_for(999), 0);
        assert_eq!(s.ports_for(999), &[] as &[u16]);
    }

    #[test]
    fn non_socket_descriptors_are_ignored() {
        let (_d, root) = fixture();
        // pid 300's fd 5 is a regular file and must not become a port.
        assert_eq!(sockets(&root).unwrap().ports_for(300).len(), 2);
    }

    #[test]
    fn a_socket_owned_by_no_visible_process_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        fs::create_dir_all(root.join("proc/net")).unwrap();
        fs::write(root.join("proc/net/tcp"), TCP).unwrap();
        // No /proc/<pid>/fd anywhere: another user's sockets.
        let s = sockets(&root).unwrap();
        assert!(s.listening.is_empty());
        assert!(s.established.is_empty());
    }

    #[test]
    fn a_missing_tcp6_table_is_not_an_error() {
        let (_d, root) = fixture();
        assert!(!root.join("proc/net/tcp6").exists());
        assert!(sockets(&root).is_ok());
    }

    #[test]
    fn ipv6_listeners_are_merged_with_ipv4() {
        let (_d, root) = fixture();
        fs::write(
            root.join("proc/net/tcp6"),
            "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   \
             0: 00000000000000000000000000000000:0050 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 42623807 1 0000000000000000 100 0 0 10 0\n",
        )
        .unwrap();
        let s = sockets(&root).unwrap();
        // inode 42623807 is pid 300's, now also seen listening on port 0x50.
        assert!(s.ports_for(300).contains(&80));
    }

    #[test]
    fn duplicate_rows_for_one_port_are_deduplicated() {
        let (_d, root) = fixture();
        let dup = format!(
            "{TCP}   5: 0100007F:DECF 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 42623807 1 0000000000000000 100 0 0 10 0\n"
        );
        fs::write(root.join("proc/net/tcp"), dup).unwrap();
        assert_eq!(sockets(&root).unwrap().ports_for(300), &[0x1E93, 0xDECF]);
    }
}
