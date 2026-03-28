use std::collections::HashMap;
use serde_json::Value;
use std::fs::File;
use std::io;
use std::io::Read;
use std::net::SocketAddr;
use std::pin::Pin;
use crate::core::async_runtime::{AsyncFile, AsyncTcpStream};

#[derive(Clone, Debug)]
#[derive(Eq, Hash, PartialEq)]
pub enum Method {
    GET, POST, PUT, DELETE, OPTIONS, HEAD, PATCH, CONNECT, TRACE, ANY
}

pub type AsyncResult<'a> = Pin<Box<dyn Future<Output=io::Result<usize>> + Send + 'a>>;
pub type HttpHandler = dyn for<'a> Fn(&'a HttpRequest, &'a mut HttpResponse) -> AsyncResult<'a> + Send + Sync;
pub type Action = Box<HttpHandler>;


#[derive(Debug)]
pub struct HttpRequest {
    pub method: Method,
    pub endpoint: String,
    pub peer: SocketAddr,
    pub header: HashMap<String, String>,
    pub query_params: Value,
    pub body_params: Value,
}

impl HttpRequest {
    pub fn new(method: Method, endpoint:String, peer:SocketAddr, header:HashMap<String, String>,
               query_params: Value, body_params: Value) -> Self
    { Self { method, endpoint, peer, header, query_params, body_params} }
}


pub struct HttpResponse {
    stream:AsyncTcpStream,
    status:u16,
    header:HashMap<String, String>,
}

impl HttpResponse {

    pub fn new(stream:AsyncTcpStream, status:u16, header:HashMap<String, String>) -> Self {
        Self { stream, status, header }
    }

    pub fn set_status(&mut self, status:u16) -> &mut Self {
        self.status = status;
        self
    }

    async fn write_status(&mut self) -> io::Result<usize> {
        let status_msg = "HTTP/1.1 ".to_string()
            + self.status.to_string().as_str()
            + "\r\n";
        self.stream.write_all(status_msg.as_bytes()).await
    }

    pub fn set_header(&mut self, key:&str, value:&str) -> &mut Self {
        self.header.insert(key.to_string(), value.to_string());
        self
    }

    async fn write_header(&mut self) -> io::Result<usize> {
        let mut total = 0;
        for (k, v) in &self.header {
            total += self.stream.write_all(
                format!("{}:{}\r\n", k, v).as_bytes()
            ).await?;
        }
        total += self.stream.write_all(b"\r\n").await?;
        Ok(total)
    }

    pub async fn write(&mut self, data:&str) -> io::Result<usize> {
        self.set_header("content-length", data.len().to_string().as_str());
        let mut total = self.write_status().await?;
        total += self.write_header().await?;
        total += self.stream.write_all(data.as_bytes()).await?;
        Ok(total)
    }

    pub async fn write_bytes(&mut self, data: &[u8]) -> io::Result<usize> {
        self.stream.write_all(data).await
    }

    pub async fn write_value(&mut self, value:Value) -> io::Result<usize> {
        let value_str = value.to_string();
        self.set_header("content-length", value_str.len().to_string().as_str());
        let mut total = self.write_status().await?;
        total += self.write_header().await?;
        total += self.stream.write_all(value_str.as_bytes()).await?;
        Ok(total)
    }

    pub async fn write_file(&mut self, path: &str) -> io::Result<usize> {
        let mut file = AsyncFile::from( File::open(path)? );
        let content_type = Self::get_content_type(path.rsplit('.').next().unwrap());

        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer).await?;

        let mut total = self.write_status().await?;
        self.set_header("content-type", content_type);
        self.set_header("content-length", buffer.len().to_string().as_str());
        total += self.write_header().await?;
        total += self.stream.write_all(&buffer).await?;

        Ok(total)
    }

    fn get_content_type(extension:&str) -> &str {
        match extension.to_lowercase().as_str() {
            "html" => "text/html",
            "js" => "application/javascript",
            "css" => "text/css",
            "png" => "image/png",
            "jpg" => "image/jpeg",
            "svg" => "image/svg+xml",
            "gif" => "image/gif",
            "ico" => "image/x-icon",
            "ttf" => "font/ttf",
            "otf" => "font/otf",
            _ => "text/plain"
        }
    }
}