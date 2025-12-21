![cdlogo](https://carefuldata.com/images/cdlogo.png)

# kiaproxy

Kiaproxy is a minimalistic and high performance TCP load balancer for the purpose of high availability.

The source code is minimalistic and understandable, fast compiling, built on [Tokio](https://crates.io/crates/tokio) async-io, and uses very few system resources.
It can run in a container, VM, or baremetal, and should compile to many OS targets.

There is a single algorithm for backend selection, which is an ordered selection based on first available.

The configuration comes from required environment variables:
```
export SERVERS=192.168.1.33:443,192.168.1.34:443,192.168.1.55:443
export LISTENER=0.0.0.0:443
```

The servers list will default to the first item and check each server starting from the first item.
The first server that responds over TCP for a client request is the one selected for use for that client request.

Unlike many load balancers, the kiaproxy "health check" is done per request. There is no UP/DOWN shared state or control loop of health checks,
each request is connected to the first available server and health checks are done per client request.

In this example, we see the first server being offline on the third client request with trandaction id 41b14e15-b659-490a-ab31-a6dd58bda9d8.

```
2025-12-21T00:52:17.223Z - INIT - INFO: kiaproxy TCP load balancer listening on TcpListener { addr: 0.0.0.0:443, fd: 10 } with backends["192.168.1.33:443", "192.168.1.34:443", "192.168.1.55:443"]
2025-12-21T00:53:13.929Z - ce4fe9c1-d281-49d3-bafa-5df113c30549 - INFO: checking for backend 192.168.1.33:443
2025-12-21T00:53:13.930Z - ce4fe9c1-d281-49d3-bafa-5df113c30549 - INFO: selected first online backend 192.168.1.33:443
2025-12-21T00:53:13.930Z - ce4fe9c1-d281-49d3-bafa-5df113c30549 - INFO: 192.168.1.240:58290 connected to backend Ok(192.168.1.33:443)
2025-12-21T00:53:24.533Z - ec0d3625-341c-4548-ab54-430b61e5ed27 - INFO: checking for backend 192.168.1.33:443
2025-12-21T00:53:24.533Z - ec0d3625-341c-4548-ab54-430b61e5ed27 - INFO: selected first online backend 192.168.1.33:443
2025-12-21T00:53:24.534Z - ec0d3625-341c-4548-ab54-430b61e5ed27 - INFO: 192.168.1.240:45262 connected to backend Ok(192.168.1.33:443)
2025-12-21T00:53:34.801Z - 41b14e15-b659-490a-ab31-a6dd58bda9d8 - INFO: checking for backend 192.168.1.33:443
2025-12-21T00:53:34.801Z - 41b14e15-b659-490a-ab31-a6dd58bda9d8 - INFO: checking for backend 192.168.1.34:443
2025-12-21T00:53:34.801Z - 41b14e15-b659-490a-ab31-a6dd58bda9d8 - INFO: selected first online backend 192.168.1.34:443
2025-12-21T00:53:34.801Z - 41b14e15-b659-490a-ab31-a6dd58bda9d8 - INFO: 192.168.1.240:52474 connected to backend Ok(192.168.1.34:443)
```

Kiaproxy is a TCP load balancer and can handle TLS passthrough (SSL/TLS/HTTPS backends), HTTP backends, and TCP/raw backends.

The connection is a bidirectional stream that works well for many types of network connections.

If no servers are available, the first one will be tried 9 times, sleeping for 1 second between each attempt, before disconnecting the client.

If a server is selected for use because it is online and somehow immediately goes offline before the client is connected, the connection will retry 9 times, sleeping for 1 second between each try.

Because of the algorithm, it is better to keep online servers near the "front" (left) of the server list - having many offline servers on the left still works, but the greater number of offline servers before reaching an online server,
the longer the connection build takes. That said, the connection build is still very fast in most uses, even with the health checks happening before the stream is established.

The typical use of kiaproxy is to provide one or more failover endpoints, so that if the primary endpoint is down, the secondary is used, etc etc.
Kiaproxy is especially useful for situations like maintenance or avoiding some types of outages.

#### Non-features

Kiaproxy is so simple that it is easy to adapt the source code to your needs, but this version doesn't intend to expand functionality.
This decision is in order to keep the program small, few dependencies, extremely light on system resources, and purpose built.

The following are _not_ features of kiaproxy:

- fan-out algorithms such as round-robin or random selection
- hot loading of configuration values
- TLS termination
- UDP support
- wasm32-unknown-unknown compile target

The choice to use environment variables also came from the desire to further reduce dependencies, size, and syscalls. See version 0.1.0 for a reference TOML config implementation if desired.
As of version 0.1.1 and onward, kiaproxy will not use the disk at all and can safely be denied disk access entirely.

## So, what about HA of kiaproxy itself?

Kiaproxy eliminates points of failure for the endpoints it proxies, but in order for kiaproxy itself to be highly available, we need a second kiaproxy instance, ideally on separate physical hardware.

The cheap and easy way is to have two different computers running kiaproxy (on separate hardware) and have DNS records for both, however that can still lead to outages.
A better solution is to use [GSLB](https://www.ibm.com/think/topics/global-server-load-balancing), or something like [CARP](https://www.openbsd.org/faq/pf/carp.html),
selecting which kiaproxy server to use in order to minimize downtime for kiaproxy itself.

## Project promises

This project will never use AI-slop. All code is reviewed, tested, and implemented by a human expert. This repository and the crates.io repository are carefully managed and protected.

This project will be maintained as best as is reasonable.
