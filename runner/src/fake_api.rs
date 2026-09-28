use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::retry::Backoff;
use crate::sender::EventSender;

pub const RUN_ID: &str = "01999999-9999-7999-9999-999999999999";

pub const FAST: Backoff = Backoff {
    first_delay: Duration::from_millis(10),
    max_delay: Duration::from_millis(40),
    budget: Duration::from_secs(3),
};

pub fn sender(api_url: &str) -> EventSender {
    EventSender::for_tests(api_url, FAST, Duration::from_secs(2))
}

pub fn dead_api_url() -> String {
    let port = closed_port();
    format!("http://127.0.0.1:{port}")
}

fn closed_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("reserving a local port")
        .port()
}

pub struct Reply {
    status: u16,
    body: String,
    delay: Duration,
}

impl Reply {
    pub fn status(status: u16) -> Self {
        Self {
            status,
            body: String::new(),
            delay: Duration::ZERO,
        }
    }

    pub fn json(status: u16, body: &serde_json::Value) -> Self {
        Self {
            body: body.to_string(),
            ..Self::status(status)
        }
    }

    pub fn after(self, delay: Duration) -> Self {
        Self { delay, ..self }
    }
}

#[derive(Debug, Clone)]
pub struct Received {
    pub method: String,
    pub path: String,
    pub body: String,
}

type Script = Arc<dyn Fn(usize) -> Reply + Send + Sync>;

pub struct FakeApi {
    pub url: String,
    received: Arc<Mutex<Vec<Received>>>,
}

impl FakeApi {
    pub async fn start(script: impl Fn(usize) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("binding the fake api");
        let port = listener.local_addr().expect("fake api address").port();
        let api = Self::at(port);
        tokio::spawn(serve(listener, Arc::new(script), api.received.clone()));
        api
    }

    pub fn start_later(
        after: Duration,
        script: impl Fn(usize) -> Reply + Send + Sync + 'static,
    ) -> Self {
        let port = closed_port();
        let api = Self::at(port);
        let received = api.received.clone();
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            let listener = TcpListener::bind(("127.0.0.1", port))
                .await
                .expect("binding the fake api on its reserved port");
            serve(listener, Arc::new(script), received).await;
        });
        api
    }

    fn at(port: u16) -> Self {
        Self {
            url: format!("http://127.0.0.1:{port}"),
            received: Arc::default(),
        }
    }

    pub fn received(&self) -> Vec<Received> {
        self.received.lock().expect("received lock").clone()
    }
}

async fn serve(listener: TcpListener, script: Script, received: Arc<Mutex<Vec<Received>>>) {
    while let Ok((socket, _)) = listener.accept().await {
        tokio::spawn(answer(socket, script.clone(), received.clone()));
    }
}

async fn answer(
    mut socket: TcpStream,
    script: Script,
    received: Arc<Mutex<Vec<Received>>>,
) -> std::io::Result<()> {
    let request = read_request(&mut socket).await?;
    let index = {
        let mut received = received.lock().expect("received lock");
        received.push(request);
        received.len() - 1
    };
    let reply = script(index);
    tokio::time::sleep(reply.delay).await;
    let head = format!(
        "HTTP/1.1 {} Scripted\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        reply.status,
        reply.body.len()
    );
    socket.write_all(head.as_bytes()).await?;
    socket.write_all(reply.body.as_bytes()).await?;
    socket.shutdown().await
}

async fn read_request(socket: &mut TcpStream) -> std::io::Result<Received> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
        let read = socket.read(&mut chunk).await?;
        if read == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let length = head
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while buf.len() < head_end + length {
        let read = socket.read(&mut chunk).await?;
        if read == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&chunk[..read]);
    }
    let mut request_line = head.split_whitespace();
    Ok(Received {
        method: request_line.next().unwrap_or_default().to_owned(),
        path: request_line.next().unwrap_or_default().to_owned(),
        body: String::from_utf8_lossy(&buf[head_end..head_end + length]).into_owned(),
    })
}
