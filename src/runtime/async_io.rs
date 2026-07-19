use std::{
    fs::File, io::{self, Error, ErrorKind, Read, Write}, net::SocketAddr, sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering}
    }, task::{Context, Poll, Waker}
};
use mio::{Interest, Registry, Token, net::TcpStream};
use rustls::{ServerConfig, ServerConnection};
use crate::runtime::{AsyncTask, Reactor, FIO_POOL};


pub struct AsyncFile {
    file: Option<File>,
    pub len: usize,
    read_buf: Arc<Mutex<Vec<u8>>>,
    written: Arc<AtomicBool>,
    waker: Arc<Mutex<Option<Waker>>>,
}
impl AsyncFile {
    pub fn from(file: File) -> Self {
        let len = match file.metadata() {
            Ok(metadata) => metadata.len() as usize,
            Err(_) => 0,
        };

        Self {
            file: Some(file),
            len,
            read_buf: Arc::new(Mutex::new(Vec::new())),
            written: Arc::new(AtomicBool::new(false)),
            waker: Arc::new(Mutex::new(None)),
        }
    }

    fn poll_read(&mut self, cx: &mut Context, buf: &mut Vec<u8>) -> Poll<io::Result<usize>> {
        let mut inner_buf = self.read_buf.lock().unwrap();
        if inner_buf.len() < self.len {
            let mut waker = self.waker.lock().unwrap();
            *waker = Some(cx.waker().clone());
            Poll::Pending
        } else {
            *buf = std::mem::take(&mut inner_buf);
            Poll::Ready(Ok(buf.len()))
        }
    }
    fn read<'a>(
        &'a mut self,
        buf: &'a mut Vec<u8>,
    ) -> impl Future<Output = io::Result<usize>> + 'a {
        std::future::poll_fn(move |cx| self.poll_read(cx, buf))
    }

    pub async fn read_to_end(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
        let mut file = match self.file.take() {
            Some(f) => f,
            None => return Err(io::ErrorKind::Other.into()),
        };
        let inner_buf = Arc::clone(&self.read_buf);
        let waker = Arc::clone(&self.waker);

        let task: AsyncTask = Box::pin(async move {
            let mut local_buf = Vec::new();
            file.read_to_end(&mut local_buf)?;
            *inner_buf.lock().unwrap() = local_buf;

            if let Some(w) = waker.lock().unwrap().take() {
                w.wake();
            }
            Ok(())
        });
        FIO_POOL.get().unwrap().round_robin(task);

        self.read(buf).await
    }

    fn poll_write(&mut self, cx: &mut Context) -> Poll<io::Result<bool>> {
        if self.written.load(Ordering::SeqCst) {
            Poll::Ready(Ok(true))
        } else {
            let mut waker = self.waker.lock().unwrap();
            *waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
    fn write(&mut self) -> impl Future<Output = io::Result<bool>> + '_ {
        std::future::poll_fn(|cx: &mut Context| self.poll_write(cx))
    }

    pub async fn write_all(&mut self, buf: Vec<u8>) -> io::Result<bool> {
        let mut file = match self.file.take() {
            Some(f) => f,
            None => return Err(io::ErrorKind::Other.into()),
        };

        let written = Arc::clone(&self.written);
        let waker = Arc::clone(&self.waker);

        let task: AsyncTask = Box::pin(async move {
            file.write_all(buf.as_slice())?;

            written.store(true, Ordering::SeqCst);
            if let Some(w) = waker.lock().unwrap().take() {
                w.wake();
            }
            Ok(())
        });
        FIO_POOL.get().unwrap().round_robin(task);

        self.write().await
    }
}


pub struct AsyncTcpStream {
    stream: TcpStream,
    token: Token,
    read_buf: Vec<u8>,
    reactor: Arc<Reactor>,
    registry: Registry,
    tls: Option<ServerConnection>,
}
impl Drop for AsyncTcpStream {
    fn drop(&mut self) {
        self.reactor.undelegate(self.token);
        let _r = self.registry.deregister(&mut self.stream);
    }
}
impl AsyncTcpStream {
    pub fn new(
        stream: TcpStream,
        token: Token,
        reactor: Arc<Reactor>,
        registry: Registry,
    ) -> Self {
        Self {
            stream,
            token,
            read_buf: Vec::new(),
            reactor,
            registry,
            tls: None,
        }
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.stream.peer_addr()
    }

    fn read_tls_chunk(&mut self, chunk: &mut [u8]) -> io::Result<usize> {
        let tls = self.tls.as_mut().unwrap();

        loop {
            // rustls 버퍼에 남아있는 내용 복호화 시도
            // 이전 루프에서 tls.read_tls 결과가 Ok(n) 일때
            // 이번 루프에서 Ok(0)=EOF 인 경우, 버퍼에 남아있는 내용을 반환하지 않고 끝나는 경우를 방지하기 위해
            // 복호화 -> 평문 반환시도 순으로 실행
            tls.process_new_packets()
                .map_err(|e| Error::new(ErrorKind::InvalidData, e))?;
            // rustls 버퍼에 완성된 평문이 발생한 경우
            // 또는 이전 호출에서 한번에 반환하지 못한 평문이 남은 경우 요청 버퍼에 담아 바로 반환
            if !tls.wants_read() {
                return tls.reader().read(chunk);
            }
            // 완성된 평문이 없으면 소켓에서 추가 적재 시도
            match tls.read_tls(&mut self.stream) {
                Ok(0) => return Ok(0), // 상대측 연결 종료 (소켓 EOF)
                Ok(_) => {}, // 소켓에서 추가로 받아낸 암호문이 존재함 -> 다음 루프 실행
                Err(e) => return Err(e) // WouldBlock 포함 에러 -> 상위 호출자가 비동기 또는 에러 처리
            }
        }
    }

    fn poll_load_buf(&mut self, cx: &mut Context) -> Poll<io::Result<usize>> {
        self.reactor.delegate(self.token, cx.waker().clone());

        let mut chunk = [0u8; 4096];

        let read_result = match self.tls {
            Some(ref mut _tls) => self.read_tls_chunk(&mut chunk),
            None => self.stream.read(&mut chunk),
        };

        match read_result {
            Ok(n) => {
                self.read_buf.extend_from_slice(&chunk[..n]);
                self.reactor.undelegate(self.token);
                Poll::Ready(Ok(n))
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                Poll::Pending
            }
            Err(e) => {
                self.reactor.undelegate(self.token);
                Poll::Ready(Err(e))
            }
        }
    }
    fn load_buf(&mut self) -> impl Future<Output = io::Result<usize>> + '_ {
        std::future::poll_fn(move |cx| self.poll_load_buf(cx))
    }

    pub async fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.read_buf.len() >= buf.len() {
            buf.copy_from_slice(&self.read_buf[..buf.len()]);
            self.read_buf.drain(..buf.len());
            return Ok(buf.len());
        }

        while self.read_buf.len() < buf.len() {
            if self.load_buf().await? == 0 {
                break;
            }
        }

        let available = std::cmp::min(self.read_buf.len(), buf.len());
        buf.copy_from_slice(&self.read_buf[..available]);
        self.read_buf.drain(..available);

        Ok(available)
    }

    pub async fn read_line(&mut self, buf: &mut String) -> io::Result<usize> {
        loop {
            if let Some(lf) = self.read_buf.iter().position(|&b| b == b'\n') {
                let line = self.read_buf.drain(..=lf).collect::<Vec<u8>>();
                buf.push_str(&String::from_utf8_lossy(&line));
                return Ok(line.len());
            } else if 0 == self.load_buf().await? {
                return Ok(0);
            }
        }
    }

    fn poll_write(&mut self, buf: &[u8], cx: &mut Context) -> Poll<io::Result<usize>> {
        let write_result = match self.tls {
            Some(ref mut tls) => {
                // TLS 사용시 평문 바이트 수는 상위 호출자(write_all) 에서 미리 누적함
                // -> 여기에서 별도로 추가적인 쓰기 바이트 수 반환하면 안됨. -> Ok(0)
                let mut tls_result = Ok(0);
                while tls.wants_write() {
                    if let Err(e) = tls.write_tls(&mut self.stream) {
                        tls_result = Err(e);
                        break;
                    }
                }
                tls_result
            },
            None => self.stream.write(buf),
        };

        match write_result {
            Ok(n) => Poll::Ready(Ok(n)),
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                self.registry.reregister(
                    &mut self.stream,
                    self.token,
                    Interest::READABLE | Interest::WRITABLE,
                )?;

                self.reactor.delegate(self.token, cx.waker().clone());
                Poll::Pending
            },
            Err(e) => Poll::Ready(Err(e)),
        }
    }
    fn write<'a>(&'a mut self, buf: &'a [u8]) -> impl Future<Output = io::Result<usize>> + 'a {
        std::future::poll_fn(move |cx| self.poll_write(buf, cx))
    }

    pub async fn write_all(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut written = 0;
        while written < data.len() {
            // TLS 연결 사용시 rustls 암호화 버퍼에 미리 데이터 로드,
            // -> 평문 바이트 수 미리 누적
            // -> 따라서 self.write(&data[written..]).await 코드에서 주어진 배열 인자가 실제로 사용되지 않음
            if let Some(tls) = &mut self.tls {
                written += tls.writer().write(&data[written..])?;
            }

            written += self.write(&data[written..]).await?
        }
        Ok(written)
    }

    pub async fn start_tls(&mut self, config: Arc<ServerConfig>) -> io::Result<()> {
        let mut conn = match ServerConnection::new(config) {
            Ok(c) => c,
            Err(e) => return Err(Error::other(e)),
        };

        while conn.is_handshaking() {
            if conn.wants_write() {
                let mut buf = Vec::new();
                conn.write_tls(&mut buf).map_err(Error::other)?;
                self.write_all(&buf).await?;
            }
            if conn.wants_read() {
                if self.load_buf().await? == 0 {
                    return Err(Error::new(
                        ErrorKind::UnexpectedEof,
                        "peer closed during TLS handshake",
                    ));
                }
                conn.read_tls(&mut &self.read_buf[..])?;
                self.read_buf.clear();
                conn.process_new_packets()
                    .map_err(|e| Error::new(ErrorKind::InvalidData, e))?;
            }
        }

        self.tls = Some(conn);
        Ok(())
    }
}
