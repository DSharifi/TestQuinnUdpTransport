use quinn::{Endpoint, EndpointConfig};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use tracing::{info_span, Instrument};
use turmoil::{net, Builder};

fn main() {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }

    tracing_subscriber::fmt::init();

    let server_addr = (IpAddr::from(Ipv4Addr::LOCALHOST), 9999);
    let client_addr = (IpAddr::from(Ipv4Addr::LOCALHOST), 8888);

    let mut sim = Builder::new().build();

    sim.host("server", move || {
        async move {
            let udp_listener = net::UdpSocket::bind(server_addr).await?;
            let custom_udp = quinn_adapter::CustomUdp::new(udp_listener);
            let config = EndpointConfig::default();

            tracing::info!("Creating endpoint");
            let endpoint = Endpoint::new_with_abstract_socket(
                config,
                None,
                custom_udp,
                Arc::new(quinn::TokioRuntime),
            )
            .unwrap();

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
            let udp_listener = net::UdpSocket::bind(client_addr).await?;
            let custom_udp = quinn_adapter::CustomUdp::new(udp_listener);
            let config = EndpointConfig::default();

            let endpoint = Endpoint::new_with_abstract_socket(
                config,
                None,
                custom_udp,
                Arc::new(quinn::TokioRuntime),
            )
            .unwrap();

            let connection = endpoint.connect(server_addr.into(), "client")?.await?;
            let (mut send, mut recv) = connection.open_bi().await?;

            send.write_all(b"hello server. From Client.").await?;

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
        net::SocketAddr,
        ops::{Deref, DerefMut},
        sync::Arc,
        task::{ready, Poll},
    };

    use futures::executor::block_on;
    use quinn::{self, udp::Transmit, AsyncUdpSocket, Runtime};
    use std::future::Future;
    use turmoil;

    pub struct CustomUdp {
        inner: turmoil::net::UdpSocket,
    }

    impl CustomUdp {
        pub fn new(inner: turmoil::net::UdpSocket) -> Self {
            Self { inner }
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
            // if transmits.is_empty() {
            //     Poll::Pending
            // } else {
            // ready!(self.socket.writable())?;
            for transmit in transmits {
                let buffer: &[u8] = &transmit.contents;
                let fut = self.inner.send_to(buffer, transmit.destination);
                let pin = std::pin::pin!(fut);
                let a = pin.poll(cx);
            }

            if transmits.is_empty() {
                Poll::Pending
            } else {
                Poll::Ready(Ok(transmits.len()))
            }
        }

        fn poll_recv(
            &self,
            cx: &mut std::task::Context,
            bufs: &mut [IoSliceMut<'_>],
            meta: &mut [quinn::udp::RecvMeta],
        ) -> Poll<io::Result<usize>> {
            let buf_slice = bufs.get_mut(0).unwrap().deref_mut();
            let meta_0 = meta.get_mut(0).unwrap();
            let x = block_on(self.inner.recv_from(buf_slice));
            match x {
                Ok((size, addr)) => {
                    meta_0.addr = addr;
                    meta_0.ecn = Some(ECN);
                    meta_0.len = size;

                    Poll::Ready(Ok(size))
                }
                Err(e) => {
                    tracing::error!("Error receiving: {:?}", e);
                    Poll::Ready(Err(e))
                }
            }
        }

        fn local_addr(&self) -> io::Result<SocketAddr> {
            self.inner.local_addr()
        }

        fn may_fragment(&self) -> bool {
            false
        }
    }

    fn quinn_endpoint(
        socket: turmoil::net::UdpSocket,
        runtime: Arc<dyn Runtime>,
    ) -> quinn::Endpoint {
        let socket = CustomUdp { inner: socket };
        quinn::Endpoint::new_with_abstract_socket(
            Default::default(),
            None,
            socket,
            Arc::new(quinn::TokioRuntime),
        )
        .expect("Failed to create endpoint")
    }
}
