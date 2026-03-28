use std::collections::HashMap;
use std::io;
use std::io::ErrorKind;
use std::sync::Arc;
use rustls::ServerConfig;
use serde_json::Value;
use crate::core::async_runtime::AsyncConnectionFuture;
use crate::applications::async_web::default::{BAD_REQUEST, INTERNAL_SERVER_ERROR, NOT_FOUND};
use crate::applications::async_web::http::{Action, HttpHandler, HttpRequest, HttpResponse, Method};
use crate::applications::async_web::http::Method::ANY;
use crate::core::async_runtime::{AsyncProtocol, AsyncTcpStream};

pub struct Handler {
    pub method: Method,
    pub endpoint: String,
    action: Action,
}

impl Handler {
    pub fn new(method: Method, endpoint:&str, action: Box<HttpHandler>) -> Self {
        Self { method, endpoint:String::from(endpoint), action }
    }

    async fn execute(&self, request:&HttpRequest, response:&mut HttpResponse) -> io::Result<usize> {
        (*self.action)(request, response).await
    }
}


pub enum Phase { PreHandle, PostHandle }


pub struct Http {
    handlers:HashMap<(Method, String), Handler>,
    pre_handlers: HashMap<String, Handler>,
    post_handlers: HashMap<String, Handler>,
    config: Option<Arc<ServerConfig>>,
    pub use_tls: bool,
}

impl AsyncProtocol for Http {
    fn handle_async_connection(&self, mut stream: AsyncTcpStream) -> AsyncConnectionFuture<'_>
    {Box::pin( async move {
        if self.use_tls {
            println!("Start TLS");
            stream.start_tls(Arc::clone(self.config.as_ref().unwrap())).await?;
        }

        let mut query_params:Value = Value::Object(serde_json::Map::new());

        let request_line:(Method, String) = match Self::parse_request_line(&mut stream, &mut query_params).await {
            Ok (request) => request,
            Err(_error) => {
                return stream.write_all(BAD_REQUEST).await;
            }
        };

        println!("request_line: {:?}", request_line);

        if request_line.1.contains("..") {
            return stream.write_all(BAD_REQUEST).await;
        }

        let header:HashMap<String, String> = Self::parse_header(&mut stream).await;

        let content_length:usize = match header.get("content-length") {
            Some(content_length) => {
                content_length.parse::<usize>().unwrap_or(0)
            }
            _ => 0
        };

        let content_type:String = match header.get("content-type") {
            Some(content_type) => content_type.clone(),
            _ => String::from("text/plain")
        };

        let body_params:Value = match Self::parse_body(&mut stream, content_length, content_type).await {
            Ok(Some(body)) => body,
            Err(_error) => {
                return stream.write_all(BAD_REQUEST).await;
            },
            _ => Value::Null
        };

        let method = request_line.0;
        let endpoint= request_line.1;

        let accept_gzip = header.get("accept-encoding")
            .map(|v| v.contains("gzip"))
            .unwrap_or(false);

        let request = HttpRequest::new (
            method.clone(), endpoint.clone(),
            stream.peer_addr().unwrap(), header,
            query_params, body_params
        );
        let mut response = HttpResponse::new(
            stream, 200, HashMap::new(), accept_gzip
        );

        if !self.handle_aop(Phase::PreHandle, endpoint.clone().as_str(), &request, &mut response).await {
            return Ok(1)
        }
        let result = match self.handlers.get(&(method.clone(), endpoint.clone())) {
            Some(handler) => handler.execute(&request, &mut response).await,
            None => {
                match self.search_wildcard(&method, endpoint.as_str()) {
                    Some(wildcard_handler) => wildcard_handler.execute(&request, &mut response).await,
                    None => response.write_bytes(NOT_FOUND).await
                }
            }
        };
        self.handle_aop(Phase::PostHandle, endpoint.as_str(), &request, &mut response).await;
        result
    })}

}

impl Http {
    pub fn new() -> Self {
        Self{
            handlers: HashMap::new(),
            pre_handlers: HashMap::new(),
            post_handlers: HashMap::new(),
            config: None,
            use_tls: false,
        }
    }

    pub fn set_config(&mut self, config:Arc<ServerConfig>) {
        self.config = Some(config);
    }

    async fn parse_request_line(stream:&mut AsyncTcpStream, params:&mut Value)
                          -> io::Result<(Method, String)>
    {
        let mut line:String = String::new();
        stream.read_line(&mut line).await?;

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
                return Err(io::Error::new(ErrorKind::Other, "unknown method"));
            }
        };

        let queries:Vec<&str> = request[1].trim().split("?").collect();
        let endpoint = queries.get(0).unwrap_or(&"");
        let query_string = queries.get(1).unwrap_or(&"");
        decode_query(query_string, params);

        Ok((method, endpoint.to_string()))
    }

    async fn parse_header(stream:&mut AsyncTcpStream) -> HashMap<String, String> {
        let mut header:HashMap<String, String> = HashMap::new();
        let mut line_buf = String::new();
        loop {
            line_buf.clear();
            let n = stream.read_line(&mut line_buf).await.unwrap_or(0);
            if n == 0 || line_buf.trim().is_empty() { break; }

            let kv: Vec<String> = line_buf.trim()
                .split(':')
                .map(|s| s.to_string())
                .collect();

            let key = kv.get(0)
                .unwrap_or(&String::new())
                .to_lowercase()
                .trim().to_string();
            let value = kv.get(1)
                .unwrap_or(&String::new())
                .trim().to_string();

            header.insert(key, value);
        }

        header
    }

    async fn parse_body(stream:&mut AsyncTcpStream, content_length:usize, content_type:String)
                  -> io::Result<Option<Value>>
    {
        let mut body = vec![0; content_length];
        stream.read(&mut body).await?;
        let body_str = String::from_utf8(body).unwrap_or(String::new());

        match content_type.to_lowercase().as_str() {
            "text/plain" => Ok(Some(Value::String(body_str))),
            "application/x-www-form-urlencoded" => Ok(None),
            "multipart/form-data" => Ok(None), // todo() 멀티파트 처리
            "application/json" => {
                let v = serde_json::from_str(body_str.as_str())
                    .map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;
                Ok(Some(v))
            },

            _ => Err(io::Error::new(ErrorKind::Other, "unknown content-type"))
        }
    }

    fn search_wildcard(&self, method: &Method, endpoint:&str) -> Option<&Handler> {
        let mut endpoint_vec = endpoint
            .split('/')
            .collect::<Vec<&str>>();

        while !endpoint_vec.is_empty() {
            let search = endpoint_vec.join("/") + "/*";
            endpoint_vec.remove(endpoint_vec.len() - 1);

            match self.handlers.get( &(method.clone(), search) ){
                Some(handler) => return Some(handler),
                None => { continue; }
            }
        }

        None
    }

    async fn handle_aop(&self, phase:Phase, endpoint:&str, request:&HttpRequest, response: &mut HttpResponse) -> bool {
        let endpoint_vec = endpoint.split('/').collect::<Vec<&str>>();
        let mut search = String::new();

        let handler_set = match phase {
            Phase::PreHandle => &self.pre_handlers,
            Phase::PostHandle => &self.post_handlers,
        };

        for i in 0..endpoint_vec.len() {
            search += endpoint_vec.get(i).unwrap_or(&"");
            let mut result:io::Result<usize> = Ok(0);

            if let Some(pre) = handler_set.get(&search) {
                result = pre.execute(request, response).await;
            }else if let Some(pre) = handler_set.get( &(search.clone()+"/*") ) {
                result = pre.execute(request, response).await;
            }

            match result {
                Ok(0) => search += "/",
                Ok(_n) => return false,
                Err(_e) => {
                    let _r =response.write_bytes(INTERNAL_SERVER_ERROR).await;
                    return false;
                }
            }
        }

        true
    }

    pub fn set_handler(&mut self, method: Method, endpoint:&str, action: Action) {
        self.handlers.insert(
            (method.clone(), endpoint.to_string().clone()),
            Handler::new(method, endpoint, action)
        );
    }

    pub fn set_pre_handler(&mut self, pattern:&str, action: Action) {
        self.pre_handlers.insert(
            pattern.to_string().clone(),
            Handler::new(ANY, pattern, action)
        );
    }

    pub fn set_post_handler(&mut self, pattern:&str, action: Action) {
        self.post_handlers.insert(
            pattern.to_string().clone(),
            Handler::new(ANY, pattern, action)
        );
    }
}

fn decode_query(qs: &str, params:&mut Value) {
    for (k, v) in form_urlencoded::parse(qs.as_bytes()) {
        if let Some(map) =params.as_object_mut() {
            map.insert( k.to_string(), Value::String(v.to_string()) );
        }
    }
}