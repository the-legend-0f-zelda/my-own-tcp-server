use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex};
use std::{io, thread};
use std::ops::Deref;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Waker};
use std::thread::JoinHandle;
use crossbeam_queue::ArrayQueue;
use mio::{Events, Interest, Token};
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
        match self.stream.read(&mut chunk) {
            Ok(n) => {
                self.read_buf.extend_from_slice(&chunk[..n]);
                Poll::Ready(Ok(n))
            },
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                self.event_manager
                    .delegate( self.token.clone(), cx.waker().clone() );
                Poll::Pending
            },
            Err(e) => Poll::Ready(Err(e)),
        }
    }
    pub fn load_buf(&mut self) -> impl Future<Output = io::Result<usize>> + '_ {
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

    /*pub fn read_to_end(&mut self, buf: &mut Vec<u8>) -> std::io::Result<usize> {
    }
    pub async fn read_line(&mut self, buf: &mut String) -> io::Result<usize> {
    }*/
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
                let mut waker_vtable = manager.waker_vtable.lock().unwrap();
                poll.poll(&mut event_queue, None).unwrap(); // block

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
        self.queue.pop()
    }
}


struct Worker {
    task_queue: Arc<TaskQueue>,
}
impl Worker {
    fn spawn() -> Self {
        let task_queue = Arc::new(TaskQueue::new());

        let queue = Arc::clone(&task_queue);
        // !!! 스레드 spawn에서 async-await 사용 불가, 수동으로 poll 필요
        thread::spawn(async move || {
            loop {
                let task = queue.pop().unwrap();
                print!("found task.");
                task.await;
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
}
pub(crate) type AsyncTask = Pin<Box<dyn Future<Output=()> + Send>>;
impl<P: AsyncProtocol> Server<P> {

    pub fn new() -> Self {
        Self {
            port_mappings: HashMap::new(),
            event_manager: Arc::new(EventManager::new()),
            thread_pool: ThreadPool::new(),
            max_threads: 1,
        }
    }

    pub fn set_port(&mut self, port: u16, protocol: P) {
        self.port_mappings.insert(port, Arc::new(protocol));
    }

    pub fn listen_port(&self, port: u16) {
        let socket = SocketAddr::new( IpAddr::V4(Ipv4Addr::new(0,0,0,0)), port );
        let listener:TcpListener = TcpListener::bind(socket).unwrap();

        loop {
            let (mut stream, peer) = match listener.accept() {
                Ok((stream, peer)) => (stream, peer),
                Err(_e) => continue,
            };

            let protocol = match self.port_mappings.get(&port) {
                Some(p) => Arc::clone(p),
                None => continue,
            };

            let manager = Arc::clone(&self.event_manager);

            {
                let poll = manager.poll.lock().unwrap();
                poll.registry().register(
                    &mut stream,
                    Token(1),
                    Interest::READABLE,
                ).unwrap();
            }

            let task:AsyncTask = Box::pin(async move {
                let async_stream = AsyncTcpStream::new(stream, Token(1), manager);
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
        let event_loop = event_manager.run();
        join_handles.push(event_loop);

        for port in &server.port_mappings {
            let server_clone = Arc::clone(&server);
            let port_clone = port.0.clone();

            let port_handle = thread::spawn(move || {
                server_clone.listen_port(port_clone);
            });

            join_handles.push(port_handle);
        }

        for handle in join_handles {
            handle.join().unwrap();
        }

        println!("tcp server terminated.");
    }
}