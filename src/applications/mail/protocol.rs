use std::error::Error;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use rustls::ServerConfig;
use crate::applications::mail::smtp::SmtpSession;
use crate::applications::model::Protocol;

pub struct Smtp {
    domain: String
}
impl Protocol for Smtp {
    fn handle_connection(&self, mut stream:TcpStream, _peer: SocketAddr, _config: Option<Arc<ServerConfig>>)
        -> Result<(), Box<dyn Error>>
    {
        let msg_ready = format!("220 {} ESMTP ready\r\n", &self.domain);
        stream.write_all(msg_ready.as_bytes())?;

        let mut line_buf = String::new();
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut session = SmtpSession::new();

        loop {
            line_buf.clear();
            if reader.read_line(&mut line_buf)? == 0 { break; }
            println!("received: {}", line_buf);

            if session.is_data {
                match line_buf.trim_end_matches(&['\r','\n'][..]) {
                    "." => {
                        session.is_data = false;
                        stream.write_all(b"250 Ok\r\n")?;
                        continue;
                    },
                    "" => {
                        session.is_content = true;
                    },
                    _ => {
                        if session.is_content {
                            session.content.push_str(
                                &line_buf.trim_end_matches( &['\r','\n'][..] )
                            );
                        }
                    }
                }
            }else {
                let mut line_iter = line_buf.split_whitespace();
                let command:&str = line_iter.next().unwrap_or("");
                let mut reply = String::new();

                match command {
                    "EHLO" => {
                        let client = line_iter.next().unwrap_or("");
                        reply = format!("250 Hello {}\r\n", client);
                    }
                    "MAIL" => {
                        if let Some(sender) = line_iter.next() {
                            let cleaned = sender
                                .trim_start_matches("FROM:")
                                .trim_start_matches("from:")
                                .trim_matches(&['<','>','\r','\n'][..])
                                .to_string();

                            session.from = cleaned;
                            reply = "250 Ok\r\n".to_string();
                        }else {
                            reply = "501 Syntax error\r\n".to_string();
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
                            reply = "250 Ok\r\n".to_string();
                        }else {
                            reply = "501 Syntax error\r\n".to_string();
                        }
                    }
                    "DATA" => {
                        reply = "354 End data with <CR><LF>.<CR><LF>\r\n".to_string();
                        session.is_data = true;
                    }
                    "QUIT" => {
                        stream.write_all("221 Bye\r\n".to_string().as_bytes())?;
                        break;
                    },
                    _ => { reply = format!("500 Unknown command: {}\r\n", command); }
                }

                println!("reply: {}", reply);
                stream.write_all(reply.as_bytes())?;
            }
        }

        println!("result: {:#?}", session);
        Ok(())
    }
}

impl Smtp {
    pub fn new(domain:&str) -> Self {
        Self {domain: domain.to_string()}
    }
}
