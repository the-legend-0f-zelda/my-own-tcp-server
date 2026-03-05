#[derive(Debug)]
pub struct SmtpSession {
    pub client_domain: String,
    pub from: String,
    pub to: Vec<String>,
    pub cc: String,
    pub date: String,
    pub subject: String,
    pub content: String,
    pub is_data: bool,
    pub is_content: bool,
    pub use_tls: bool
}

impl SmtpSession {
    pub fn new() -> Self {
        Self {
            client_domain: String::new(),
            from: String::new(),
            to: Vec::new(),
            cc: String::new(),
            date: String::new(),
            subject: String::new(),
            content: String::new(),
            is_data: false,
            is_content: false,
            use_tls: false
        }
    }
}