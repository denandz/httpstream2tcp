use clap::Parser;
use futures::{SinkExt, StreamExt, channel::mpsc, future};
use http_body_util::StreamBody;
use http_body_util::{BodyExt, Empty, combinators::BoxBody};
use hyper::body::Body;
use hyper::body::{Bytes, Frame};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::net::{Ipv4Addr, SocketAddr};
use tokio::io;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio_util::codec::{BytesCodec, FramedRead};

#[derive(Parser)]
#[clap(version = concat!(env!("CARGO_PKG_VERSION")))]
struct Args {
    #[clap(
        short,
        long,
        help = "Address to bind the server to.",
        default_value = "0.0.0.0"
    )]
    address: String,
    #[clap(short, long, help = "Port to listen on.", default_value = "3000")]
    port: u16,
    #[clap(
        short,
        long,
        help = "IP to connect to.",
        default_value = "127.0.0.1:22"
    )]
    client_address: String,
    #[clap(short, long, help = "Verbose mode.")]
    verbose: bool,
}

async fn echo(
    req: Request<hyper::body::Incoming>,
    client_addr: SocketAddr,
    peer_addr: SocketAddr,
    verbose: bool,
) -> anyhow::Result<Response<impl Body<Data = Bytes, Error = anyhow::Error>>> {
    println!(
        "[+] {} to {} from {:?}",
        req.method(),
        req.uri().path(),
        peer_addr
    );
    match (req.method(), req.uri().path()) {
        (&Method::PUT, "/stream") => {
            let s = TcpStream::connect(client_addr)
                .await
                .expect("Connection Failed");
            let (s_read, s_write) = s.into_split();

            let mut req_body = req.into_body();
            tokio::task::spawn(async move {
                while let Some(foo) = req_body.frame().await {
                    if foo.is_err() {
                        if verbose {
                            println!("[.] Ending client body read task");
                        }
                        break;
                    }

                    let data = foo.unwrap().into_data();
                    if verbose {
                        println!("-> {:?}", data);
                    }

                    let n = s_write.try_write(&data.unwrap());
                    if let Err(e) = n {
                        eprintln!("[!] try_write error: {:?}", e);
                    }
                }
            });

            let (mut res_body_tx, res_body_rx) =
                mpsc::channel::<Result<Frame<Bytes>, anyhow::Error>>(1);

            tokio::task::spawn(async move {
                let mut server_stream = FramedRead::new(s_read, BytesCodec::new())
                    .filter_map(|i| match i {
                        Ok(i) => {
                            if verbose {
                                println!("<- {:?}", i);
                            }
                            future::ready(Some(i.freeze()))
                        }
                        Err(e) => {
                            eprintln!("[!] failed to read from socket; error={e}");
                            future::ready(None)
                        }
                    })
                    .map(|i| Ok(Ok(Frame::data(i))));

                res_body_tx.send_all(&mut server_stream).await
            });

            let stream_body = StreamBody::new(res_body_rx);
            let resp = Response::builder().body(BoxBody::new(stream_body))?;

            Ok(resp)
        }

        // Return 404 Not Found for other routes.
        _ => {
            let mut not_found = Response::new(empty());
            *not_found.status_mut() = StatusCode::NOT_FOUND;
            Ok(not_found)
        }
    }
}

fn empty() -> BoxBody<Bytes, anyhow::Error> {
    Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed()
}

#[tokio::main()]
async fn main() -> io::Result<()> {
    let args = Args::parse();
    println!("[.] Listening on {}:{}", args.address, args.port);
    println!("[.] Will connect to {}", args.client_address);

    let bind_address: Ipv4Addr = args
        .address
        .parse()
        .expect("[!] Error parsing bind address");
    let addr = SocketAddr::from((bind_address, args.port));
    let client_addr: SocketAddr = args
        .client_address
        .parse()
        .expect("[!] Error parsing client address:port");
    let listener = TcpListener::bind(addr).await?;

    // Continuously accept incoming connections
    loop {
        let (stream, _) = listener.accept().await?;
        let peer_addr = stream.peer_addr()?;
        let io = TokioIo::new(stream);

        // Spawn a tokio task to serve multiple connections concurrently
        tokio::task::spawn(async move {
            if let Err(err) = http1::Builder::new()
                .serve_connection(
                    io,
                    service_fn(move |req| echo(req, client_addr, peer_addr, args.verbose)),
                )
                .await
            {
                eprintln!("[!] Error serving connection: {:?}", err);
            }
        });
    }
}
