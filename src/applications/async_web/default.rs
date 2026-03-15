// ==========================
// 2xx
// ==========================

pub static OK: &[u8] =
    b"HTTP/1.1 200 OK\r\n";

pub static CREATED: &[u8] =
    b"HTTP/1.1 201 Created\r\n";

pub static NO_CONTENT: &[u8] =
    b"HTTP/1.1 204 No Content\r\n";


// ==========================
// 3xx
// ==========================

pub static MOVED_PERMANENTLY: &[u8] =
    b"HTTP/1.1 301 Moved Permanently\r\n";

pub static FOUND: &[u8] =
    b"HTTP/1.1 302 Found\r\n";

pub static NOT_MODIFIED: &[u8] =
    b"HTTP/1.1 304 Not Modified\r\n";


// ==========================
// 4xx
// ==========================

pub static BAD_REQUEST: &[u8] =
    b"HTTP/1.1 400 Bad Request\r\n\
Content-Length: 11\r\n\
Content-Type: text/plain\r\n\r\n\
Bad Request";

pub static UNAUTHORIZED: &[u8] =
    b"HTTP/1.1 401 Unauthorized\r\n";

pub static FORBIDDEN: &[u8] =
    b"HTTP/1.1 403 Forbidden\r\n";

pub static NOT_FOUND: &[u8] =
    b"HTTP/1.1 404 Not Found\r\n\
Content-Length: 9\r\n\
Content-Type: text/plain\r\n\r\n\
Not Found";

pub static METHOD_NOT_ALLOWED: &[u8] =
    b"HTTP/1.1 405 Method Not Allowed\r\n";

pub static REQUEST_TIMEOUT: &[u8] =
    b"HTTP/1.1 408 Request Timeout\r\n";

pub static CONFLICT: &[u8] =
    b"HTTP/1.1 409 Conflict\r\n";

pub static PAYLOAD_TOO_LARGE: &[u8] =
    b"HTTP/1.1 413 Payload Too Large\r\n";

pub static UNSUPPORTED_MEDIA_TYPE: &[u8] =
    b"HTTP/1.1 415 Unsupported Media Type\r\n";

pub static TOO_MANY_REQUESTS: &[u8] =
    b"HTTP/1.1 429 Too Many Requests\r\n";


// ==========================
// 5xx
// ==========================

pub static INTERNAL_SERVER_ERROR: &[u8] =
    b"HTTP/1.1 500 Internal Server Error\r\n\
Content-Length: 21\r\n\
Content-Type: text/plain\r\n\r\n\
Internal Server Error";

pub static NOT_IMPLEMENTED: &[u8] =
    b"HTTP/1.1 501 Not Implemented\r\n";

pub static BAD_GATEWAY: &[u8] =
    b"HTTP/1.1 502 Bad Gateway\r\n";

pub static SERVICE_UNAVAILABLE: &[u8] =
    b"HTTP/1.1 503 Service Unavailable\r\n";

pub static GATEWAY_TIMEOUT: &[u8] =
    b"HTTP/1.1 504 Gateway Timeout\r\n";