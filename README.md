# lichen

A heavily in-development, experimental, and early days installer for aerynOS.
A privileged backend (`lichen_backend`) provides all the relevant logic and
functionality, while a frontend (`cli`) provides a user interface for the
installer.

Additional frontends will be added in the future, but the `cli` frontend is
the reference implementation using the `installer` workflow/control crate.

All communication is performed asynchronously over a Unix domain socket, using
gRPC as the protocol. See `crates/protocols` for the protocol definitions.

Eventually we'll support TCP sockets for remote installations, empowering
a WASM frontend for web-based installations, and more. Before then we need to
get our ducks in a row (and add Polkit middleware checks for the backend, etc).

Much of the code is powered by our [disks-rs](https://github.com/aerynOS/disks-rs)
project, including disk enumeration, analysis, provisioning and dynamic disk strategies.

## Build & Test

### Running the backend

```bash
$ cargo build -p backend
$ sudo ./target/debug/lichen_backend
0.004011159s  INFO lichen_backend: 🚀 Serving on /run/lichen.sock
```

### Running the frontend

```bash
$ cargo run -p cli
```

## License

`lichen` is available under the terms of the [MPL-2.0](https://spdx.org/licenses/MPL-2.0.html)
