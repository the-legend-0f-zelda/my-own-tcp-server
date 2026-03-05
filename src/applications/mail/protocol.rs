use std::error::Error;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::mem::take;
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use crate::applications::mail::smtp::SmtpSession;
use crate::applications::model::Protocol;

pub struct Smtp {
    domain: String,
    config: Option<Arc<ServerConfig>>,
    post_handle: fn(SmtpSession) -> Result<(), Box<dyn Error>>,
}
impl Protocol for Smtp {
    fn handle_connection(&self, mut stream:TcpStream, _peer: SocketAddr)
        -> Result<(), Box<dyn Error>>
    {
        let msg_ready = format!("220 {} ESMTP ready\r\n", &self.domain);
        stream.write_all(msg_ready.as_bytes())?;

        let mut line_buf = String::new();
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut session = SmtpSession::new();

        loop {
            line_buf.clear();
            if reader.read_line(&mut line_buf)? == 0 { return (self.post_handle)(session); }

            match self.build_response( take(&mut line_buf).as_str(), &mut session )
            {
                Some(response) => {
                    println!("response: {}", response);
                    stream.write_all(response.as_bytes())?;
                },
                None => { if session.use_tls == true { break; } }
            }
        }

        // STARTTLS
        stream.write_all(b"220 2.0.0 Ready to start TLS\r\n")?;
        stream.flush()?;

        let conn = ServerConnection::new(self.config.clone().unwrap())?;
        let mut tls_stream = StreamOwned::new(conn, stream);
        session = SmtpSession::new();

        loop {
            line_buf.clear();
            if tls_stream.read_line(&mut line_buf)? == 0 { return (self.post_handle)(session); }

            match self.build_response( take(&mut line_buf).as_str(), &mut session )
            {
                Some(response) => {
                    println!("response: {}", &response);
                    tls_stream.write_all(response.as_bytes())?;
                    tls_stream.flush()?;
                },
                None => {}
            }
        }
    }

    fn set_config(&mut self, config: &Option<Arc<ServerConfig>>) {
        self.config = match *config {
            Some(ref config) => Some(config.clone()),
            None => None
        };
    }
    fn get_config(&self) -> &Option<Arc<ServerConfig>> { &self.config }
}

impl Smtp {
    pub fn new(domain:&str, post_handle:fn(SmtpSession)->Result<(), Box<dyn Error>>) -> Self {
        Self {
            domain: domain.to_string(),
            config: None,
            post_handle
        }
    }

    pub fn build_response(&self, incoming:&str, session:&mut SmtpSession) -> Option<String> {
        println!("{}", incoming);
        if session.is_data {
            match incoming.trim_end_matches(&['\r','\n'][..]) {
                "." => {
                    session.is_data = false;
                    Some(String::from("250 Ok\r\n"))
                },
                "" => {
                    session.is_content = true;
                    None
                },
                _ => {
                    if session.is_content {
                        session.content.push_str(
                            incoming.trim_end_matches( &['\r','\n'][..] )
                        );
                    }
                    None
                }
            }
        }else {
            let mut line_iter = incoming.split_whitespace();
            let command:&str = line_iter.next().unwrap_or("");

            match command.to_uppercase().as_str() {
                "STARTTLS" => {
                    if self.config.is_some() {
                        session.use_tls = true;
                        None
                    }else {
                        Some(format!("500 Unknown command: {}\r\n", command))
                    }
                },
                "EHLO" | "HELO" => {
                    if self.get_config().is_some() {
                        Some(String::from("250 STARTTLS\r\n"))
                    }else {
                        Some(String::from("250 Hello {}\r\n"))
                    }
                }
                "MAIL" => {
                    if let Some(sender) = line_iter.next() {
                        let cleaned = sender
                            .trim_start_matches("FROM:")
                            .trim_start_matches("from:")
                            .trim_matches(&['<','>','\r','\n'][..])
                            .to_string();

                        session.from = cleaned;
                        Some(String::from("250 Ok\r\n"))
                    }else {
                        Some(String::from("501 Syntax error\r\n"))
                    }
                }
                "RCPT" => {
                    if let Some(receiver) = line_iter.next() {
                        let cleaned = receiver
                            .trim_start_matches("TO:")
                            .trim_start_matches("to:")
                            .trim_matches(&['<','>','\r','\n'][..])
                            .to_string();

                        session.to.push(cleaned);
                        Some(String::from("250 Ok\r\n"))
                    }else {
                        Some(String::from("501 Syntax error\r\n"))
                    }
                }
                "DATA" => {
                    session.is_data = true;
                    Some(String::from("354 End data with <CR><LF>.<CR><LF>\r\n"))
                }
                "QUIT" => {
                    Some(String::from("221 Bye\r\n"))
                },
                _ => {
                    Some(format!("500 Unknown command: {}\r\n", command))
                }
            }
        }
    }
}
