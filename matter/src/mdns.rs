// Minimal mDNS runner — identical to the rs-matter examples/src/common/mdns.rs
// but kept local to avoid a path dependency on the examples crate.

use async_io::Async;
use log::{debug, error, info, warn};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::transport::network::mdns::builtin::{BuiltinMdnsResponder, Host};
use rs_matter::transport::network::mdns::{
    MDNS_IPV4_BROADCAST_ADDR, MDNS_IPV6_BROADCAST_ADDR, MDNS_SOCKET_DEFAULT_BIND_ADDR,
};
use rs_matter::transport::network::{Ipv4Addr, Ipv6Addr};
use rs_matter::{Matter, crypto::Crypto};
use socket2::{Domain, Protocol, Socket, Type};
use std::net::UdpSocket;

pub async fn run_mdns<C: Crypto>(matter: &Matter<'_>, crypto: C) -> Result<(), Error> {
    let (ipv4_addr, ipv6_addr, interface) = initialize_network()?;

    let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_only_v6(false)?;
    socket.bind(&MDNS_SOCKET_DEFAULT_BIND_ADDR.into())?;
    let socket = Async::<UdpSocket>::new_nonblocking(socket.into())?;

    socket
        .get_ref()
        .join_multicast_v6(&MDNS_IPV6_BROADCAST_ADDR, interface)?;
    socket
        .get_ref()
        .join_multicast_v4(&MDNS_IPV4_BROADCAST_ADDR, &ipv4_addr)?;

    BuiltinMdnsResponder::new()
        .run(
            &socket,
            &socket,
            &Host {
                hostname: "comelit-matter",
                ip: ipv4_addr,
                ipv6: ipv6_addr,
            },
            Some(ipv4_addr),
            Some(interface),
            matter,
            crypto,
        )
        .await
}

fn initialize_network() -> Result<(Ipv4Addr, Ipv6Addr, u32), Error> {
    let all = if_addrs::get_if_addrs().map_err(|_| ErrorCode::StdIoError)?;
    debug!("Available network interfaces: {:?}", all);

    let find_ipv6_candidate = |ipv6_filter: fn(std::net::Ipv6Addr) -> bool| {
        all.iter()
            .filter(|ia| !ia.is_loopback())
            .filter_map(|ia| match ia.addr {
                if_addrs::IfAddr::V6(ref v6) if ipv6_filter(v6.ip) => {
                    Some((ia.name.clone(), v6.ip, ia.index.unwrap_or(0)))
                }
                _ => None,
            })
            .find_map(|(iname, ipv6, index)| {
                all.iter()
                    .filter(|ia2| ia2.name == iname)
                    .find_map(|ia2| match ia2.addr {
                        if_addrs::IfAddr::V4(ref v4) => {
                            Some((iname.clone(), v4.ip, ipv6, index))
                        }
                        _ => None,
                    })
            })
    };

    let find_fallback_candidate = || {
        all.iter()
            .filter(|ia| !ia.is_loopback())
            .filter(|ia| ia.name.starts_with("eth") || ia.name.starts_with("eno") || ia.name.starts_with("en"))
            .map(|ia| match ia.addr {
                if_addrs::IfAddr::V4(ref v4) => (
                    ia.name.clone(),
                    v4.ip,
                    std::net::Ipv6Addr::UNSPECIFIED,
                    ia.index.unwrap_or(0),
                ),
                if_addrs::IfAddr::V6(ref v6) => (
                    ia.name.clone(),
                    std::net::Ipv4Addr::UNSPECIFIED,
                    v6.ip,
                    ia.index.unwrap_or(0),
                ),
            })
            .next()
    };

    let candidate = find_ipv6_candidate(|ip| ip.is_unicast_link_local())
        .or_else(|| find_ipv6_candidate(|_| true))
        .or_else(|| {
            warn!("No network interface with a suitable IPv6 address found");
            find_fallback_candidate()
        })
        .ok_or_else(|| {
            error!("Cannot find network interface suitable for mDNS");
            ErrorCode::StdIoError
        })?;

    let (iname, ip, ipv6, index) = candidate;
    info!("Using interface {iname} ({ip}/{ipv6}) for mDNS");
    Ok((ip.octets().into(), ipv6.octets().into(), index))
}
