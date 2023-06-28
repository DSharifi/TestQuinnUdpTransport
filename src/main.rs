use quinn::{ClientConfig, Endpoint, EndpointConfig, ServerConfig};
use rustls;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tracing::{info_span, Instrument};
use turmoil::{net, Builder};

fn main() {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }

    tracing_subscriber::fmt::init();

    let server_addr = (IpAddr::from(Ipv4Addr::UNSPECIFIED), 9999);
    let client_addr = (IpAddr::from(Ipv4Addr::UNSPECIFIED), 8888);

    let mut sim = Builder::new().build();

    sim.host("server", move || {
        async move {
            let udp_listener = net::UdpSocket::bind(server_addr).await.unwrap();
            let custom_udp =
                quinn_adapter::CustomUdp::new("client".to_string(), client_addr.1, udp_listener);
            let config = EndpointConfig::default();

            tracing::info!("Creating endpoint");
            let mut endpoint = Endpoint::new_with_abstract_socket(
                config,
                None,
                custom_udp,
                Arc::new(quinn::TokioRuntime),
            )
            .unwrap();
            endpoint.set_server_config(Some(configure_server().0));
            endpoint.set_default_client_config(configure_client());

            tracing::info!("Waiting for client to connect");

            let connection = endpoint.accept().await.unwrap().await.unwrap();
            tracing::info!("Client connected.");

            tracing::info!("Opening bi directional channel.");

            let (mut send, mut recv) = connection.open_bi().await?;

            tracing::info!("Connected to endpoint");

            let buffer = &mut [0u8; 1024];
            let bytes_read = recv
                .read(buffer)
                .await
                .expect("Client could not read from channel")
                .expect("No bytes read from channel");

            let text_read = std::str::from_utf8(&buffer[..bytes_read]).expect("Invalid UTF-8");
            tracing::info!("Server received  message {:?}.", text_read);

            // let text = connection.read_datagram()
            // endpoint.connect(config, addr, "client")

            // let text = str::from_utf8(buffer);
            // tracing::info!("Got message {:?}. From {:?}.", text, sender_socket);
            Ok(())
        }
        .instrument(info_span!("server"))
    });

    sim.client(
        "client",
        async move {
            let udp_listener = net::UdpSocket::bind(client_addr).await.unwrap();
            let custom_udp =
                quinn_adapter::CustomUdp::new("server".to_string(), server_addr.1, udp_listener);
            let config = EndpointConfig::default();

            let mut endpoint = Endpoint::new_with_abstract_socket(
                config,
                None,
                custom_udp,
                Arc::new(quinn::TokioRuntime),
            )
            .unwrap();
            endpoint.set_server_config(Some(configure_server().0));
            endpoint.set_default_client_config(configure_client());

            let connection = endpoint
                .connect(server_addr.into(), "client")?
                .await
                .unwrap();
            let (mut send, mut recv) = connection.open_bi().await.unwrap();

            send.write_all(b"hello server. From Client.").await.unwrap();

            let buffer = &mut [0u8; 1024];
            let bytes_read = recv
                .read(buffer)
                .await
                .expect("Client could not read from channel")
                .expect("No bytes read from channel");

            let text_read = std::str::from_utf8(&buffer[..bytes_read]).expect("Invalid UTF-8");
            tracing::info!("Client received  message {:?}.", text_read);

            Ok(())
        }
        .instrument(info_span!("client")),
    );

    sim.run().unwrap();
}

mod quinn_adapter {
    const ECN: quinn_udp::EcnCodepoint = quinn_udp::EcnCodepoint::Ect0;

    use std::{
        io::{self, IoSliceMut},
        net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4},
        // net::SocketAddr,
        ops::DerefMut,
        sync::Arc,
        task::Poll,
    };

    use quinn::{self, udp::Transmit, AsyncUdpSocket, Runtime};
    use std::future::Future;
    use turmoil;

    pub struct CustomUdp {
        other: String,
        other_p: u16,
        inner: turmoil::net::UdpSocket,
    }

    impl CustomUdp {
        pub fn new(other: String, other_p: u16, inner: turmoil::net::UdpSocket) -> Self {
            Self {
                other,
                other_p,
                inner,
            }
        }
    }

    impl std::fmt::Debug for CustomUdp {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "CustomUdp")
        }
    }

    impl AsyncUdpSocket for CustomUdp {
        fn poll_send(
            &self,
            state: &quinn::udp::UdpState,
            cx: &mut std::task::Context,
            transmits: &[Transmit],
        ) -> Poll<Result<usize, io::Error>> {
            tracing::info!("POLL SEND! {}", self.other);
            let _ = match std::pin::pin!(self.inner.writable()).poll(cx) {
                Poll::Ready(x) => x?,
                Poll::Pending => return Poll::Pending,
            };

            let mut transmits_sent = 0;
            for transmit in transmits {
                tracing::info!("POLL SEND to {:?}!", transmit);
                let buffer: &[u8] = &transmit.contents;
                let mut bytes_sent = 0;
                loop {
                    tracing::info!(
                        "POLL SEND to {:?}! {}",
                        (turmoil::lookup(self.other.clone()), self.other_p,),
                        buffer.len()
                    );
                    match self
                        .inner
                        .try_send_to(buffer, (turmoil::lookup(self.other.clone()), self.other_p))
                    {
                        Ok(x) => bytes_sent += x,
                        Err(e) => {
                            let a = e.kind();
                            tracing::info!("POLL SNED!111 {:?}", a);
                            if matches!(a, io::ErrorKind::WouldBlock) {
                                cx.waker().wake_by_ref();
                                return Poll::Pending;
                            }
                            return Poll::Ready(Err(e));
                        }
                    }
                    if bytes_sent == buffer.len() {
                        break;
                    }
                    if bytes_sent > buffer.len() {
                        panic!("Bug: Should not send more bytes then in buffer");
                    }
                }
                transmits_sent += 1;
            }

            tracing::info!("POLL SEND! {}", transmits_sent);
            Poll::Ready(Ok(transmits_sent))
        }

        fn poll_recv(
            &self,
            cx: &mut std::task::Context,
            bufs: &mut [IoSliceMut<'_>],
            meta: &mut [quinn::udp::RecvMeta],
        ) -> Poll<io::Result<usize>> {
            tracing::info!("POLL RECV! {}", self.other);
            let _ = match std::pin::pin!(self.inner.readable()).poll(cx) {
                Poll::Ready(x) => {
                    tracing::info!("POLL RECV!2  {:?}", x);
                    x?
                }
                Poll::Pending => {
                    tracing::info!("POLL RECV PENDING;!");
                    return Poll::Pending;
                }
            };

            assert!(bufs.len() == meta.len());
            let mut bufs_received = 0;
            tracing::info!("POLL RECV!2asdfasf  {:?}", bufs.len());
            let buf = &mut bufs[0];
            tracing::info!("POLL RECV!2asdfasf  {:?}", buf.len());
            let mut m = meta[0];
            let mut bytes_received = 0;
            loop {
                match self.inner.try_recv_from(buf.deref_mut()) {
                    Ok((x, addr)) => {
                        tracing::info!("POLL RECV!111 {} {}", addr, x);
                        m.addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), addr.port());
                        bytes_received += x;
                    }
                    Err(e) => {
                        let a = e.kind();
                        tracing::info!("POLL RECV!111 {:?}", a);
                        if matches!(a, io::ErrorKind::WouldBlock) {
                            cx.waker().wake_by_ref();
                            return Poll::Pending;
                        }

                        return Poll::Ready(Err(e));
                    }
                }
                tracing::info!("POLL RECV STOOOOP {} {}", buf.len(), bytes_received);
                if bytes_received == 1200 {
                    tracing::info!("POLL RECV STOOOOP");
                    break;
                }
                // if bytes_received > buf.len() {
                //     panic!("Bug: Should not send more bytes then in buffer");
                // }
            }
            m.len = bytes_received;
            m.stride = 1200;
            m.dst_ip = Some(IpAddr::V4(Ipv4Addr::UNSPECIFIED));

            m.ecn = Some(ECN);
            tracing::info!("POLL RECV z`zzzz! {:?}", m);
            bufs_received += 1;
            tracing::info!("POLL RECV3! {}", bufs_received);
            Poll::Ready(Ok(1))
        }

        fn local_addr(&self) -> io::Result<SocketAddr> {
            self.inner.local_addr()
        }

        fn may_fragment(&self) -> bool {
            false
        }
    }
}

/// Dummy certificate verifier that treats any certificate as valid.
/// NOTE, such verification is vulnerable to MITM attacks, but convenient for testing.
struct SkipServerVerification;

impl SkipServerVerification {
    fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl rustls::client::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::Certificate,
        _intermediates: &[rustls::Certificate],
        _server_name: &rustls::ServerName,
        _scts: &mut dyn Iterator<Item = &[u8]>,
        _ocsp_response: &[u8],
        _now: std::time::SystemTime,
    ) -> Result<rustls::client::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::ServerCertVerified::assertion())
    }
}

fn configure_client() -> ClientConfig {
    let crypto = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_custom_certificate_verifier(SkipServerVerification::new())
        .with_no_client_auth();

    ClientConfig::new(Arc::new(crypto))
}

/// Returns default server configuration along with its certificate.
fn configure_server() -> (ServerConfig, Vec<u8>) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = cert.serialize_der().unwrap();
    let priv_key = cert.serialize_private_key_der();
    let priv_key = rustls::PrivateKey(priv_key);
    let cert_chain = vec![rustls::Certificate(cert_der.clone())];

    let mut server_config = ServerConfig::with_single_cert(cert_chain, priv_key).unwrap();
    let transport_config = Arc::get_mut(&mut server_config.transport).unwrap();
    transport_config.max_concurrent_uni_streams(0_u8.into());

    (server_config, cert_der)
}
