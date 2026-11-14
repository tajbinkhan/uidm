#!/usr/bin/env python3
"""Turn the IP-to-country CSVs into the table UDM ships.

UDM shows each download's server country from a table built into the
program, so no address is ever sent anywhere to look it up. The table comes
from the npm package @ip-location-db/geo-whois-asn-country, whose
geo-whois-asn-country-ipv4.csv and geo-whois-asn-country-ipv6.csv list
inclusive ranges as `start,end,country`.

Usage (Python 3, standard library only):

    npm pack @ip-location-db/geo-whois-asn-country
    tar -xzf ip-location-db-geo-whois-asn-country-*.tgz
    python3 tools/ip-country/make_table.py package crates/infrastructure/data/ip-country.bin

then write the package's version into crates/infrastructure/data/README.md.

The table (before zlib compression):

    b"UDMCTRY1"
    u32 LE  count of IPv4 ranges, then that many u32 LE starts, then their
            2-byte country codes
    u32 LE  count of IPv6 ranges, then that many u64 LE starts (the first
            64 bits of the address), then their 2-byte country codes

Starts are sorted and each range runs to the next start. Each start is
written as its distance from the one before (the first from 0), which
compresses to half the size. Adjacent ranges of
one country are merged, and the gaps between listed ranges are written as
ranges with the code b"\\0\\0", which means no country. IPv6 ranges are kept
to /64 prefixes (a range that starts inside a /64 starts at the next one),
which keeps the table small; no allocation is smaller than that.

`crates/domain/src/country.rs` reads it.
"""

import ipaddress
import struct
import sys
import zlib
from pathlib import Path

NONE = b"\0\0"


def ranges(path, bits, shift):
    """The CSV's ranges as (start, end_exclusive, code), shifted right."""
    out = []
    for line in path.read_text(encoding="ascii").splitlines():
        line = line.strip()
        if not line:
            continue
        start, end, code = line.split(",")
        code = code.strip().upper()
        if len(code) != 2 or not code.isalpha():
            continue
        s = int(ipaddress.ip_address(start)) >> shift
        e = (int(ipaddress.ip_address(end)) + 1) >> shift
        # A range that does not fill its first prefix starts at the next one.
        if (int(ipaddress.ip_address(start)) & ((1 << shift) - 1)) != 0:
            s += 1
        if e > s:
            out.append((s, e, code.encode("ascii")))
    out.sort()
    return out


def table(rs, limit):
    """Sorted starts and codes, gaps as NONE, neighbours of one code merged."""
    starts, codes = [], []

    def push(start, code):
        if codes and codes[-1] == code:
            return
        if starts and starts[-1] == start:
            starts.pop()
            codes.pop()
            if codes and codes[-1] == code:
                return
        starts.append(start)
        codes.append(code)

    at = 0
    for s, e, code in rs:
        if s < at:
            # Overlaps an earlier range: keep the earlier one's addresses.
            s = at
            if s >= e:
                continue
        if s > at:
            push(at, NONE)
        push(s, code)
        at = e
    if at < limit:
        push(at, NONE)
    return starts, codes


def pack(starts, codes, fmt):
    steps = [b - a for a, b in zip([0] + starts, starts)]
    return struct.pack("<I", len(starts)) + b"".join(struct.pack(fmt, s) for s in steps) + b"".join(codes)


def main():
    if len(sys.argv) != 3:
        sys.exit("usage: make_table.py <unpacked package folder> <output file>")
    src, out = Path(sys.argv[1]), Path(sys.argv[2])
    v4 = table(ranges(src / "geo-whois-asn-country-ipv4.csv", 32, 0), 1 << 32)
    v6 = table(ranges(src / "geo-whois-asn-country-ipv6.csv", 128, 64), 1 << 64)
    raw = b"UDMCTRY1" + pack(*v4, "<I") + pack(*v6, "<Q")
    data = zlib.compress(raw, 9)
    out.write_bytes(data)
    print(f"{len(v4[0])} IPv4 and {len(v6[0])} IPv6 ranges, {len(raw)} bytes, {len(data)} compressed")


if __name__ == "__main__":
    main()
