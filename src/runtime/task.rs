use crossbeam_queue::ArrayQueue;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::{io, thread};

pub(crate) type AsyncTask = Pin<Box<dyn Future<Output = io::Result<()>> + Send>>;

pub(crate) struct TaskQueue {
    queue: ArrayQueue<AsyncTask>,
    empty: Mutex<bool>,
    notifier: Condvar,
}

impl TaskQueue {
    pub(crate) fn new() -> Self {
        Self {
            queue: ArrayQueue::new(512),
            empty: Mutex::new(true),
            notifier: Condvar::new(),
        }
    }

    pub(crate) fn push(&self, task: AsyncTask) {
        let mut empty = self.empty.lock().unwrap();
        match self.queue.push(task) {
            Ok(_) => {}
            Err(_) => { /* queue is full */ }
        }
        *empty = false;
        self.notifier.notify_one();
    }

    pub(crate) fn pop(&self) -> Option<AsyncTask> {
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


pub(crate) struct Worker {
    pub(crate) task_queue: Arc<TaskQueue>,
}

impl Worker {
    pub(crate) fn spawn() -> Self {
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
