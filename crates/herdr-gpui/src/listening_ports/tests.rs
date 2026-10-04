#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::{
    ListeningPorts, Reading,
    scan::{Bind, MAX_PORTS, Port, Ports, parse},
};
use crate::{Error, usage::Host};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

fn port(number: u16, bind: Bind, process: &str) -> Port {
    Port {
        number,
        bind,
        process: process.into(),
    }
}

#[test]
fn listeners_belong_to_the_workspace_their_process_names() {
    // lsof's addresses, as the macOS branch prints them.
    let ports = parse(
        "L 10 *:5173 node\n\
         L 10 [::1]:5173 node\n\
         L 11 127.0.0.1:3000 Google Chrome He\n\
         L 12 *:7000 ControlCenter\n\
         L 13 192.168.1.5:8080 python3\n\
         E 10 w7V\n\
         E 11 w7V\n\
         E 13 wE\n",
    )
    .unwrap();
    assert_eq!(
        ports["w7V"],
        [
            port(3000, Bind::Loopback, "Google Chrome He"),
            // Both sockets of one server are one port, the wider kept.
            port(5173, Bind::Any, "node"),
        ]
    );
    assert_eq!(
        ports["wE"],
        [port(
            8080,
            Bind::Address(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 5))),
            "python3"
        )]
    );
    // A process started outside Herdr belongs to no workspace.
    assert_eq!(ports.len(), 2);
}

#[test]
fn ss_addresses_parse_like_lsof_ones() {
    let ports = parse(
        "L 1 0.0.0.0:3000 node\n\
         L 1 [::]:3000 node\n\
         L 2 127.0.0.53%lo:53 resolved\n\
         L 3 [fe80::1%eth0]:9000 api\n\
         L 4 [::1]:4000 vite\n\
         E 1 w1\nE 2 w1\nE 3 w1\nE 4 w1\n",
    )
    .unwrap();
    let binds: Vec<(u16, Bind)> = ports["w1"]
        .iter()
        .map(|port| (port.number, port.bind))
        .collect();
    assert_eq!(
        binds,
        [
            (53, Bind::Loopback),
            (3000, Bind::Any),
            (4000, Bind::Loopback),
            (
                9000,
                Bind::Address(IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)))
            ),
        ]
    );
}

#[test]
fn malformed_and_untrusted_lines_are_dropped() {
    let long = "w".repeat(65);
    let text = format!(
        "garbage\n\
         L x *:1 a\n\
         L 1 *:0 zero\n\
         L 1 *:70000 big\n\
         L 1 nohost dev\n\
         L 1 example.com:80 named\n\
         L 1 *:81 \x1b[31mred\x07\n\
         L 2 *:82 eq\n\
         L 3 *:83 long\n\
         E 1 w1\n\
         E 2 a=b\n\
         E 3 {long}\n\
         E y w1\n"
    );
    let ports = parse(&text).unwrap();
    assert_eq!(ports.len(), 1);
    // Control characters are stripped from the name; the rest is display text.
    assert_eq!(ports["w1"], [port(81, Bind::Any, "[31mred")]);
}

#[test]
fn a_host_without_tools_says_so() {
    assert!(matches!(parse("N\n"), Err(Error::ListeningPortsTool)));
    // An idle host answers with nothing at all, which is no ports, not an error.
    assert_eq!(parse("\n").unwrap(), Ports::new());
}

#[test]
fn each_workspace_keeps_its_lowest_ports_only() {
    let mut text = String::new();
    for number in (1..=MAX_PORTS as u16 + 8).rev() {
        text.push_str(&format!("L 1 *:{} srv\n", 1000 + number));
    }
    text.push_str("E 1 w1\n");
    let ports = parse(&text).unwrap();
    let numbers: Vec<u16> = ports["w1"].iter().map(|port| port.number).collect();
    assert_eq!(numbers.len(), MAX_PORTS);
    assert_eq!(numbers.first(), Some(&1001));
    assert!(numbers.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn urls_reach_the_host_the_port_is_on() {
    let local = Host::Local;
    let remote = Host::Ssh("me@devbox".into());
    let url = |port: &Port, host: &Host| port.url(host).map(|url| url.as_str().to_owned());
    let any = port(3000, Bind::Any, "node");
    let loopback = port(5173, Bind::Loopback, "vite");
    let lan = port(
        8080,
        Bind::Address(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2))),
        "py",
    );
    let v6 = port(
        9000,
        Bind::Address(IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 2))),
        "api",
    );
    assert_eq!(url(&any, &local).as_deref(), Some("http://localhost:3000/"));
    assert_eq!(
        url(&loopback, &local).as_deref(),
        Some("http://localhost:5173/")
    );
    assert_eq!(url(&lan, &local).as_deref(), Some("http://10.0.0.2:8080/"));
    assert_eq!(url(&v6, &local).as_deref(), Some("http://[fd00::2]:9000/"));
    // A remote server is opened on its host, never on this machine's localhost.
    assert_eq!(url(&any, &remote).as_deref(), Some("http://devbox:3000/"));
    assert_eq!(url(&loopback, &remote), None);
    assert_eq!(url(&lan, &remote).as_deref(), Some("http://10.0.0.2:8080/"));
    assert_eq!(
        url(&any, &Host::Ssh("fd00::9".into())).as_deref(),
        Some("http://[fd00::9]:3000/")
    );
    assert_eq!(url(&any, &Host::Ssh("me@".into())), None);
    assert_eq!(any.address(), "*:3000");
    assert_eq!(loopback.address(), "localhost:5173");
}

#[test]
fn only_a_different_scan_changes_what_is_shown() {
    let mut reading = Reading::default();
    let found = parse("L 1 *:3000 node\nE 1 w1\n").unwrap();
    assert!(reading.apply(Ok(found.clone())));
    assert!(!reading.apply(Ok(found.clone())));
    // A failed scan keeps the last ports and is not a change.
    assert!(!reading.apply(Err(Error::ListeningPorts(Box::new(
        Error::UsageUnreachable
    )))));
    assert_eq!(reading.ports, found);
    let error = reading.error.clone().unwrap();
    assert!(
        error.starts_with("Could not read listening ports"),
        "{error}"
    );
    assert!(error.contains("SSH"), "the cause is kept: {error}");
    assert!(reading.apply(Ok(Ports::new())));
    assert_eq!(reading.error, None);
}

#[test]
fn seeded_hosts_are_looked_up_by_workspace_and_forgotten_when_dropped() {
    let mut ports = ListeningPorts::default();
    let host = Host::Ssh("devbox".into());
    ports.seed(host.clone(), parse("L 1 *:3000 node\nE 1 w1\n").unwrap());
    assert_eq!(ports.get(&host, "w1").len(), 1);
    assert!(ports.get(&host, "w2").is_empty());
    assert!(ports.get(&Host::Local, "w1").is_empty());
    // A host still wanted keeps its reading; no worker is started for it.
    assert!(!ports.poll([host.clone(), host.clone()]));
    assert_eq!(ports.get(&host, "w1").len(), 1);
    assert!(
        ports.poll(std::iter::empty()),
        "dropping shown ports is a change"
    );
    assert!(ports.get(&host, "w1").is_empty());
    assert!(!ports.poll(std::iter::empty()));
}

/// The real step against this machine's own tools: a socket this test holds
/// is listed with this process's id.
#[cfg(unix)]
#[test]
fn this_machine_lists_a_socket_this_process_holds() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let number = listener.local_addr().unwrap().port();
    let mut shell = super::local_shell().unwrap();
    let output = shell
        .run(super::scan::COMMAND, super::STEP_TIMEOUT)
        .unwrap();
    if output.stdout.lines().any(|line| line == "N") {
        // Neither ss nor lsof here; parse must say so rather than show nothing.
        assert!(matches!(
            parse(&output.stdout),
            Err(Error::ListeningPortsTool)
        ));
        return;
    }
    let expected = format!("L {} 127.0.0.1:{number} ", std::process::id());
    assert!(
        output
            .stdout
            .lines()
            .any(|line| line.starts_with(&expected)),
        "{expected:?} missing from {:?}",
        output.stdout
    );
    parse(&output.stdout).unwrap();
}
