use std::error::Error;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;


pub trait Protocol: Send + Sync {
    fn handle_connection(&self, stream:TcpStream, peer:SocketAddr) -> Result<(), Box<dyn Error>>;
    fn set_config(&mut self, config: &Option<Arc<rustls::ServerConfig>>);
    fn get_config(&self) -> &Option<Arc<rustls::ServerConfig>>;
}

pub trait ReadWrite: Read + Write + Send + Sync + 'static {}
impl<T: Read + Write + Send + Sync + 'static> ReadWrite for T {}