use std::io;
use crate::applications::async_web::http::{AsyncResult, HttpRequest, HttpResponse};
use crate::applications::async_web::http::Method::GET;
use crate::applications::async_web::protocol::Http;
use crate::core::async_runtime::{AsyncProtocol, AsyncTcpStream, Server};

mod applications;
mod core;


fn main() {
    //test
    /*struct TestProtocol {}
    impl AsyncProtocol for TestProtocol {
        async fn handle_async_connection(&self, mut stream: AsyncTcpStream) -> io::Result<usize> {
            println!("in handle_async_connection");
            //let mut buf = [0u8; 1024];
            let mut buf = String::new();
            let _num_read =stream.read_line(&mut buf).await;
            while stream.read_line(&mut buf).await.unwrap() != 0 {
                println!("line read - {}", buf);
                if buf == "\r\n" || buf == "\n" {
                    buf.clear();
                    break;
                }
                buf.clear();
            }
            stream.write_all(b"<h1>Write Test OK</h1>").await
            //println!("async read result: {:?}", String::from_utf8(buf.to_vec()));
        }
    }

    let mut server:Server<TestProtocol> = Server::new();
    server.set_port(8080, TestProtocol {});*/

    async fn handle_hello(_req:HttpRequest, mut res:HttpResponse) -> io::Result<usize> {
        res.write("<h1>helllllloooodododododofoff</h1>").await
    }

    let mut prot = Http::new();
    prot.handle(GET, "/hello", handle_hello);

    let mut server:Server<Http> = Server::new();
    server.set_max_threads(1);
    server.set_port(8080, prot);
    server.start();
}
