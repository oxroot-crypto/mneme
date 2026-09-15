//! 测试专用:最小的一次性 HTTP/1.1 服务端,模拟 OpenAI 协议端点。
//!
//! 只在 `cargo test --example memory` 时参与编译(经 `#[cfg(test)]` 挂载),
//! 正常 `cargo run --example` 的主二进制不带它。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// 轮询 `accept` 的间隔(毫秒);取自旋到非阻塞循环,避免测试尾部长挂。
const ACCEPT_POLL_MS: u64 = 2;

/// 可编排响应的本地 mock 服务。
///
/// 每次收到请求:读完整请求体 → 调用 `handler` 得到 `(status, body)` → 应答。
/// 收到的请求体会按顺序留存,供断言检查(如批量输入个数)。
pub struct MockServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<Vec<String>>>,
}

impl MockServer {
    /// 启动 mock 服务;`handler` 按请求体返回 `(HTTP 状态码, 响应体)`。
    ///
    /// # Arguments
    /// * `handler` - 纯函数式响应编排;在服务线程内被调用,需 `Send + 'static`。
    ///
    /// # Returns
    /// 已绑定 127.0.0.1 随机端口的服务句柄。
    ///
    /// # Panics
    /// 端口绑定失败时 panic(测试基础设施错误,不掩盖)。
    pub fn spawn<F>(handler: F) -> Self
    where
        F: Fn(&str) -> (u16, String) + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock: 绑定回环端口失败");
        listener
            .set_nonblocking(true)
            .expect("mock: 设置非阻塞失败");
        let addr = listener.local_addr().expect("mock: 读取本地地址失败");
        let stop = Arc::new(AtomicBool::new(false));
        let stop_in_thread = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            let mut seen = Vec::new();
            while !stop_in_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let body = read_request(&mut stream).unwrap_or_default();
                        let (status, response) = handler(&body);
                        let _ = write_response(&mut stream, status, &response);
                        seen.push(body);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(ACCEPT_POLL_MS));
                    }
                    Err(_) => break,
                }
            }
            seen
        });
        Self {
            addr,
            stop,
            handle: Some(handle),
        }
    }

    /// 返回可直接喂给 [`crate::embedding::EmbeddingConfig`] 的 base URL。
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// 停止服务线程,返回按序留存的请求体列表。
    pub fn requests(mut self) -> Vec<String> {
        self.shutdown()
    }

    /// 停服并回收线程;重复调用安全(Drop 时兜底)。
    fn shutdown(&mut self) -> Vec<String> {
        self.stop.store(true, Ordering::Relaxed);
        self.handle
            .take()
            .map_or_else(Vec::new, |handle| handle.join().unwrap_or_default())
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

/// 读取一次 HTTP/1.1 请求,返回请求体(唤醒连接直接 EOF 时返回空串)。
fn read_request(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(String::new());
    }
    let mut content_length = 0_usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header == "\r\n" || header == "\n" {
            break;
        }
        if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0_u8; content_length];
    reader.read_exact(&mut body)?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// 写回单次响应并关闭连接(`Connection: close`,避免连接复用干扰一次性服务)。
fn write_response(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Unknown",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}
