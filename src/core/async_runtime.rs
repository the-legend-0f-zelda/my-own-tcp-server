use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use crossbeam_queue::ArrayQueue;
use mio::Events;
use mio::net::{TcpListener, TcpStream};


pub trait AsyncProtocol: Send + Sync + 'static {
    fn handle_async_connection(&self, stream: TcpStream, peer: SocketAddr) -> impl Future<Output = ()> + Send;
}


pub struct EventManager {
    event_queue: Events,
    poll: mio::Poll,
}
impl EventManager {
    pub fn new() -> Self {
        Self {
            event_queue: Events::with_capacity(1024),
            poll: mio::Poll::new().unwrap(),
        }
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
            Err(_) => {}
        }
        *empty = false;
    }

    fn pop(&self) -> Option<AsyncTask> {
        let mut empty =self.empty.lock().unwrap();
        while *empty {
            empty = self.notifier.wait(empty).unwrap();
        }
        self.queue.pop()
    }
}


struct Worker {
    task_queue: Arc<TaskQueue>,
}
impl Worker {
    fn spawn(mut self) {
        self.task_queue = Arc::new(TaskQueue::new());
        let queue = Arc::clone(&self.task_queue);
        thread::spawn(async move || {
            loop {
                let task = queue.pop().unwrap();
                task.await;
            }
        });
    }
}


struct ThreadPool {
    workers: Vec<Worker>,
    round: usize,
}
impl ThreadPool {
    fn new() -> Self {
        Self {
            workers: Vec::new(),
            round: 0,
        }
    }

    fn round_robin(&mut self, task:AsyncTask) {
        self.round = (self.round + 1) % self.workers.len();
        self.workers
            .get_mut(self.round)
            .unwrap()
            .task_queue
            .push(task);
    }
}


pub struct Server<P: AsyncProtocol> {
    port_mappings:HashMap<u16, Arc<P>>,
    event_manager: EventManager,
    thread_pool: ThreadPool,
}
pub(crate) type AsyncTask = Pin<Box<dyn Future<Output=()> + Send>>;
impl<P: AsyncProtocol> Server<P> {

    pub fn new() -> Self {
        Self {
            port_mappings: HashMap::new(),
            event_manager: EventManager::new(),
            thread_pool: ThreadPool::new(),
        }
    }

    pub fn listen_port(&mut self, port: u16) {
        let socket = SocketAddr::new( IpAddr::V4(Ipv4Addr::new(0,0,0,0)), port );
        let listener:TcpListener = TcpListener::bind(socket).unwrap();

        loop {
            let (stream, peer) = match listener.accept() {
                Ok((stream, peer)) => (stream, peer),
                Err(_e) => continue,
            };

            let protocol = match self.port_mappings.get(&port) {
                Some(p) => Arc::clone(p),
                None => continue,
            };

            let task:AsyncTask = Box::pin(async move {
                protocol.handle_async_connection(stream, peer).await;
            });

            self.thread_pool.round_robin(task);
        }

    }
}