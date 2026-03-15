use std::collections::HashMap;
use std::io;
use std::io::ErrorKind;
use std::sync::Arc;
use rustls::ServerConfig;
use serde_json::Value;
use crate::applications::async_web::default::{BAD_REQUEST, NOT_FOUND};
use crate::applications::async_web::http::{Action, HttpHandler, HttpRequest, HttpResponse, Method};
use crate::core::async_runtime::{AsyncProtocol, AsyncTcpStream};

pub struct Handler {
    pub method: Method,
    pub endpoint: String,
    action: Box<HttpHandler>,
}

impl Handler {
    pub fn new(method: Method, endpoint:&str, action: Box<HttpHandler>) -> Self {
        Self { method, endpoint:String::from(endpoint), action }
    }

    async fn execute(&self, request:HttpRequest, response:HttpResponse) -> io::Result<usize> {
        (*self.action)(request, response).await
    }
}


pub struct Http {
    handlers:HashMap<(Method, String), Handler>,
    config: Option<Arc<ServerConfig>>,
}

impl AsyncProtocol for Http {
    async fn handle_async_connection(&self, mut stream: AsyncTcpStream) -> io::Result<usize> {
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

        let body_params:Value = match Self::parse_body(&mut stream, content_length, content_type).await {
            Ok(Some(body)) => body,
            Err(_error) => {
                return stream.write_all(BAD_REQUEST).await;
            },
            _ => Value::Null
        };


        let request = HttpRequest::new (
            request_line.0, request_line.1, header,
            query_params, body_params
        );
        let mut response = HttpResponse::new(
            stream, 200, HashMap::new()
        );

        match self.handlers.get(&(request.method.clone(), request.endpoint.clone())) {
            Some(handler) => {
                handler.execute(request, response).await
            },
            None => {
                match self.search_wildcard(&request.method, &request.endpoint.as_str())
                {
                    Some(wildcard_handler) => { wildcard_handler.execute(request, response).await },
                    None => { response.write_bytes(NOT_FOUND).await }
                }
            }
        }

    }

/*    fn set_config(&mut self, config: &Option<Arc<ServerConfig>>) {
        self.config = match *config {
            Some(ref config) => Some(config.clone()),
            None => None
        };
    }

    fn get_config(&self) -> &Option<Arc<ServerConfig>> { &self.config }*/
}

impl Http {
    pub fn new() -> Self {
        Self{
            handlers: HashMap::new(),
            config: None
        }
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

            let key = kv.get(0).unwrap_or(&String::new()).trim().to_string();
            let value = kv.get(1).unwrap_or(&String::new()).trim().to_string();
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

    pub fn handle<F, Fut>(&mut self, method: Method, endpoint:&str, action: F)
    where
        Fut: Future<Output = io::Result<usize>> + Send + 'static,
        F: Fn(HttpRequest, HttpResponse) -> Fut + Send + Sync + 'static,
    {
        self.handlers.insert(
            (method.clone(), endpoint.to_string().clone()),
            Handler::new(
                method, &endpoint,
                Box::new(move |req, res| Box::pin(action(req, res)))
            )
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