use std::error::Error;
use std::fs::File;
use std::io::BufReader;
use std::sync::{Arc, RwLock};
use rustls::ServerConfig;
use rustls_pemfile::{certs, pkcs8_private_keys};
use tcp_server::applications::web::http::Method::GET;
use tcp_server::frameworks::mvc::http_server;
use tcp_server::applications::mail::protocol::Smtp;
use tcp_server::applications::mail::smtp::SmtpSession;
use tcp_server::core::runtime::Port;


mod applications;
mod core;

fn main() {
    //test
    static CONTENT_ROOT: &str = "./examples";

    http_server::handle_request(GET, "/hi", |_req, mut res| {
        res.write("<h1>HI</h1>")?;
        Ok(())
    });

    http_server::handle_request(GET, "/hi/hello/*", |_req, mut res| {
        res.write("<h1>HI HELLO</h1>")?;
        Ok(())
    });

    http_server::handle_request(
        GET, "/does/rwlock/works/*", |_req, mut res| {
        res.write("<h1>RW Lock works OoO</h1>")?;
        Ok(())
    });

    http_server::handle_request(
        GET, "/welcome", |_req, mut res| {
            let mut i:usize = 0;
            i -= 500;
            println!("Welcome {}!", i);
            res.write_file(
                &format!("{}{}", CONTENT_ROOT, "/hello.html")
            )?;
            Ok(())
        }
    );

    http_server::handle_request(
        GET, "/img/*", |req, mut res| {
        res.write_file(
            &format!("{}{}", CONTENT_ROOT, req.endpoint)
        )?;
        Ok(())
    });

    let cert_file = &mut BufReader::new(File::open("./cert/cert.pem").unwrap());
    let key_file = &mut BufReader::new(File::open("./cert/key.pem").unwrap());
    let certs = certs(cert_file).collect::<Result<Vec<_>, _>>().unwrap();
    let key = pkcs8_private_keys(key_file)
        .next().unwrap().unwrap();

    let tls_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key.into())
        .unwrap();

    fn handle_mail(session:SmtpSession) -> Result<(), Box<dyn Error>> {
        println!("@@@ handle mail @@@");
        println!("session: {:?}", session);
        Ok(())
    }

    let mail_proc = Arc::new(RwLock::new(Smtp::new("scamsite.biz", handle_mail)));

    http_server::bind_port(7070);
    http_server::bind_port(443);
    http_server::bind_port(5432);
    http_server::set_max_threads(1);
    http_server::set_tls_config(tls_config);

    let mut multi_proto_server = http_server::clone();
    multi_proto_server.bind_port(Port::new(25, mail_proc));
    multi_proto_server.start();
}
