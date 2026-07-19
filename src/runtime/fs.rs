use std::{
    fs::File,
    io::{self, Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering}
    },
    task::{Context, Poll, Waker}
};
use crate::runtime::{AsyncTask, FIO_POOL};


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
