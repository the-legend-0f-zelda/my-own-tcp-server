use std::io;
use std::sync::Arc;
use std::mem::take;
use rustls::ServerConfig;
use crate::applications::async_mail::smtp::SmtpSession;
use crate::core::async_runtime::{AsyncProtocol, AsyncTask, AsyncTcpStream};

pub struct Smtp {
    domain: String,
    config: Option<Arc<ServerConfig>>,
    post_handle: Box<dyn Fn(SmtpSession) -> AsyncTask + Send + Sync>,
}
impl AsyncProtocol for Smtp {
    async fn handle_async_connection(&self, mut stream:AsyncTcpStream) -> io::Result<usize>
    {
        stream.write_all( format!("220 {} ESMTP ready\r\n", &self.domain).as_bytes() ).await?;

        let mut line_buf = String::new();
        let mut session = SmtpSession::new();

        loop {
            line_buf.clear();
            if stream.read_line(&mut line_buf).await? == 0 {
                break;
            }

            if session.is_data {
                match line_buf.trim_end_matches(&['\r','\n'][..]) {
                    "." => {
                        session.is_data = false;
                        stream.write_all(b"250 Ok\r\n").await?;
                    },
                    "" => {
                        session.is_content = true;
                    },
                    _ => {
                        if session.is_content {
                            session.content.push_str(
                                line_buf.trim_end_matches( &['\r','\n'][..] )
                            );
                        }
                    }
                }

                continue;
            }


            let mut line_iter = line_buf.split_whitespace();
            let command:&str = line_iter.next().unwrap_or("");

            match command.to_uppercase().as_str() {
                "STARTTLS" => {
                    if let Some(ref config) = self.config {
                        session.use_tls = true;
                        stream.write_all(b"220 2.0.0 Ready to start TLS\r\n").await?;
                        stream.start_tls(Arc::clone(config)).await?;
                    } else {
                        stream.write_all(format!("500 Unknown command: {}\r\n", command).as_bytes()).await?;
                    }
                },
                "EHLO" | "HELO" => {
                    if self.config.is_some() {
                        stream.write_all(b"250 STARTTLS\r\n").await?;
                    } else {
                        stream.write_all(b"250 Hello\r\n").await?;
                    }
                }
                "MAIL" => {
                    if let Some(sender) = line_iter.next() {
                        let cleaned = sender
                            .trim_start_matches("FROM:")
                            .trim_start_matches("from:")
                            .trim_matches(&['<', '>', '\r', '\n'][..])
                            .to_string();

                        session.from = cleaned;
                        stream.write_all(b"250 Ok\r\n").await?;
                    } else {
                        stream.write_all(b"501 Syntax error\r\n").await?;
                    }
                }
                "RCPT" => {
                    if let Some(receiver) = line_iter.next() {
                        let cleaned = receiver
                            .trim_start_matches("TO:")
                            .trim_start_matches("to:")
                            .trim_matches(&['<', '>', '\r', '\n'][..])
                            .to_string();

                        session.to.push(cleaned);
                        stream.write_all(b"250 Ok\r\n").await?;
                    } else {
                        stream.write_all(b"501 Syntax error\r\n").await?;
                    }
                }
                "DATA" => {
                    session.is_data = true;
                    stream.write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n").await?;
                }
                "QUIT" => {
                    session.quit = true;
                    stream.write_all(b"221 Bye\r\n").await?;
                    break;
                },
                _ => {
                    stream.write_all(format!("500 Unknown command: {}\r\n", command).as_bytes()).await?;
                }
            }
        }

        (self.post_handle)(session).await?;
        return Ok(1);
    }

}

impl Smtp {
    pub fn new(domain:&str, post_handle:impl Fn(SmtpSession)->AsyncTask + Send + Sync + 'static) -> Self {
        Self {
            domain: domain.to_string(),
            config: None,
            post_handle: Box::new(post_handle),
        }
    }

    /*pub fn build_response(&self, incoming:&str, session:&mut SmtpSession) -> Option<String> {
        if session.is_data {
            return match incoming.trim_end_matches(&['\r','\n'][..]) {
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
        }

        let mut line_iter = incoming.split_whitespace();
        let command:&str = line_iter.next().unwrap_or("");

        return match command.to_uppercase().as_str() {
            "STARTTLS" => {
                if self.config.is_some() {
                    session.use_tls = true;
                    None
                } else {
                    Some(format!("500 Unknown command: {}\r\n", command))
                }
            },
            "EHLO" | "HELO" => {
                if self.get_config().is_some() {
                    Some(String::from("250 STARTTLS\r\n"))
                } else {
                    Some(String::from("250 Hello {}\r\n"))
                }
            }
            "MAIL" => {
                if let Some(sender) = line_iter.next() {
                    let cleaned = sender
                        .trim_start_matches("FROM:")
                        .trim_start_matches("from:")
                        .trim_matches(&['<', '>', '\r', '\n'][..])
                        .to_string();

                    session.from = cleaned;
                    Some(String::from("250 Ok\r\n"))
                } else {
                    Some(String::from("501 Syntax error\r\n"))
                }
            }
            "RCPT" => {
                if let Some(receiver) = line_iter.next() {
                    let cleaned = receiver
                        .trim_start_matches("TO:")
                        .trim_start_matches("to:")
                        .trim_matches(&['<', '>', '\r', '\n'][..])
                        .to_string();

                    session.to.push(cleaned);
                    Some(String::from("250 Ok\r\n"))
                } else {
                    Some(String::from("501 Syntax error\r\n"))
                }
            }
            "DATA" => {
                session.is_data = true;
                Some(String::from("354 End data with <CR><LF>.<CR><LF>\r\n"))
            }
            "QUIT" => {
                session.quit = true;
                Some(String::from("221 Bye\r\n"))
            },
            _ => {
                Some(format!("500 Unknown command: {}\r\n", command))
            }
        }
    }*/
}
