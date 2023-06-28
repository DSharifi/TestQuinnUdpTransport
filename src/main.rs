use quinn::{ClientConfig, Endpoint, EndpointConfig, ServerConfig};
use rustls;
use std::net::{IpAddr, Ipv4Addr};
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
            let this_ip = turmoil::lookup("server");
            let custom_udp = quinn_adapter::CustomUdp::new(this_ip, udp_listener);
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

            let (_, mut recv) = connection.open_bi().await?;

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
            let this_ip = turmoil::lookup("client");
            let custom_udp = quinn_adapter::CustomUdp::new(this_ip, udp_listener);
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

            tracing::info!("Connecting to server.");
            let connection = endpoint
                .connect((turmoil::lookup("server"), 9999).into(), "client")?
                .await
                .unwrap();

            tracing::info!("Opening bi stream.");
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
        net::{IpAddr, SocketAddr},
        task::Poll,
    };

    use quinn::{self, udp::Transmit, AsyncUdpSocket};
    use std::future::Future;
    use turmoil;

    pub struct CustomUdp {
        ip: IpAddr,
        inner: turmoil::net::UdpSocket,
    }

    impl CustomUdp {
        pub fn new(ip: IpAddr, inner: turmoil::net::UdpSocket) -> Self {
            Self { ip, inner }
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
            _state: &quinn::udp::UdpState,
            cx: &mut std::task::Context,
            transmits: &[Transmit],
        ) -> Poll<Result<usize, io::Error>> {
            let _ = match std::pin::pin!(self.inner.writable()).poll(cx) {
                Poll::Ready(x) => x?,
                Poll::Pending => return Poll::Pending,
            };

            let mut transmits_sent = 0;
            tracing::info!("POLL SEND: Sending {} transmists", transmits.len());
            for transmit in transmits {
                let buffer: &[u8] = &transmit.contents;
                tracing::info!(
                    "POLL SEND: Sending transmit to {} with len {}",
                    transmit.destination,
                    transmit.contents.len(),
                );
                let mut bytes_sent = 0;
                loop {
                    match self.inner.try_send_to(buffer, transmit.destination) {
                        Ok(x) => bytes_sent += x,
                        Err(e) => {
                            if matches!(e.kind(), io::ErrorKind::WouldBlock) {
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

            tracing::info!("POLL SEND: Successfully sent {} transmits", transmits_sent);
            Poll::Ready(Ok(transmits_sent))
        }

        fn poll_recv(
            &self,
            cx: &mut std::task::Context,
            bufs: &mut [IoSliceMut<'_>],
            meta: &mut [quinn::udp::RecvMeta],
        ) -> Poll<io::Result<usize>> {
            let _ = match std::pin::pin!(self.inner.readable()).poll(cx) {
                Poll::Ready(x) => x?,
                Poll::Pending => {
                    return Poll::Pending;
                }
            };

            assert!(bufs.len() == meta.len());
            // Fill up first buf.
            let mut m = &mut meta[0];
            let mut bytes_received = 0;
            match self.inner.try_recv_from(&mut bufs[0]) {
                Ok((x, addr)) => {
                    m.addr = addr;
                    bytes_received += x;
                    tracing::info!("POLL RECV: Received {} data ", x);
                }
                Err(e) => {
                    if matches!(e.kind(), io::ErrorKind::WouldBlock) {
                        cx.waker().wake_by_ref();
                        return Poll::Pending;
                    }
                    return Poll::Ready(Err(e));
                }
            }
            if bytes_received == 0 {
                return Poll::Pending;
            }

            m.len = bytes_received;
            m.stride = bytes_received;
            m.ecn = Some(ECN);
            m.dst_ip = Some(self.ip);

            tracing::info!(
                "POLL RECV: Received {} data with meta {:?} first 10 bytes",
                bytes_received,
                m,
            );
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
