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

    let mut prot = http::extract();
    prot.set_config(config);
    prot.use_tls = true;

    let mut server = Server::new();
    server.set_port(443, prot);
    server.set_read_timeout(Some(Duration::from_millis(300)));
    server.start();
}
