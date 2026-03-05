use std::error::Error;
use std::net::TcpListener;
use std::sync::{Arc, PoisonError, RwLock};
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;
use crate::applications::model::Protocol;
use crate::core::executor::{ThreadPool};


pub struct Server {
    pub ports: Vec<Port>,
    thread_pool: ThreadPool,
    pub tls_config: Option<Arc<rustls::ServerConfig>>,
}


#[derive(Clone)]
pub struct Port {
    port_num: u16,
    protocol: Arc<RwLock<dyn Protocol>>,
}

pub type Task = Box<dyn FnOnce() -> Result<(), Box<dyn Error>> + Send + Sync + 'static>;

impl Port {
    pub fn new(port_num: u16, protocol: Arc<RwLock<dyn Protocol>>) -> Self {
        Self { port_num, protocol }
    }
}

impl Server {

    pub fn new(ports:Vec<Port>, max_threads: u16) -> Server {
        Server {
            ports,
            thread_pool: ThreadPool::new(max_threads),
            tls_config: None,
        }
    }

    pub fn start(self) {
        let mut join_handles:Vec<JoinHandle<()>> = Vec::new();
        let server = Arc::new(self);

        for port in &server.ports {
            let server_clone = server.clone();
            let port_clone = port.clone();

            port.protocol
                .write().unwrap()
                .set_config(&server.tls_config);

            let port_handle = thread::spawn(move || {
                server_clone.listen_port(port_clone);
            });
            join_handles.push(port_handle);
        }

        for handle in join_handles {
            handle.join().unwrap();
        }
    }

    fn listen_port(&self, port:Port) {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", port.port_num)).unwrap();
        let protocol:Arc<RwLock<dyn Protocol>> = port.protocol.clone();

        println!("Server listening on port {}", port.port_num);

        for stream_result in listener.incoming() {
            let stream = match stream_result {
                Ok(stream) => stream,
                Err(e) => {
                    println!("Error handling connection: {:?}", e);
                    // TODO LOG ERROR
                    continue
                }
            };

            match stream.set_read_timeout(Some(Duration::from_secs(3))) {
                Ok(_) => {},
                Err(e) => {
                    println!("Error handling connection: {:?}", e);
                    // TODO LOG ERROR
                    continue
                }
            }

            let peer = match stream.peer_addr() {
                Ok(peer) => peer,
                Err(_e) => continue // TODO LOG ERROR
            };
            let protocol_lock = Arc::clone(&protocol);

            let task:Task = Box::new(move || {
                let protocol = match protocol_lock.read() {
                    Ok(read) => read,
                    Err(e) => {
                        return Err(Box::new(PoisonError::new(e.to_string())));
                    } // TODO LOG ERROR
                };
                match protocol.handle_connection(stream, peer) {
                    Ok(result) => { Ok(result) }, // todo log_connection ?
                    Err(e) => {
                        println!("Error handling connection: {:?}", e);
                        println!("Worker: {:?}", std::thread::current().id());
                        // TODO LOG ERROR
                        Err(e)
                    }
                }
            });

            self.thread_pool.queue.push(task);
        }
    }
}