use std::error::Error;
use std::sync::{Arc, OnceLock, RwLock, RwLockWriteGuard};
use crate::applications::model::Protocol;
use crate::applications::web::protocol::Http;
use crate::applications::web::http::{HttpRequest, HttpResponse, Method};
use crate::core::runtime::{Port, Server};


static HTTP_SERVER:OnceLock<RwLock<Server>> = OnceLock::new();
static HTTP_MVC:OnceLock<Arc<RwLock<Http>>> = OnceLock::new();

fn get_server() -> RwLockWriteGuard<'static, Server> {
    HTTP_SERVER.get_or_init(|| {
        RwLock::new(Server::new(Vec::new()))
    })
        .write()
        .unwrap()
}

fn get_protocol() -> RwLockWriteGuard<'static, Http> {
    HTTP_MVC.get_or_init(|| {
            Arc::new(RwLock::new(Http::new()))
        })
        .write()
        .unwrap()
}

pub fn set_max_threads(max_threads: u16) {
    get_server().set_max_threads(max_threads);
}

pub fn bind_port(port_num:u16) {
    let mut server = get_server();
    let mvc: Arc<RwLock<dyn Protocol>> = HTTP_MVC.get().unwrap().clone();
    server.bind_port(Port::new(port_num, mvc));
}

pub fn set_tls_config(tls_config: rustls::ServerConfig) {
    get_server().set_tls_config(tls_config);
}

pub fn handle_request(method:Method, path:&str,
                      action: fn(HttpRequest, HttpResponse) -> Result<(), Box<dyn Error>>)
{
    get_protocol().handle(method, path, action);
}

pub fn clone() -> Server { get_server().clone() }

pub fn start() {
    get_server()
        .clone()
        .start();
}