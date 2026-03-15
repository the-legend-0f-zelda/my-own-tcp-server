use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex};
use std::{io, thread};
use std::ops::Deref;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::JoinHandle;
use crossbeam_queue::ArrayQueue;
use mio::{Events, Interest, Registry, Token};
use mio::net::{TcpListener, TcpStream};


pub trait AsyncProtocol: Send + Sync + 'static {
    fn handle_async_connection(&self, stream: AsyncTcpStream) -> impl Future<Output = ()> + Send;
}


pub struct AsyncTcpStream {
    stream: TcpStream,
    token: Token,
    read_buf: Vec<u8>,
    event_manager: Arc<EventManager>,
}
impl AsyncTcpStream {
    pub fn new(stream: TcpStream, token: Token, event_manager:Arc<EventManager>) -> Self {
        Self { stream, token, read_buf: Vec::new(),  event_manager}
    }

    pub fn peer_addr(&self) -> SocketAddr {
        self.stream.peer_addr().unwrap()
    }

    pub fn poll_load_buf(&mut self, cx:&mut Context) -> Poll<io::Result<usize>> {
        let mut chunk = [0u8; 4096];
        println!("start poll load buf");
        match self.stream.read(&mut chunk) {
            Ok(n) => {
                self.read_buf.extend_from_slice(&chunk[..n]);
                println!("load buf ok.");
                Poll::Ready(Ok(n))
            },
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                self.event_manager
                    .delegate( self.token.clone(), cx.waker().clone() );
                println!("load buf error: would block");
                Poll::Pending
            },
            Err(e) => {
                println!("load buf error: {}", e);
                Poll::Ready(Err(e))
            },
        }
    }
    pub fn load_buf(&mut self) -> impl Future<Output = io::Result<usize>> + '_ {
        std::future::poll_fn(move |cx| self.poll_load_buf(cx))
    }

    pub async fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        println!("start reading");
        if self.read_buf.len() >= buf.len() {
            buf.copy_from_slice(&self.read_buf[..buf.len()]);
            self.read_buf.drain(..buf.len());
            println!("read done.");
            return Ok(buf.len());
        }

        while self.read_buf.len() < buf.len() {
            println!("start load buf");
            if self.load_buf().await? == 0 {break;}
        }

        let available = std::cmp::min(self.read_buf.len(), buf.len());
        buf.copy_from_slice(&self.read_buf[..available]);
        self.read_buf.drain(..available);

        println!("read done.");
        Ok(available)
    }

    /*pub fn read_to_end(&mut self, buf: &mut Vec<u8>) -> std::io::Result<usize> {

    }*/
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
    pub fn drain_read_buf(&mut self, n: usize) -> Vec<u8> {
        self.read_buf.drain(..n).collect()
    }

    pub fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.stream.write(buf)
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
                poll.poll(&mut event_queue, None).unwrap(); // block
                drop(poll);

                let mut waker_vtable = manager.waker_vtable.lock().unwrap();
                // !!! 이벤트 알림와서 웨이커 깨우는동안 작업스레드 Pending 발생시 토큰:웨이커 저장 및 Pending 반환 지연
                // TODO 현재 이벤트루프 스레드에서 vtable에 락걸고 들어온 이벤트 iterate
                // => 이벤트 탐색하는동안 다른 워커스레드에서 delegate() 불가
                // => 요청수&i/o 많아지면 병목 가능성
                // => DashMap으로 변경?
                for event in event_queue.deref().iter() {
                    match waker_vtable.remove(&event.token()) {
                        Some(waker) => {waker.wake()}
                        None => {/*뭐여시벌*/}
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
            println!("empty.");
            empty = self.notifier.wait(empty).unwrap();
        }
        println!("pop");
        let task = self.queue.pop();
        *empty = self.queue.is_empty();
        task
    }
}


struct TaskWaker {
    task: Mutex<Option<AsyncTask>>,
    task_queue: Arc<TaskQueue>
}
impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        let task = self.task.lock().unwrap().take();
        if let Some(task) = task {
            self.task_queue.push(task);
        }
    }
}
impl TaskWaker {
    fn new(task: Option<AsyncTask>, task_queue: Arc<TaskQueue>) -> Self {
        let task_to_wake = Mutex::new(task);
        Self {
            task: task_to_wake,
            task_queue
        }
    }

    fn delegate(self: Arc<Self>, task: AsyncTask) {
        *self.task.lock().unwrap() = Some(task);
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

                print!("found task.");
                match task.as_mut().poll(&mut context) {
                    Poll::Pending => {
                        task_waker.delegate(task);
                        println!("pending.");
                    },
                    Poll::Ready(_) => {
                        println!("task done.");
                    }
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
    thread_pool: ThreadPool,
    max_threads: usize,
    next_token: AtomicUsize
}
pub(crate) type AsyncTask = Pin<Box<dyn Future<Output=()> + Send>>;
impl<P: AsyncProtocol> Server<P> {

    pub fn new() -> Self {
        Self {
            port_mappings: HashMap::new(),
            event_manager: Arc::new(EventManager::new()),
            thread_pool: ThreadPool::new(),
            max_threads: 1,
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
        let listener:TcpListener = TcpListener::bind(socket).unwrap();

        loop {
            let (mut stream, peer) = match listener.accept() {
                Ok((stream, peer)) => (stream, peer),
                Err(_e) => continue,
            };
            let token = self.next_token();

            event_registry.register(
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
                let async_stream = AsyncTcpStream::new(stream, token, event_manager);
                protocol.handle_async_connection(async_stream).await;
            });

            println!("round robin");
            self.thread_pool.round_robin(task);
        }

    }

    pub fn set_max_threads(&mut self, max_threads: usize) {
        self.max_threads = max_threads;
    }

    pub fn start(mut self) {
        let mut join_handles:Vec<JoinHandle<()>> = Vec::new();

        self.thread_pool.spawn_workers(self.max_threads);
        let server = Arc::new(self);
        let event_manager = Arc::clone(&server.event_manager);

        for port in &server.port_mappings {
            let server_clone = Arc::clone(&server);
            let port_clone = port.0.clone();
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