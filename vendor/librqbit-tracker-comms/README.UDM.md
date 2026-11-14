# librqbit-tracker-comms, patched for UDM

This is `librqbit-tracker-comms` 9.1.0 from crates.io
(<https://github.com/ikatson/rqbit>, Apache-2.0, by Igor Katson), which
librqbit uses for its HTTP and UDP trackers. The workspace's
`[patch.crates-io]` puts it in place of the published crate.

One change, in `src/tracker_comms_udp.rs`, `UdpTrackerClient::new`: when the
dual-stack UDP socket (`[::]:0`) cannot be made because the system has no
IPv6 (Linux booted with `ipv6.disable=1`), it binds `0.0.0.0:0` instead. The
published crate fails there, and with it the whole BitTorrent session, so no
torrent could run on such a system. Drop this copy once a librqbit release
does the same.

`LICENSE` is the Apache License 2.0 the crate is published under (the
published package carries no copy of its own).
