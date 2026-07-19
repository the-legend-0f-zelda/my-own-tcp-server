use std::{
    collections::HashMap,
    time::Duration,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering}
    },
    thread::{self, JoinHandle}
};
use crate::runtime::{
    AsyncProtocol, FIO_POOL, Reactor, ThreadPool,
    net::AsyncTcpStream,
    task::AsyncTask
};
use mio::{Interest, Registry, Token, net::TcpStream};
use rustls::ServerConfig;


pub struct Server {
    port_mappings: HashMap<u16, Arc<dyn AsyncProtocol>>,
    reactor: Arc<Reactor>,
    nio_pool: ThreadPool,
    max_nio_threads: usize,
    max_fio_threads: usize,
    next_token: AtomicUsize,
    config: Option<Arc<ServerConfig>>,
    read_timeout: Option<Duration>
}
impl Server {
    pub fn new() -> Self {
        Self {
            port_mappings: HashMap::new(),
            reactor: Arc::new(Reactor::new()),
            nio_pool: ThreadPool::new(),
            max_nio_threads: 1,
            max_fio_threads: 1,
            next_token: AtomicUsize::new(0),
            config: None,
            read_timeout: None,
        }
    }

    pub fn set_port(&mut self, port: u16, protocol: impl AsyncProtocol) {
        self.port_mappings.insert(port, Arc::new(protocol));
    }

    pub fn set_max_nio_threads(&mut self, max_nio_threads: usize) {
        self.max_nio_threads = max_nio_threads;
    }

    pub fn set_max_fio_threads(&mut self, max_fio_threads: usize) {
        self.max_fio_threads = max_fio_threads;
    }

    pub fn set_config(&mut self, config: Arc<ServerConfig>) {
        self.config = Some(config);
    }

    pub fn set_read_timeout(&mut self, read_timeout: Option<Duration>) {
        self.read_timeout = read_timeout;
    }

    fn next_token(&self) -> Token {
        Token(self.next_token.fetch_add(1, Ordering::Relaxed))
    }

    pub fn listen_port(&self, port: u16, event_registry: Registry) {
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), port);
        let listener = std::net::TcpListener::bind(socket).unwrap();
        println!("listening on port {}", port);

        loop {
            let (std_stream, _peer) = match listener.accept() {
                Ok((stream, _peer)) => (stream, _peer),
                Err(e) => {
                    println!("accept error {}", e);
                    continue;
                }
            };

            let _r = std_stream.set_nonblocking(true);
            let mut stream = TcpStream::from_std(std_stream);

            let token = self.next_token();
            let registry = event_registry.try_clone().unwrap();

            if let Err(_e) = registry.register(&mut stream, token, Interest::READABLE) {
                continue;
            }

            let protocol = match self.port_mappings.get(&port) {
                Some(p) => Arc::clone(p),
                None => continue,
            };

            let event_manager = Arc::clone(&self.reactor);

            let task: AsyncTask = Box::pin(async move {
                let async_stream = AsyncTcpStream::new(stream, token, event_manager, registry);
                protocol.handle_async_connection(async_stream).await?;
                Ok(())
            });

            self.nio_pool.round_robin(task);
        }
    }

    pub fn start(mut self) {
        let mut join_handles: Vec<JoinHandle<()>> = Vec::new();

        self.nio_pool.spawn_workers(self.max_nio_threads);
        FIO_POOL.get_or_init(|| {
            let mut fio_pool = ThreadPool::new();
            fio_pool.spawn_workers(self.max_fio_threads);
            fio_pool
        });

        let server = Arc::new(self);
        let reactor = Arc::clone(&server.reactor);

        for port in &server.port_mappings {
            let server_clone = Arc::clone(&server);
            let port_clone = *port.0;
            let registry_clone = reactor.get_registry_clone();

            let port_handle = thread::spawn(move || {
                server_clone.listen_port(port_clone, registry_clone);
            });
            join_handles.push(port_handle);
        }

        let event_loop = reactor.run();
        join_handles.push(event_loop);

        for handle in join_handles {
            handle.join().unwrap();
        }

        println!("tcp server terminated.");
    }
}
