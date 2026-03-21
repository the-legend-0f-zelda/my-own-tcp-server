use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::{io, thread};
use std::fs::{File, Metadata};
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::JoinHandle;
use crossbeam_queue::ArrayQueue;
use mio::{Events, Interest, Registry, Token};
use mio::net::TcpStream;


pub trait AsyncProtocol: Send + Sync + 'static {
    fn handle_async_connection(&self, stream: AsyncTcpStream) -> impl Future<Output = io::Result<usize>> + Send;
}

pub struct AsyncFile {
    file: Option<File>,
    pub len: usize,
    read_buf: Arc<Mutex<Vec<u8>>>,
    waker: Arc<Mutex<Option<Waker>>>,
}
static FIO_POOL:OnceLock<ThreadPool> = OnceLock::new();
impl Drop for AsyncFile {
    fn drop(&mut self) {
    }
}
impl AsyncFile {
    pub fn from(file: File) -> Self {
        let len = file.metadata().unwrap().len();
        Self {
            file: Some(file),
            len: len as usize,
            read_buf: Arc::new(Mutex::new(Vec::new())),
            waker: Arc::new(Mutex::new(None)),
        }
    }

    fn poll_read(&mut self, cx:&mut Context, buf:&mut Vec<u8>) -> Poll<io::Result<usize>> {
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
    fn read<'a>(&'a mut self, buf:&'a mut Vec<u8>) -> impl Future<Output = io::Result<usize>> + 'a {
        std::future::poll_fn(move |cx| self.poll_read(cx, buf))
    }

    pub async fn read_to_end(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
        let mut file = self.file.take().unwrap();
        let inner_buf = Arc::clone(&self.read_buf);
        let waker = Arc::clone(&self.waker);

        let task: AsyncTask = Box::pin(async move {
            let mut local_buf = Vec::new();
            file.read_to_end(&mut local_buf).unwrap();
            *inner_buf.lock().unwrap() = local_buf;
            if let Some(w) = waker.lock().unwrap().take() {
                w.wake();
            }
        });
        FIO_POOL.get().unwrap().round_robin(task);

        self.read(buf).await
    }
}

pub struct AsyncTcpStream {
    stream: TcpStream,
    token: Token,
    read_buf: Vec<u8>,
    event_manager: Arc<EventManager>,
    registry: Registry
}
impl Drop for AsyncTcpStream {
    fn drop(&mut self) {
        self.registry.deregister(&mut self.stream).unwrap();
    }
}
impl AsyncTcpStream {
    fn new(stream: TcpStream, token: Token, event_manager:Arc<EventManager>, registry: Registry) -> Self {
        Self { stream, token, read_buf: Vec::new(),  event_manager, registry }
    }

    pub fn peer_addr(&self) -> SocketAddr {
        self.stream.peer_addr().unwrap()
    }

    fn poll_load_buf(&mut self, cx:&mut Context) -> Poll<io::Result<usize>> {
        let mut chunk = [0u8; 4096];

        match self.stream.read(&mut chunk) {
            Ok(n) => {
                self.read_buf.extend_from_slice(&chunk[..n]);
                Poll::Ready(Ok(n))
            },
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                self.event_manager
                    .delegate( self.token, cx.waker().clone() );
                Poll::Pending
            },
            Err(e) => Poll::Ready(Err(e)),
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
            if self.load_buf().await? == 0 {break;}
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
                return Ok(line.len())
            }else {
                if 0 == self.load_buf().await? {return Ok(0)}
            }
        }
    }

    fn poll_write(&mut self, buf: &[u8], cx:&mut Context) -> Poll<io::Result<usize>> {
        match self.stream.write(buf) {
            Ok(n) => Poll::Ready(Ok(n)),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                self.registry.reregister(
                    &mut self.stream,
                    self.token,
                    Interest::READABLE | Interest::WRITABLE,
                ).unwrap();

                self.event_manager
                    .delegate(self.token.clone(), cx.waker().clone());
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }
    fn write<'a>(&'a mut self, buf: &'a [u8]) -> impl Future<Output = io::Result<usize>> + 'a {
        std::future::poll_fn(move |cx| self.poll_write(buf, cx))
    }

    pub async fn write_all(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut written = 0;
        while written < data.len() {
            written += self.write(&data[written..]).await?
        }
        Ok(written)
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
                println!("event loop: polling...");
                poll.poll(&mut event_queue, None).unwrap(); // block
                drop(poll);
                println!("event loop: got events");

                let mut waker_vtable = manager.waker_vtable.lock().unwrap();
                // !!! 이벤트 알림와서 웨이커 깨우는동안 작업스레드 Pending 발생시 토큰:웨이커 저장 및 Pending 반환 지연
                // TODO 현재 이벤트루프 스레드에서 vtable에 락걸고 들어온 이벤트 iterate
                // => 이벤트 탐색하는동안 다른 워커스레드에서 delegate() 불가
                // => 요청수&i/o 많아지면 병목 가능성
                // => DashMap으로 변경?
                for event in event_queue.deref().iter() {
                    let has_waker = waker_vtable.contains_key(&event.token());
                    println!("event token: {:?}, waker exists: {}", event.token(), has_waker);
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

    fn get_registry_clone(&self) -> Registry {
        let poll = self.poll.lock().unwrap();
        poll.registry().try_clone().unwrap()
    }
}


struct TaskQueue {
    queue: ArrayQueue<AsyncTask>,
    empty: Mutex<bool>,
    notifier: Condvar
}
impl TaskQueue {
    fn new() -> Self {
        Self {
            queue: ArrayQueue::new(512),
            empty: Mutex::new(true),
            notifier: Condvar::new()
        }
    }

    fn push(&self, task: AsyncTask) {
        let mut empty = self.empty.lock().unwrap();
        match self.queue.push(task) {
            Ok(_) => {}
            Err(_) => {/* queue is full */}
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
    woken: AtomicBool
}
impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        if let Some(task) = self.task.lock().unwrap().take() {
            self.task_queue.push(task);
        }else {
            self.woken.store(true, Ordering::SeqCst);
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
        }else {
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
                let mut task:AsyncTask = match queue.pop() {
                    Some(task) => task,
                    None => {continue;}
                };
                let task_waker = Arc::new(TaskWaker::new(None, queue.clone()));
                let waker = Waker::from(Arc::clone(&task_waker));
                let mut context = Context::from_waker(&waker);

                if task.as_mut().poll(&mut context).is_pending() {
                    task_waker.delegate(task);
                }
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

    fn spawn_workers(&mut self, size:usize) {
        for _i in 0..size {
            self.workers.push(Worker::spawn());
        }
    }

    fn round_robin(&self, task:AsyncTask) {
        let round = self.round.fetch_add(1, Ordering::Relaxed) % self.workers.len();
        self.workers[round].task_queue.push(task);
    }
}


pub struct Server<P: AsyncProtocol> {
    port_mappings:HashMap<u16, Arc<P>>,
    event_manager: Arc<EventManager>,
    nio_pool: ThreadPool,
    max_nio_threads: usize,
    max_fio_threads: usize,
    next_token: AtomicUsize
}
pub(crate) type AsyncTask = Pin<Box<dyn Future<Output=()> + Send>>;
impl<P: AsyncProtocol> Server<P> {

    pub fn new() -> Self {
        Self {
            port_mappings: HashMap::new(),
            event_manager: Arc::new(EventManager::new()),
            nio_pool: ThreadPool::new(),
            max_nio_threads: 1,
            max_fio_threads: 1,
            next_token: AtomicUsize::new(0)
        }
    }

    pub fn set_port(&mut self, port: u16, protocol: P) {
        self.port_mappings.insert(port, Arc::new(protocol));
    }

    fn next_token(&self) -> Token {
        Token(self.next_token.fetch_add(1, Ordering::Relaxed))
    }

    pub fn listen_port(&self, port: u16, event_registry: Registry) {
        let socket = SocketAddr::new( IpAddr::V4(Ipv4Addr::new(0,0,0,0)), port );
        let listener = std::net::TcpListener::bind(socket).unwrap();
        println!("listening on port {}", port);

        loop {
            let (std_stream, peer) = match listener.accept() {
                Ok((stream, peer)) => (stream, peer),
                Err(e) => {
                    println!("accept error {}", e);
                    continue
                },
            };
            let mut stream = TcpStream::from_std(std_stream);
            println!("accepted connection");
            let token = self.next_token();
            let registry = event_registry.try_clone().unwrap();

            registry.register(
                &mut stream,
                token,
                Interest::READABLE,
            ).unwrap();

            let protocol = match self.port_mappings.get(&port) {
                Some(p) => Arc::clone(p),
                None => continue,
            };

            let event_manager = Arc::clone(&self.event_manager);

            let task:AsyncTask = Box::pin(async move {
                let async_stream = AsyncTcpStream::new(
                    stream, token, event_manager, registry
                );
                protocol.handle_async_connection(async_stream).await.unwrap();
            });

            self.nio_pool.round_robin(task);
        }

    }

    pub fn set_max_nio_threads(&mut self, max_nio_threads: usize) {
        self.max_nio_threads = max_nio_threads;
    }

    pub fn set_max_fio_threads(&mut self, max_fio_threads: usize) {
        self.max_fio_threads = max_fio_threads;
    }

    pub fn start(mut self) {
        let mut join_handles:Vec<JoinHandle<()>> = Vec::new();

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