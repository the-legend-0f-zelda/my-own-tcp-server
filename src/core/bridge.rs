use std::{future, sync::{Arc, Mutex}, task::{Context, Poll}};

pub fn tokio_bridge<F, T>(tokio_fut: F) -> impl Future<Output = T>
where F: Future<Output = T> + Send + 'static,
    T: Send + 'static
{
    let mut tf = Some(tokio_fut);
    let result:Arc<Mutex<Option<T>>> = Arc::new(Mutex::new(None));

    future::poll_fn(move |cx| {
        let waker = cx.waker().clone();

        if let Some(task) = tf.take() { // 최초 poll 시 => tokio 런타임에서 실행 => 실행 완료시 외부 cx.waker 깨움
            let result_clone = Arc::clone(&result);

            tokio::spawn(async move {
                let tmp_result = Some(task.await);
                let mut guard = result_clone.lock().unwrap();
                *guard = tmp_result;
                drop(guard);
                waker.wake();
            });
        }

        let mut guard = result.lock().unwrap();
        if guard.is_none() {
            drop(guard);
            Poll::Pending
        }else {
            Poll::Ready((*guard).take().unwrap())
        }
    })
}
