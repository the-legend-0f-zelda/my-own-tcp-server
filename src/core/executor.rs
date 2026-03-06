use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::thread;
use std::thread::{JoinHandle};
use crate::core::runtime::Task;

pub struct Worker {
    handle: JoinHandle<()>,
}
impl Worker {
    pub fn spawn(task_queue:Arc<TaskQueue>) -> Self {
        let handle = thread::spawn(move || {
            loop {
                let task = task_queue.pop();
                match catch_unwind(AssertUnwindSafe(task)) {
                    Ok(Ok(_)) => {},
                    Ok(Err(_e)) => {
                        /* TODO LOG ERROR */
                        continue;
                    }
                    Err(_panic) => {
                        /* TODO LOG PANIC */
                        continue;
                    }
                }
            }
        });

        Self { handle }
    }
}


pub struct TaskQueue {
    queue: Mutex<VecDeque<Task>>,
    status: Condvar,
}
impl TaskQueue {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            status: Condvar::new(),
        }
    }

    pub fn push(&self, task: Task) {
        self.queue.lock().unwrap().push_back(task);
        self.status.notify_one();
    }

    pub fn pop(&self) -> Task {
        let mut queue = self.queue.lock().unwrap();
        while queue.is_empty() {
            queue = self.status.wait(queue).unwrap();
        }
        queue.pop_front().unwrap()
    }
}


pub struct ThreadPool {
    pub queue: Arc<TaskQueue>,
    workers: Vec<Worker>,
}
impl ThreadPool {
    pub fn new(size:u16) -> Self {
        let queue = Arc::new(TaskQueue::new());
        let mut workers:Vec<Worker> = Vec::with_capacity(size as usize);

        for _ in 0..size {
            workers.push(
                Worker::spawn(Arc::clone(&queue))
            );
        }

        Self { queue, workers }
    }
}