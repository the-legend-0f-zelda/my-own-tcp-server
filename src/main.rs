use crate::core::async_runtime::{AsyncProtocol, AsyncTcpStream, Server};
use crate::core::runtime::Port;

mod applications;
mod core;


fn main() {
    //test
    struct TestProtocol {}
    impl AsyncProtocol for TestProtocol {
        fn handle_async_connection(&self, mut stream: AsyncTcpStream) -> impl Future<Output=()> + Send {
            async move {
                let mut buf = [0u8; 1024];
                let _num_read =stream.read(&mut buf).await;
                println!("async read result: {:?}", String::from_utf8(buf.to_vec()));
            }
        }
    }

    let mut server:Server<TestProtocol> = Server::new();
    server.set_port(8080, TestProtocol {});
    server.set_max_threads(1);
    server.start();
}
