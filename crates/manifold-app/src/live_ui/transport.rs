use serde_json::Value;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const MAX_REQUEST: usize = 64 * 1024;
const MAX_RESPONSE: usize = 2 * 1024 * 1024;
const WORK: usize = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct Transport {
    listener: UnixListener,
    client: Option<UnixStream>,
    request: Vec<u8>,
    response: Vec<u8>,
    response_pos: usize,
    awaiting_reply: bool,
    last_activity: Instant,
    path: PathBuf,
    identity: (u64, u64),
    generation: u64,
}

impl Transport {
    pub(super) fn bind(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "socket path must be absolute",
            ));
        }
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket has no parent"))?;
        let pm = fs::symlink_metadata(parent)?;
        if !pm.file_type().is_dir()
            || pm.permissions().mode() & 0o777 != 0o700
            || pm.uid() != unsafe { libc::geteuid() } as u32
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "socket parent must be a private directory owned by the current user",
            ));
        }
        if path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "socket path already exists",
            ));
        }
        let listener = UnixListener::bind(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        let m = fs::symlink_metadata(path)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            client: None,
            request: Vec::new(),
            response: Vec::new(),
            response_pos: 0,
            awaiting_reply: false,
            last_activity: Instant::now(),
            path: path.to_owned(),
            identity: (m.dev(), m.ino()),
            generation: 0,
        })
    }

    pub(super) fn poll(&mut self) -> Option<Value> {
        if self.client.is_some() && self.last_activity.elapsed() >= TIMEOUT {
            self.drop_client();
        }
        if self.client.is_none() {
            match self.listener.accept() {
                Ok((stream, _)) => match stream.set_nonblocking(true) {
                    Ok(()) => {
                        self.client = Some(stream);
                        self.generation = self.generation.wrapping_add(1);
                        self.request.clear();
                        self.response.clear();
                        self.response_pos = 0;
                        self.awaiting_reply = false;
                        self.last_activity = Instant::now();
                    }
                    Err(e) => eprintln!("live UI: failed to configure client socket: {e}"),
                },
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => eprintln!("live UI: accept failed: {e}"),
            }
        }
        self.client.as_ref()?;
        if !self.response.is_empty() {
            self.flush();
            return None;
        }
        if self.awaiting_reply {
            let mut probe = [0u8; 1];
            let client = self.client.as_mut()?;
            match client.read(&mut probe) {
                Ok(0) => self.drop_client(),
                Ok(_) => {
                    eprintln!("live UI: client sent data before reply");
                    self.drop_client();
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => {
                    eprintln!("live UI: client probe failed: {e}");
                    self.drop_client();
                }
            }
            self.flush();
            return None;
        }
        let mut buf = [0u8; WORK];
        let mut read = 0;
        let client = self.client.as_mut()?;
        loop {
            if read == WORK {
                break;
            }
            match client.read(&mut buf[read..]) {
                Ok(0) => {
                    self.drop_client();
                    return None;
                }
                Ok(n) => {
                    read += n;
                    self.last_activity = Instant::now();
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => {
                    eprintln!("live UI: read failed: {e}");
                    self.drop_client();
                    return None;
                }
            }
        }
        self.request.extend_from_slice(&buf[..read]);
        if self.request.len() > MAX_REQUEST {
            self.set_error("request too large");
            self.flush();
            return None;
        }
        if let Some(pos) = self.request.iter().position(|&b| b == b'\n') {
            let line = self.request[..pos].to_vec();
            self.request.clear();
            match serde_json::from_slice::<Value>(&line) {
                Ok(v) => {
                    self.awaiting_reply = true;
                    return Some(v);
                }
                Err(_) => self.set_error("malformed JSON"),
            }
            self.flush();
        }
        None
    }

    pub(super) fn reply(&mut self, reply: Value) {
        if !self.awaiting_reply {
            return;
        }
        let mut bytes = match serde_json::to_vec(&reply) {
            Ok(v) => v,
            Err(_) => b"{\"ok\":false,\"error\":\"serialization failed\"}".to_vec(),
        };
        bytes.push(b'\n');
        if bytes.len() > MAX_RESPONSE {
            self.set_error("response too large");
        } else {
            self.response = bytes;
            self.response_pos = 0;
        }
        self.awaiting_reply = false;
        self.last_activity = Instant::now();
    }

    pub(super) fn connected(&self) -> bool {
        self.client.is_some()
    }

    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    fn set_error(&mut self, msg: &str) {
        self.response = format!(
            "{{\"ok\":false,\"error\":{}}}\n",
            serde_json::to_string(msg).unwrap()
        )
        .into_bytes();
        self.response_pos = 0;
        self.awaiting_reply = false;
    }
    fn flush(&mut self) {
        let mut failed = false;
        let mut budget = WORK;
        if let Some(stream) = self.client.as_mut() {
            while self.response_pos < self.response.len() && budget > 0 {
                let end = (self.response_pos + budget).min(self.response.len());
                match stream.write(&self.response[self.response_pos..end]) {
                    Ok(0) => {
                        failed = true;
                        break;
                    }
                    Ok(n) => {
                        self.response_pos += n;
                        budget -= n;
                        self.last_activity = Instant::now();
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
                    Err(e) => {
                        eprintln!("live UI: write failed: {e}");
                        failed = true;
                        break;
                    }
                }
            }
        }
        if failed || !self.response.is_empty() && self.response_pos == self.response.len() {
            self.drop_client();
        }
    }
    fn drop_client(&mut self) {
        self.client = None;
        self.request.clear();
        self.response.clear();
        self.response_pos = 0;
        self.awaiting_reply = false;
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        if let Ok(m) = fs::symlink_metadata(&self.path)
            && (m.dev(), m.ino()) == self.identity
            && let Err(error) = fs::remove_file(&self.path)
        {
            log::warn!("live UI socket cleanup failed: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = PathBuf::from(format!(
                "/private/tmp/manifold-ui-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
        fn socket(&self) -> PathBuf {
            self.0.join("ui.sock")
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn receive(client: &mut UnixStream) -> Vec<u8> {
        client.set_nonblocking(true).unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            match client.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => bytes.extend_from_slice(&buffer[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => panic!("{e}"),
            }
        }
        bytes
    }

    #[test]
    fn private_bind_and_existing_endpoint_are_enforced() {
        let dir = Directory::new();
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Transport::bind(&dir.socket()).is_err());
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o700)).unwrap();
        let transport = Transport::bind(&dir.socket()).unwrap();
        assert_eq!(fs::metadata(dir.socket()).unwrap().mode() & 0o777, 0o600);
        assert!(Transport::bind(&dir.socket()).is_err());
        assert!(dir.socket().exists());
        drop(transport);
        assert!(!dir.socket().exists());
    }

    #[test]
    fn partial_request_reply_and_disconnect_recovery() {
        let dir = Directory::new();
        let mut server = Transport::bind(&dir.socket()).unwrap();
        let mut client = UnixStream::connect(dir.socket()).unwrap();
        client.write_all(b"{\"op\":").unwrap();
        assert!(server.poll().is_none());
        client.write_all(b"\"observe\"}\n").unwrap();
        assert_eq!(server.poll().unwrap()["op"], "observe");
        server.reply(serde_json::json!({"ok":true}));
        server.poll();
        assert_eq!(
            serde_json::from_slice::<Value>(&receive(&mut client)).unwrap()["ok"],
            true
        );
        let generation = server.generation();
        let client = UnixStream::connect(dir.socket()).unwrap();
        server.poll();
        assert!(server.generation() > generation);
        drop(client);
        server.poll();
        assert!(!server.connected());
    }

    #[test]
    fn malformed_and_oversized_requests_fail() {
        let dir = Directory::new();
        let mut server = Transport::bind(&dir.socket()).unwrap();
        let mut client = UnixStream::connect(dir.socket()).unwrap();
        client.write_all(b"invalid\n").unwrap();
        server.poll();
        assert_eq!(
            serde_json::from_slice::<Value>(&receive(&mut client)).unwrap()["ok"],
            false
        );
        let mut client = UnixStream::connect(dir.socket()).unwrap();
        for chunk in vec![b'x'; MAX_REQUEST + 1].chunks(4096) {
            client.write_all(chunk).unwrap();
            server.poll();
        }
        assert_eq!(
            serde_json::from_slice::<Value>(&receive(&mut client)).unwrap()["error"],
            "request too large"
        );
    }

    #[test]
    fn timeout_and_generation_prevent_reply_reuse() {
        let dir = Directory::new();
        let mut server = Transport::bind(&dir.socket()).unwrap();
        let mut client = UnixStream::connect(dir.socket()).unwrap();
        client.write_all(b"{}\n").unwrap();
        assert!(server.poll().is_some());
        let old = server.generation();
        server.last_activity = Instant::now() - TIMEOUT;
        server.poll();
        assert!(!server.connected());
        server.reply(serde_json::json!({"old":true}));
        let mut next = UnixStream::connect(dir.socket()).unwrap();
        server.poll();
        assert_ne!(old, server.generation());
        assert!(receive(&mut next).is_empty());
    }

    #[test]
    fn reply_size_and_per_poll_write_budget_are_bounded() {
        let dir = Directory::new();
        let mut server = Transport::bind(&dir.socket()).unwrap();
        let mut client = UnixStream::connect(dir.socket()).unwrap();
        client.write_all(b"{}\n").unwrap();
        server.poll();
        server.reply(serde_json::json!({"text":"x".repeat(WORK * 2)}));
        server.poll();
        assert!(server.response_pos <= WORK);
        assert!(server.connected());
        for _ in 0..512 {
            receive(&mut client);
            server.poll();
            if !server.connected() {
                break;
            }
        }
        assert!(!server.connected(), "bounded response must fully drain");
        let mut client = UnixStream::connect(dir.socket()).unwrap();
        client.write_all(b"{}\n").unwrap();
        server.poll();
        server.reply(serde_json::json!({"text":"x".repeat(MAX_RESPONSE)}));
        server.poll();
        assert_eq!(
            serde_json::from_slice::<Value>(&receive(&mut client)).unwrap()["error"],
            "response too large"
        );
    }

    #[test]
    fn cleanup_preserves_replaced_endpoint() {
        let dir = Directory::new();
        let server = Transport::bind(&dir.socket()).unwrap();
        fs::remove_file(dir.socket()).unwrap();
        fs::write(dir.socket(), b"replacement").unwrap();
        drop(server);
        assert_eq!(fs::read(dir.socket()).unwrap(), b"replacement");
    }
}
