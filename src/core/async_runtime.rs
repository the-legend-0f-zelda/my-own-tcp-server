use crossbeam_queue::ArrayQueue;
use io::Error;
use mio::net::TcpStream;
use mio::{Events, Interest, Registry, Token};
use rustls::{ServerConfig, ServerConnection};
use std::collections::HashMap;
use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::ops::Deref;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::JoinHandle;
use std::time::Duration;
use std::{io, thread};


pub trait AsyncProtocol: Send + Sync + 'static {
    fn handle_async_connection(&self, stream: AsyncTcpStream) -> AsyncConnectionFuture<'_>;
}
pub type AsyncConnectionFuture<'a> = Pin<Box<dyn Future<Output = io::Result<usize>> + Send + 'a>>;


static FIO_POOL: OnceLock<ThreadPool> = OnceLock::new();


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
    event_manager: Arc<EventManager>,
    registry: Registry,
    tls: Option<ServerConnection>,
}
impl Drop for AsyncTcpStream {
    fn drop(&mut self) {
        self.event_manager.undelegate(self.token);
        let _r = self.registry.deregister(&mut self.stream);
    }
}
impl AsyncTcpStream {
    fn new(
        stream: TcpStream,
        token: Token,
        event_manager: Arc<EventManager>,
        registry: Registry,
    ) -> Self {
        Self {
            stream,
            token,
            read_buf: Vec::new(),
            event_manager,
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
        self.event_manager.delegate(self.token, cx.waker().clone());

        let mut chunk = [0u8; 4096];

        let read_result = match self.tls {
            Some(ref mut _tls) => self.read_tls_chunk(&mut chunk),
            None => self.stream.read(&mut chunk),
        };

        match read_result {
            Ok(n) => {
                self.read_buf.extend_from_slice(&chunk[..n]);
                self.event_manager.undelegate(self.token);
                Poll::Ready(Ok(n))
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                Poll::Pending
            }
            Err(e) => {
                self.event_manager.undelegate(self.token);
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

                self.event_manager.delegate(self.token, cx.waker().clone());
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

pub struct EventManager {
    event_queue: Mutex<Events>,
    poll: Mutex<mio::Poll>,
    waker_vtable: Mutex<HashMap<Token, Waker>>,
}
impl EventManager {
    pub fn new() -> Self {
        Self {
            event_queue: Mutex::new(Events::with_capacity(1024)),
            poll: Mutex::new(mio::Poll::new().unwrap()),
            waker_vtable: Mutex::new(HashMap::new()),
        }
    }

    fn run(self: Arc<Self>) -> JoinHandle<()> {
        let manager = Arc::clone(&self);
        thread::spawn(move || {
            let mut event_queue = manager.event_queue.lock().unwrap();

            loop {
                let mut poll = manager.poll.lock().unwrap();
                if let Err(_e) = poll.poll(&mut event_queue, None) {
                    // block
                    continue; // todo log error
                }
                drop(poll);

                let mut waker_vtable = manager.waker_vtable.lock().unwrap();
                // !!! 이벤트 알림와서 웨이커 깨우는동안 작업스레드 Pending 발생시 토큰:웨이커 저장 및 Pending 반환 지연
                // TODO 현재 이벤트루프 스레드에서 vtable에 락걸고 들어온 이벤트 iterate
                // => 이벤트 탐색하는동안 다른 워커스레드에서 delegate() 불가
                // => 요청수&i/o 많아지면 병목 가능성
                // => DashMap으로 변경?
                for event in event_queue.deref().iter() {
                    let has_waker = waker_vtable.contains_key(&event.token());
                    if let Some(waker) = waker_vtable.remove(&event.token()) {
                        waker.wake();
                    }
                }
            }
        })
    }

    fn delegate(&self, token: Token, waker: Waker) {
        self.waker_vtable.lock().unwrap().insert(token, waker);
    }

    fn undelegate(&self, token: Token) {
        self.waker_vtable.lock().unwrap().remove(&token);
    }

    fn get_registry_clone(&self) -> Registry {
        let poll = self.poll.lock().unwrap();
        poll.registry().try_clone().unwrap()
    }
}

struct TaskQueue {
    queue: ArrayQueue<AsyncTask>,
    empty: Mutex<bool>,
    notifier: Condvar,
}
impl TaskQueue {
    fn new() -> Self {
        Self {
            queue: ArrayQueue::new(512),
            empty: Mutex::new(true),
            notifier: Condvar::new(),
        }
    }

    fn push(&self, task: AsyncTask) {
        let mut empty = self.empty.lock().unwrap();
        match self.queue.push(task) {
            Ok(_) => {}
            Err(_) => { /* queue is full */ }
        }
        *empty = false;
        self.notifier.notify_one();
    }

    fn pop(&self) -> Option<AsyncTask> {
        let mut empty = self.empty.lock().unwrap();
        while *empty {
            empty = self.notifier.wait(empty).unwrap();
        }
        let task = self.queue.pop();
        *empty = self.queue.is_empty();

        task
    }
}

struct TaskWaker {
    task: Mutex<Option<AsyncTask>>,
    task_queue: Arc<TaskQueue>,
    woken: AtomicBool,
}
impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        match self.task.lock().unwrap().take() {
            Some(task) => { self.task_queue.push(task);},
            None => { self.woken.store(true, Ordering::SeqCst); }
        }
    }
}
impl TaskWaker {
    fn new(task: Option<AsyncTask>, task_queue: Arc<TaskQueue>) -> Self {
        let task_to_wake = Mutex::new(task);
        Self {
            task: task_to_wake,
            task_queue,
            woken: AtomicBool::new(false),
        }
    }

    fn delegate(self: Arc<Self>, task: AsyncTask) {
        let mut guard = self.task.lock().unwrap();
        if self.woken.load(Ordering::SeqCst) {
            self.task_queue.push(task);
        } else {
            *guard = Some(task);
        }
    }
}

struct Worker {
    task_queue: Arc<TaskQueue>,
}
impl Worker {
    fn spawn() -> Self {
        let task_queue = Arc::new(TaskQueue::new());
        let queue = Arc::clone(&task_queue);

        thread::spawn(move || {
            loop {
                match std::panic::catch_unwind(AssertUnwindSafe(|| {
                    let mut task: AsyncTask = match queue.pop() {
                        Some(task) => task,
                        None => return
                    };
                    let task_waker = Arc::new(TaskWaker::new(None, queue.clone()));
                    let waker = Waker::from(Arc::clone(&task_waker));
                    let mut context = Context::from_waker(&waker);
                    match task.as_mut().poll(&mut context) {
                        Poll::Ready(_) => {}
                        Poll::Pending => { task_waker.delegate(task); }
                    }
                })) {
                    Ok(_) => {}
                    Err(e) => { eprintln!("[NIO] task panicked: {:?}", e); }
                };
            }
        });

        Self { task_queue }
    }
}

struct ThreadPool {
    workers: Vec<Worker>,
    round: AtomicUsize,
}
impl ThreadPool {
    fn new() -> Self {
        Self {
            workers: Vec::new(),
            round: AtomicUsize::new(0),
        }
    }

    fn spawn_workers(&mut self, size: usize) {
        for _i in 0..size {
            self.workers.push(Worker::spawn());
        }
    }

    fn round_robin(&self, task: AsyncTask) {
        let round = self.round.fetch_add(1, Ordering::Relaxed) % self.workers.len();
        self.workers[round].task_queue.push(task);
    }
}

pub struct Server {
    port_mappings: HashMap<u16, Arc<dyn AsyncProtocol>>,
    event_manager: Arc<EventManager>,
    nio_pool: ThreadPool,
    max_nio_threads: usize,
    max_fio_threads: usize,
    next_token: AtomicUsize,
    config: Option<Arc<ServerConfig>>,
    read_timeout: Option<Duration>,
}
pub(crate) type AsyncTask = Pin<Box<dyn Future<Output = io::Result<()>> + Send>>;
impl Server {
    pub fn new() -> Self {
        Self {
            port_mappings: HashMap::new(),
            event_manager: Arc::new(EventManager::new()),
            nio_pool: ThreadPool::new(),
            max_nio_threads: 1,
            max_fio_threads: 1,
            next_token: AtomicUsize::new(0),
            config: None,
            read_timeout: None,
        }
    }

    pub fn set_port(&mut self, port: u16, protocol: impl AsyncProtocol) {
        self.port_mappings.insert(port, Arc::new(protocol));
    }

    pub fn set_max_nio_threads(&mut self, max_nio_threads: usize) {
        self.max_nio_threads = max_nio_threads;
    }

    pub fn set_max_fio_threads(&mut self, max_fio_threads: usize) {
        self.max_fio_threads = max_fio_threads;
    }

    pub fn set_config(&mut self, config: Arc<ServerConfig>) {
        self.config = Some(config);
    }

    pub fn set_read_timeout(&mut self, read_timeout: Option<Duration>) {
        self.read_timeout = read_timeout;
    }

    fn next_token(&self) -> Token {
        Token(self.next_token.fetch_add(1, Ordering::Relaxed))
    }

    pub fn listen_port(&self, port: u16, event_registry: Registry) {
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), port);
        let listener = std::net::TcpListener::bind(socket).unwrap();
        println!("listening on port {}", port);

        loop {
            let (std_stream, peer) = match listener.accept() {
                Ok((stream, peer)) => (stream, peer),
                Err(e) => {
                    println!("accept error {}", e);
                    continue;
                }
            };
            //std_stream.set_read_timeout(self.read_timeout).unwrap();
            let _r = std_stream.set_nonblocking(true);
            let mut stream = TcpStream::from_std(std_stream);

            let token = self.next_token();
            let registry = event_registry.try_clone().unwrap();

            if let Err(_e) = registry.register(&mut stream, token, Interest::READABLE) {
                continue;
            }

            let protocol = match self.port_mappings.get(&port) {
                Some(p) => Arc::clone(p),
                None => continue,
            };

            let event_manager = Arc::clone(&self.event_manager);

            let task: AsyncTask = Box::pin(async move {
                let async_stream = AsyncTcpStream::new(stream, token, event_manager, registry);
                protocol.handle_async_connection(async_stream).await?;
                Ok(())
            });

            self.nio_pool.round_robin(task);
        }
    }

    pub fn start(mut self) {
        let mut join_handles: Vec<JoinHandle<()>> = Vec::new();

        self.nio_pool.spawn_workers(self.max_nio_threads);
        FIO_POOL.get_or_init(|| {
            let mut fio_pool = ThreadPool::new();
            fio_pool.spawn_workers(self.max_fio_threads);
            fio_pool
        });

        let server = Arc::new(self);
        let event_manager = Arc::clone(&server.event_manager);

        for port in &server.port_mappings {
            let server_clone = Arc::clone(&server);
            let port_clone = *port.0;
            let registry_clone = event_manager.get_registry_clone();

            let port_handle = thread::spawn(move || {
                server_clone.listen_port(port_clone, registry_clone);
            });
            join_handles.push(port_handle);
        }

        let event_loop = event_manager.run();
        join_handles.push(event_loop);

        for handle in join_handles {
            handle.join().unwrap();
        }

        println!("tcp server terminated.");
    }
}
