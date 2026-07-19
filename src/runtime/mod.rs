use mio::{Events, Registry, Token};
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::Waker;
use std::thread::JoinHandle;
use std::thread;
use crate::runtime::net::{AsyncConnectionFuture, AsyncTcpStream};
use crate::runtime::task::{AsyncTask, Worker};

pub mod fs;
pub mod bridge;
pub mod net;
pub mod task;

pub(crate) static FIO_POOL: OnceLock<ThreadPool> = OnceLock::new();

pub trait AsyncProtocol: Send + Sync + 'static {
    fn handle_async_connection(&self, stream: AsyncTcpStream) -> AsyncConnectionFuture<'_>;
}

pub(crate) struct ThreadPool {
    workers: Vec<Worker>,
    round: AtomicUsize,
}

impl ThreadPool {
    pub(crate) fn new() -> Self {
        Self {
            workers: Vec::new(),
            round: AtomicUsize::new(0),
        }
    }

    pub(crate) fn spawn_workers(&mut self, size: usize) {
        for _i in 0..size {
            self.workers.push(Worker::spawn());
        }
    }

    pub(crate) fn round_robin(&self, task: AsyncTask) {
        let round = self.round.fetch_add(1, Ordering::Relaxed) % self.workers.len();
        self.workers[round].task_queue.push(task);
    }
}

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

    pub(crate) fn run(self: Arc<Self>) -> JoinHandle<()> {
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

    pub(crate) fn get_registry_clone(&self) -> Registry {
        let poll = self.poll.lock().unwrap();
        poll.registry().try_clone().unwrap()
    }
}
