# panonoctl-rs

Quick n' dirty REPL for controlling the [Panono
camera](https://www.panono.com/). Inspired by
https://github.com/florianl/panonoctl.

Tested against firmware version 0.3.2-Apricot.product.412. I assume this must be
a relatively old version since many of the jsonrpc methods found from
decompiling the app return generic "method not found" error codes.

Both USB and WiFi connection modes start a DHCP server and hand out IP addresses
to connecting devices, but the SSDP notify packet is only broadcast via WiFi.
The websocket port is only open on WiFi as well and I have not been able to
establish a jsonrpc connection over USB LAN. HTTP for firmware update and UPF
download endpoints seems available on both.

## usage

    git clone https://github.com/trumank/panonoctl-rs
    cd panonoctl-rs
    cargo run --release
