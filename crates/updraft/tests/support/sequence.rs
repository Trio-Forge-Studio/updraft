use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
#[derive(Clone)]
pub struct Request {
    pub head: String,
    pub body: Vec<u8>,
}
pub enum Response {
    Body(u16, String),
    Reset,
    Truncated(u16),
}
impl Response {
    pub fn json(value: serde_json::Value) -> Self {
        Self::Body(200, value.to_string())
    }
}
pub struct Server {
    pub origin: String,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    pub fn new(replies: Vec<Response>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let response_origin = origin.clone();
        let requests = Arc::new(Mutex::new(vec![]));
        let calls = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut replies = VecDeque::from(replies);
            while !stopped.load(Ordering::SeqCst) {
                let (mut socket, _) = match listener.accept() {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::park_timeout(Duration::from_millis(2));
                        continue;
                    }
                    Err(_) => break,
                };
                socket.set_nonblocking(false).unwrap();
                socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut bytes = vec![];
                let mut buffer = [0; 8192];
                let split = loop {
                    match socket.read(&mut buffer) {
                        Ok(0) | Err(_) => break None,
                        Ok(n) => bytes.extend_from_slice(&buffer[..n]),
                    }
                    if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        break Some(at + 4);
                    }
                };
                let Some(split) = split else { continue };
                let head = String::from_utf8_lossy(&bytes[..split]).into_owned();
                let length = head
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                            .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                while bytes.len() < split + length {
                    match socket.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => bytes.extend_from_slice(&buffer[..n]),
                    }
                }
                calls.lock().unwrap().push(Request { head, body: bytes[split..].to_vec() });
                match replies.pop_front().unwrap_or(Response::Body(500, "unexpected request".into())) {
                    Response::Reset => {}
                    Response::Truncated(status) => {
                        let _ = write!(
                            socket,
                            "HTTP/1.1 {status} Reply\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{{}}"
                        );
                    }
                    Response::Body(status, body) => {
                        let body = body.replace("$ORIGIN", &response_origin);
                        let _ = write!(
                            socket,
                            "HTTP/1.1 {status} Reply\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                    }
                }
            }
        });
        Self { origin, requests, stop, thread: Some(thread) }
    }
    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}
