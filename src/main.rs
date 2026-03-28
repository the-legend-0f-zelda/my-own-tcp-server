use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;
use std::time::Duration;
use rustls::pki_types::CertificateDer;
use rustls::ServerConfig;
use rustls_pemfile::{certs, private_key};
use crate::applications::async_web::http::Method::*;
use crate::applications::async_web::protocol::Phase::*;
use crate::core::async_runtime::Server;
use crate::frameworks::web::http;

mod frameworks;
mod applications;
mod core;


fn main() {
    let cert_file = &mut BufReader::new(File::open("./cert/cert.pem").unwrap());
    let key_file = &mut BufReader::new(File::open("./cert/key.pem").unwrap());

    let certs: Vec<CertificateDer> = certs(cert_file).collect::<Result<_, _>>().unwrap();
    let key = private_key(key_file).unwrap().unwrap();

    let config = Arc::new(
        ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key).unwrap()
    );

    http::route(GET, "/test", handler!(_req, res, {
        res.write_file("./examples/hello.html").await
    }));

    http::route(GET, "/img/*", handler!(req, res, {
        res.write_file(
            format!("./examples{}", req.endpoint).as_str()
        ).await
    }));

    /* AOP TEST */
    http::route(GET, "/logged/test1", handler!(_req, res, {
        res.write("<h1>TEST</h1>").await
    }));

    http::route(GET, "/logged/test2/*", handler!(_req, res, {
        res.write("<h1>O.O</h1>").await
    }));
    
    http::filter(PreHandle, "/logged/*", handler!(req, _res, {
        println!("========== PRE HANDLE! ===========");
        println!("endpoint: {}", req.endpoint);
        println!("method: {:?}", req.method);
        println!("peer: {}", req.peer);
        println!("==================================");
        Ok(0)
    }));

    http::filter(PostHandle, "/logged/*", handler!(req, _res, {
        println!("========== POST HANDLE! ===========");
        println!("endpoint: {}", req.endpoint);
        println!("method: {:?}", req.method);
        println!("peer: {}", req.peer);
        println!("==================================");
        Ok(0)
    }));

    let mut prot = http::extract();
    prot.set_config(config);
    prot.use_tls = true;

    let mut server = Server::new();
    server.set_port(443, prot);
    server.set_read_timeout(Some(Duration::from_millis(300)));
    server.start();
}
