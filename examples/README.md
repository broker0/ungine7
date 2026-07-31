# Examples

This directory contains experimental binaries built on the workspace crates.
They demonstrate protocol handling, local game-server behavior, proxies,
recording and replay, and client-data inspection. They are not production-ready.

Run an example from the repository root with:

```powershell
cargo run -p <package-name> -- [arguments]
```

## Servers and Clients

| Example | Package | Purpose |
| --- | --- | --- |
| [`client`](client) | `client-example` | Minimal protocol client. |
| [`text-client`](text-client) | `text-client` | Interactive text-based client. |
| [`server`](server) | `server` | Minimal server using the protocol crate directly. |
| [`simple-server`](simple-server) | `simple-server` | Minimal server using the network crate. |
| [`demo-server`](demo-server) | `demo-server` | Feature-oriented local server demonstration. |
| [`path-server`](path-server) | `path-server` | Server demonstration focused on pathing and world interaction. |

## Proxies and Replay

| Example | Package | Purpose |
| --- | --- | --- |
| [`proxy`](proxy) | `proxy-example` | Basic protocol proxy. |
| [`simple-proxy`](simple-proxy) | `simple-proxy` | Simplified proxy example. |
| [`memory-proxy`](memory-proxy) | `memory-proxy` | In-memory proxy experiment. |
| [`mirror-proxy`](mirror-proxy) | `mirror-proxy` | Proxy and mirroring experiment. |
| [`rpc-proxy`](rpc-proxy) | `rpc-proxy` | RPC-controlled proxy experiment. |
| [`web-proxy`](web-proxy) | `web-proxy` | Web-facing proxy experiment. |
| [`replay-proxy`](replay-proxy) | `replay-proxy` | Traffic recording, preprocessing, and replay tools. |

## Utilities

| Example | Package | Purpose |
| --- | --- | --- |
| [`data-files`](data-files) | `data-files` | Inspect supported client-data file formats. |
| [`benchmark`](benchmark) | `benchmark` | Local server, client, and movement benchmarks. |
| [`common`](common) | `common` | Shared support crate used by several examples. |

## Educational Use Only

These examples are solely for education and research. Run them only against
systems and data you own or are explicitly authorized to use. Do not expose
them to public networks, use real account credentials, automate third-party
services, bypass security controls, or violate a service's rules or applicable
law.

The examples may handle legacy plaintext authentication and are intentionally
minimal. Do not treat them as a secure deployment template.
