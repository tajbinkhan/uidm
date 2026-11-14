//! The country of a server's address, from the table built into UDM
//! (`data/ip-country.bin`, see `data/README.md`). It is unpacked the first
//! time it is asked, once; nothing is ever looked up online.

use std::io::Read;
use std::net::IpAddr;
use std::sync::OnceLock;

use udm_domain::country::{CountryCode, CountryTable};

/// The table as shipped: zlib-compressed.
static PACKED: &[u8] = include_bytes!("../data/ip-country.bin");

/// The table, unpacked; empty when it cannot be read, which a test would
/// catch before a release.
fn table() -> &'static CountryTable {
    static TABLE: OnceLock<CountryTable> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut bytes = Vec::new();
        let read = flate2::read::ZlibDecoder::new(PACKED).read_to_end(&mut bytes);
        match read.map_err(|e| e.to_string()).and_then(|_| CountryTable::parse(&bytes).map_err(|e| e.to_string())) {
            Ok(table) => table,
            Err(e) => {
                tracing::warn!(error = %e, "the country table could not be read");
                CountryTable::default()
            }
        }
    })
}

/// The country of `ip`; `None` for a private or unknown address.
pub fn of(ip: IpAddr) -> Option<CountryCode> {
    table().lookup(ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_table_knows_well_known_servers() {
        let (v4, v6) = table().len();
        assert!(v4 > 100_000 && v6 > 50_000, "{v4} and {v6} ranges");
        let country = |ip: &str| of(ip.parse().unwrap()).map(|c| c.to_string());
        assert_eq!(country("8.8.8.8").as_deref(), Some("US"));
        assert_eq!(country("2001:4860:4860::8888").as_deref(), Some("US"));
        assert_eq!(country("193.0.6.139").as_deref(), Some("NL"), "RIPE NCC");
        assert_eq!(country("202.12.29.205").as_deref(), Some("AU"), "APNIC");
        assert_eq!(country("192.168.1.1"), None);
        assert_eq!(country("127.0.0.1"), None);
    }
}
