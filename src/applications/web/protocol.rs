use std::collections::{HashMap};
use std::error::Error;
use std::io;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use serde_json::Value;
use crate::applications::web::default::*;
use crate::applications::web::http::{Action, HttpRequest, HttpResponse, Method};
use crate::applications::web::util::decode_query;
use crate::applications::model::{Protocol};
use crate::applications::model::ReadWrite;


pub struct Handler {
    pub method: Method,
    pub endpoint: String,
    action: Action
}

impl Handler {
    pub fn new(method: Method, endpoint:&str, action: Action) -> Self {
        Self { method, endpoint:String::from(endpoint), action }
    }

    pub fn execute(&self, request:HttpRequest, response:HttpResponse) -> Result<(), Box<dyn Error>> {
        Ok((*self.action)(request, response)?)
    }
}


pub struct Http {
    handlers:HashMap<(Method, String), Handler>,
    config: Option<Arc<ServerConfig>>,
}

impl Protocol for Http {
    fn handle_connection(&self, stream: TcpStream, peer:SocketAddr) -> Result<(), Box<dyn Error>>
    {
        let mut stream_to_handle:Box<dyn ReadWrite> = match self.get_config() {
            Some(config) => {
                let conn = ServerConnection::new(config.clone())?;
                let tls_stream = StreamOwned::new(conn, stream);
                Box::new(tls_stream)
            },
            None => Box::new(stream)
        };

        let mut buf_reader = BufReader::new(&mut *stream_to_handle);
        let mut query_params:Value = Value::Object(serde_json::Map::new());

        let request_line:(Method, String) = match Self::parse_request_line(&mut buf_reader, &mut query_params) {
            Ok (request) => request,
            Err(_error) => {
                stream_to_handle.write_all(BAD_REQUEST)?;
                return Ok(());
            }
        };

        if request_line.1.contains("..") {
            stream_to_handle.write_all(BAD_REQUEST)?;
            return Ok(());
        }

        let header:HashMap<String, String> = Self::parse_header(&mut buf_reader);

        let content_length:usize = match header.get("Content-Length") {
            Some(content_length) => {
                content_length.parse::<usize>().unwrap_or(0)
            }
            _ => 0
        };

        let content_type:String = match header.get("Content-Type") {
            Some(content_type) => content_type.clone(),
            _ => String::from("text/plain")
        };

        let body_params:Value = match Self::parse_body(&mut buf_reader, content_length, content_type) {
            Ok(Some(body)) => body,
            Err(_error) => {
                stream_to_handle.write_all(BAD_REQUEST)?;
                return Ok(());
            },
            _ => Value::Null
        };


        let request = HttpRequest::new (
            request_line.0, request_line.1, header,
            query_params, body_params, peer
        );
        let mut response = HttpResponse::new(
            stream_to_handle, 200, HashMap::new()
        );

        match self.handlers.get(&(request.method.clone(), request.endpoint.clone())) {
            Some(handler) => {
                handler.execute(request, response)?;
            },
            None => {
                // 와일드카드 검색
                match self.search_wildcard(&request.method, &request.endpoint.as_str()){
                    Some(wildcard_handler) => {
                        wildcard_handler.execute(request, response)?;
                    },
                    None => {
                        // 핸들러 없음 => 404
                        response.write_bytes(NOT_FOUND)?;
                    }
                }
            }
        }

        Ok(())
    }

    fn set_config(&mut self, config: &Option<Arc<ServerConfig>>) {
        self.config = match *config {
            Some(ref config) => Some(config.clone()),
            None => None
        };
    }

    fn get_config(&self) -> &Option<Arc<ServerConfig>> { &self.config }
}

impl Http {
    pub fn new() -> Self {
        Self{
            handlers: HashMap::new(),
            config: None
        }
    }

    fn parse_request_line(reader:&mut BufReader<&mut dyn ReadWrite>, params:&mut Value)
        -> Result<(Method, String), Box<dyn Error>>
    {
        let mut line:String = String::new();
        reader.read_line(&mut line)?;

        let request:Vec<&str> = line.trim().split(' ').collect();
        let method:Method = match request[0].trim().to_uppercase().as_str() {
            "GET" => Method::GET,
            "POST" => Method::POST,
            "PUT" => Method::PUT,
            "PATCH" => Method::PATCH,
            "DELETE" => Method::DELETE,
            "HEAD" => Method::HEAD,
            "OPTIONS" => Method::OPTIONS,
            "TRACE" => Method::TRACE,
            "CONNECT" => Method::CONNECT,
            _ => {
                return Err(Box::new(io::Error::new(ErrorKind::Other, "unknown method")));
            }
        };

        let queries:Vec<&str> = request[1].trim().split("?").collect();
        let endpoint = queries.get(0).unwrap_or(&"");
        let query_string = queries.get(1).unwrap_or(&"");
        decode_query(query_string, params);

        Ok((method, endpoint.to_string()))
    }

    fn parse_header(reader:&mut BufReader<&mut dyn ReadWrite>) -> HashMap<String, String> {
        let mut header:HashMap<String, String> = HashMap::new();
        let mut line_buf = String::new();
        loop {
            line_buf.clear();
            let n = reader.read_line(&mut line_buf).unwrap_or(0);
            if n == 0 || line_buf.trim().is_empty() { break; }

            let kv: Vec<String> = line_buf.trim()
                .split(':')
                .map(|s| s.to_string())
                .collect();

            let key = kv.get(0).unwrap_or(&String::new()).trim().to_string();
            let value = kv.get(1).unwrap_or(&String::new()).trim().to_string();
            header.insert(key, value);
        }

        header
    }

    fn parse_body(reader:&mut BufReader<&mut dyn ReadWrite>, content_length:usize, content_type:String)
        -> Result<Option<Value>, Box<dyn Error>>
    {
        let mut body = vec![0; content_length];
        reader.read_exact(&mut body)?;
        let body_str = String::from_utf8(body).unwrap_or(String::new());

        match content_type.to_lowercase().as_str() {
            "text/plain" => Ok(Some(Value::String(body_str))),
            "application/x-www-form-urlencoded" => Ok(None),
            "multipart/form-data" => Ok(None), // todo() 멀티파트 처리
            "application/json" => {
                Ok(Some(serde_json::from_str(body_str.as_str())?))
            },
            _ => Err(Box::new(io::Error::new(ErrorKind::Other, "unknown content-type")))
        }
    }

    fn search_wildcard(&self, method: &Method, endpoint:&str) -> Option<&Handler> {
        let mut endpoint_vec = endpoint
            .split('/')
            .collect::<Vec<&str>>();

        while endpoint_vec.len() > 0 {
            let search = endpoint_vec.join("/") + "/*";
            endpoint_vec.remove(endpoint_vec.len() - 1);
            
            match self.handlers.get( &(method.clone(), search) ){
                Some(handler) => {
                    return Some(handler)
                },
                None => { continue; }
            }
        }

        None
    }

    pub fn handle(&mut self, method: Method, endpoint:&str,
                  action: fn(HttpRequest, HttpResponse) -> Result<(), Box<dyn Error>>)
    {
        self.handlers.insert(
            (method.clone(), endpoint.to_string().clone()),
            Handler::new(method, &endpoint, Box::new(action))
        );
    }

}