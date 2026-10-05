//! The named pipe the service listens on.
//!
//! Access is decided by the pipe's own security descriptor, enforced by the
//! kernel before a byte is read:
//!
//! * SYSTEM and Administrators: full access.
//! * Interactive users (logged on at this machine): read/write.
//! * Everyone else — network logons, services running as other accounts — is
//!   not in the list and cannot open it. Remote clients are also rejected at
//!   the pipe level.
//!
//! Each connection carries one request, at most [`MAX_REQUEST`] bytes.

use std::ffi::c_void;

use ramwarden_nt::pipe::{PIPE_NAME, Request, Response};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::watch;
use windows::Win32::Foundation::{HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::System::Pipes::GetNamedPipeClientProcessId;
use windows::core::HSTRING;

use crate::handler::Handler;

/// SYSTEM and Administrators full access; interactive users read/write.
const SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)";

/// Far more than any request needs; bounds a misbehaving client.
const MAX_REQUEST: u64 = 16 * 1024;

/// A security descriptor built from SDDL, freed on drop.
struct Descriptor(PSECURITY_DESCRIPTOR);

// SAFETY: an immutable, self-contained allocation only read by the kernel.
unsafe impl Send for Descriptor {}
unsafe impl Sync for Descriptor {}

impl Descriptor {
    fn new() -> std::io::Result<Self> {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `sd` receives a LocalAlloc'd descriptor we free on drop.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(&HSTRING::from(SDDL), SDDL_REVISION_1, &mut sd, None)
        }
        .map_err(std::io::Error::other)?;
        Ok(Descriptor(sd))
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptor... above.
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0.0)));
        }
    }
}

fn create(sd: &Descriptor, first: bool) -> std::io::Result<NamedPipeServer> {
    let mut attrs = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0.0,
        bInheritHandle: false.into(),
    };
    // SAFETY: `attrs` and the descriptor it points at outlive the call.
    unsafe {
        ServerOptions::new()
            // Refuse to start if something else already owns the name, rather
            // than serving behind an impostor.
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(PIPE_NAME, &mut attrs as *mut _ as *mut c_void)
    }
}

/// Serve until `shutdown` flips to true.
pub async fn serve(mut shutdown: watch::Receiver<bool>) -> std::io::Result<()> {
    let sd = Descriptor::new()?;
    let mut server = create(&sd, true)?;
    tracing::info!("listening on {PIPE_NAME}");
    loop {
        tokio::select! {
            r = server.connect() => r?,
            _ = shutdown.changed() => break,
        }
        let client = server;
        server = create(&sd, false)?;
        tokio::spawn(async move {
            if let Err(e) = serve_one(client).await {
                tracing::warn!("client error: {e}");
            }
        });
    }
    tracing::info!("stopped");
    Ok(())
}

fn client_pid(pipe: &NamedPipeServer) -> Option<u32> {
    use std::os::windows::io::AsRawHandle;
    let mut pid = 0u32;
    // SAFETY: querying a connected pipe handle we own.
    unsafe { GetNamedPipeClientProcessId(HANDLE(pipe.as_raw_handle()), &mut pid) }.ok()?;
    Some(pid)
}

async fn serve_one(pipe: NamedPipeServer) -> std::io::Result<()> {
    let pid = client_pid(&pipe);
    let (read, mut write) = tokio::io::split(pipe);
    let mut line = String::new();
    BufReader::new(read.take(MAX_REQUEST)).read_line(&mut line).await?;

    let response = match serde_json::from_str::<Request>(line.trim()) {
        Ok(req) => {
            tracing::info!("pid {pid:?}: {req:?}");
            // The work is blocking system calls; keep it off the reactor.
            tokio::task::spawn_blocking(move || Handler::new(pid).handle(req))
                .await
                .unwrap_or_else(|e| Response::error(format!("handler panicked: {e}")))
        }
        Err(e) => Response::error(format!("not a request this service understands: {e}")),
    };
    let mut out = serde_json::to_string(&response).unwrap_or_else(|_| r#"{"status":"error","message":"encode"}"#.into());
    out.push('\n');
    write.write_all(out.as_bytes()).await?;
    write.flush().await
}
