use quinn;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use tokio;
use turmoil;

fn main() {
    let mut sim = turmoil::Builder::new().build();
    // register a host

    let host = IpAddr::V6(Ipv6Addr::LOCALHOST);
    let port: u16 = 8080;

    sim.host("host", || async {
        // host software goes here
        // let host = quinn::Endpoint::client()

        Ok(())
    });

    // define test code
    sim.client("test", async {
        // we can interact with other hosts from here

        // Create an IPv6 socket address
        let host: IpAddr = IpAddr::V6(Ipv6Addr::LOCALHOST);
        let port: u16 = 8080;

        // Create a socket address
        let sock_addr = SocketAddr::new(host, port);

        let client = quinn::Endpoint::connect(sock_addr, &Default::default()).unwrap();

        Ok(())
    });

    // run the simulation and handle the result
    _ = sim.run();
}
