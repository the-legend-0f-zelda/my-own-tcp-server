use std::sync::{Mutex, MutexGuard, OnceLock};
use crate::applications::async_web::http::{Action, HttpRequest, HttpResponse, Method};
use crate::applications::async_web::protocol::Http;

static HTTP_MVC:OnceLock<Mutex<Option<Http>>> = OnceLock::new();

fn as_guard() -> MutexGuard<'static, Option<Http>> {
    HTTP_MVC.get_or_init(|| {
            Mutex::new(Some(Http::new()))
        })
        .lock().unwrap()
}

pub fn route(method:Method, path:&str, action: Action) {
    as_guard().as_mut().unwrap()
        .handle(method, path, action);
}

pub fn extract() -> Http {
    HTTP_MVC.get().unwrap()
        .lock().unwrap()
        .take().unwrap()
}


#[macro_export]
macro_rules! handler {
    // 외부 함수포인터 또는 클로저 => 핸들러
    ($f:expr) => {
        Box::new(|req, res| Box::pin($f(req, res)))
    };
    // 인라인 함수 => 핸들러
    ($req:ident, $res:ident, $body:expr) => {
        Box::new(|$req, $res| Box::pin(async move { $body }))
    };
}