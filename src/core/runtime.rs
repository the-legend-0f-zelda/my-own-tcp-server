use std::error::Error;
use std::net::TcpListener;
use std::sync::{Arc, PoisonError, RwLock};
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;
use crate::applications::model::Protocol;
use crate::core::executor::{TaskQueue, ThreadPool};

#[derive(Clone)]
pub struct Server {
    ports: Vec<Port>,
    max_threads: u16,
    tls_config: Option<Arc<rustls::ServerConfig>>,
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

    pub fn new(ports:Vec<Port>) -> Self {
        Server {
            ports,
            max_threads: 0,
            tls_config: None,
        }
    }

    pub fn bind_port(&mut self, port:Port) -> &mut Self {
        self.ports.push(port);
        self
    }

    pub fn set_max_threads(&mut self, max_threads:u16) -> &mut Self {
        self.max_threads = max_threads;
        self
    }

    pub fn set_tls_config(&mut self, tls_config:rustls::ServerConfig) -> &mut Self {
        self.tls_config = Some(Arc::new(tls_config));
        self
    }

    pub fn start(self) {
        let mut join_handles:Vec<JoinHandle<()>> = Vec::new();
        let thread_pool = ThreadPool::new(self.max_threads);
        let server = Arc::new(self);

        for port in &server.ports {
            let server_clone = server.clone();
            let port_clone = port.clone();
            let task_queue = thread_pool.queue.clone();

            port.protocol
                .write()
                .unwrap()
                .set_config(&server.tls_config);

            let port_handle = thread::spawn(move || {
                server_clone.listen_port(port_clone, task_queue);
            });

            join_handles.push(port_handle);
        }

        for handle in join_handles {
            handle.join().unwrap();
        }
    }

    fn listen_port(&self, port:Port, task_queue: Arc<TaskQueue>) {
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

            task_queue.push(task);
        }
    }
}