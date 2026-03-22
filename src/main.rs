use std::fs::File;
use std::io;
use std::io::BufReader;
use std::sync::Arc;
use std::time::Duration;
use rustls::pki_types::CertificateDer;
use rustls::ServerConfig;
use rustls_pemfile::{certs, private_key};
use crate::applications::async_web::http::{HttpRequest, HttpResponse};
use crate::applications::async_web::http::Method::GET;
use crate::applications::async_web::protocol::Http;
use crate::core::async_runtime::Server;

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
    server.set_port(8080, TestProtoc1ol {});*/

    async fn handle_hello(_req:HttpRequest, mut res:HttpResponse) -> io::Result<usize> {
        res.write("<h1>helllllloooodododododofoff</h1>").await
    }

    async fn handle_file(_req:HttpRequest, mut res:HttpResponse) -> io::Result<usize> {
        res.write_file("./examples/hello.html").await
    }

    async fn handle_error(_req:HttpRequest, mut res:HttpResponse) -> io::Result<usize> {
        let mut try_error:usize = 0;
        try_error -= 1;
        res.write("hohohohoho").await
    }

    let mut prot = Http::new();
    prot.handle(GET, "/hello", handle_hello);
    prot.handle(GET, "/", handle_file);
    prot.handle(GET, "/error/*", handle_error);

    let cert_file = &mut BufReader::new(File::open("./cert/cert.pem").unwrap());
    let key_file = &mut BufReader::new(File::open("./cert/key.pem").unwrap());

    let certs: Vec<CertificateDer> = certs(cert_file).collect::<Result<_, _>>().unwrap();
    let key = private_key(key_file).unwrap().unwrap();

    let config = Arc::new(
        ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key).unwrap()
    );

    prot.handle(GET, "/img/*", |req, mut res| async move {
        res.write_file(
            format!("./examples{}", req.endpoint).as_str()
        ).await
    });

    prot.set_config(config);
    prot.use_tls = true;

    let mut server = Server::new();
    server.set_port(443, prot);
    server.set_read_timeout(Some(Duration::from_millis(300)));
    server.start();
}
