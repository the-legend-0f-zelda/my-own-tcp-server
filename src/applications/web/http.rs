use std::collections::HashMap;
use std::net::{SocketAddr};
use serde_json::Value;
use std::error::Error;
use std::fs::File;
use std::io::{Read, Write};
use crate::applications::model::ReadWrite;

#[derive(Clone, Debug)]
#[derive(Eq, Hash, PartialEq)]
pub enum Method {
    GET, POST, PUT, DELETE, OPTIONS, HEAD, PATCH, CONNECT, TRACE
}

pub type Action = Box<dyn Fn(HttpRequest, HttpResponse) -> Result<(), Box<dyn Error>> + Send + Sync>;

pub struct HttpRequest {
    pub method: Method,
    pub endpoint: String,
    pub header: HashMap<String, String>,
    pub query_params: Value,
    pub body_params: Value,
    pub peer: SocketAddr
}

impl HttpRequest {
    pub fn new(method: Method, endpoint:String, header:HashMap<String, String>,
               query_params: Value, body_params: Value, peer: SocketAddr) -> Self
    {
        Self { method, endpoint, header, query_params, body_params, peer }
    }
}


pub struct HttpResponse {
    stream:Box<dyn ReadWrite>,
    status:u16,
    header:HashMap<String, String>,
}

impl HttpResponse {

    pub fn new(stream:Box<dyn ReadWrite>, status:u16, header:HashMap<String, String>) -> Self {
        Self { stream, status, header }
    }

    pub fn set_status(&mut self, status:u16) -> &mut Self {
        self.status = status;
        self
    }

    fn write_status(&mut self) -> Result<(), Box<dyn Error>> {
        let status_msg = "HTTP/1.1 ".to_string()
            + self.status.to_string().as_str()
            + "\r\n";
        Ok(self.stream.write_all(status_msg.as_bytes())?)
    }

    pub fn set_header(&mut self, key:&str, value:&str) -> &mut Self {
        self.header.insert(key.to_string(), value.to_string());
        self
    }

    fn write_header(&mut self) -> Result<(), Box<dyn Error>> {
        for (k, v) in &self.header {
            self.stream.write_all(
                format!("{}:{}\r\n", k, v).as_bytes()
            )?
        }
        self.stream.write_all(b"\r\n")?;
        Ok(())
    }

    pub fn write(&mut self, data:&str) -> Result<(), Box<dyn Error>> {
        self.set_header("content-length", data.len().to_string().as_str());
        self.write_status()?;
        self.write_header()?;
        Ok(self.stream.write_all(data.as_bytes())?)
    }

    pub fn write_value(&mut self, value:Value) -> Result<(), Box<dyn Error>> {
        let value_str = value.to_string();
        self.set_header("content-length", value_str.len().to_string().as_str());
        self.write_status()?;
        self.write_header()?;
        Ok(self.stream.write_all(value_str.as_bytes())?)
    }

    pub fn write_file(&mut self, path: &str) -> Result<(), Box<dyn Error>> {
        let mut file = File::open(path)?;
        let content_type = Self::get_content_type(path.rsplit('.').next().unwrap());

        let file_size = file.metadata()?.len();
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)?;

        self.write_status()?;
        self.set_header("content-type", content_type);
        self.set_header("content-length", file_size.to_string().as_str());
        self.write_header()?;
        self.stream.write_all(&buffer)?;

        Ok(())
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