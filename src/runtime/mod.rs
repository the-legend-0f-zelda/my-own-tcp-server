use crossbeam_queue::ArrayQueue;
use mio::net::TcpStream;
use mio::{Events, Interest, Registry, Token};
use rustls::ServerConfig;
use std::collections::HashMap;
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

use crate::runtime::async_io::AsyncTcpStream;


pub mod bridge;
pub mod async_io;


pub trait AsyncProtocol: Send + Sync + 'static {
    fn handle_async_connection(&self, stream: AsyncTcpStream) -> AsyncConnectionFuture<'_>;
}
pub type AsyncConnectionFuture<'a> = Pin<Box<dyn Future<Output = io::Result<usize>> + Send + 'a>>;

static FIO_POOL: OnceLock<ThreadPool> = OnceLock::new();


pub struct Reactor {
    event_queue: Mutex<Events>,
    poll: Mutex<mio::Poll>,
    waker_vtable: Mutex<HashMap<Token, Waker>>,
}
impl Reactor {
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
    reactor: Arc<Reactor>,
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
            reactor: Arc::new(Reactor::new()),
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
            let (std_stream, _peer) = match listener.accept() {
                Ok((stream, _peer)) => (stream, _peer),
                Err(e) => {
                    println!("accept error {}", e);
                    continue;
                }
            };

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

            let event_manager = Arc::clone(&self.reactor);

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
        let reactor = Arc::clone(&server.reactor);

        for port in &server.port_mappings {
            let server_clone = Arc::clone(&server);
            let port_clone = *port.0;
            let registry_clone = reactor.get_registry_clone();

            let port_handle = thread::spawn(move || {
                server_clone.listen_port(port_clone, registry_clone);
            });
            join_handles.push(port_handle);
        }

        let event_loop = reactor.run();
        join_handles.push(event_loop);

        for handle in join_handles {
            handle.join().unwrap();
        }

        println!("tcp server terminated.");
    }
}
