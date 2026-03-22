use std::io;
use std::sync::{Arc, OnceLock, RwLock, RwLockWriteGuard};
use crate::applications::async_web::http::{HttpRequest, HttpResponse, Method};
use crate::applications::async_web::protocol::Http;

static HTTP_MVC:OnceLock<Arc<RwLock<Http>>> = OnceLock::new();

fn as_ref_mut() -> RwLockWriteGuard<'static, Http> {
    HTTP_MVC.get_or_init(|| {
            Arc::new(RwLock::new(Http::new()))
        })
        .write()
        .unwrap()
}

pub fn route<Fut, F>(method:Method, path:&str, action: F)
where
    Fut: Future<Output = io::Result<usize>> + Send + 'static,
    F: Fn(HttpRequest, HttpResponse) -> Fut + Send + Sync + 'static,
{ as_ref_mut().handle(method, path, action); }

pub fn extract() -> Http {
    let clone = Arc::clone( HTTP_MVC.get().unwrap() );
    let rwlock = Arc::try_unwrap(clone).ok().unwrap();
    rwlock.into_inner().unwrap()
}