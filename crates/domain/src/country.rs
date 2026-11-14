//! Which country a server's address belongs to, from a table of address
//! ranges UDM ships (made by `tools/ip-country/make_table.py`). Nothing is
//! looked up online: the table is read from bytes the caller hands in.

use std::fmt;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

/// An ISO 3166-1 alpha-2 country code, such as `US`: two capital letters.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CountryCode([u8; 2]);

impl CountryCode {
    /// Two ASCII letters, in either case.
    pub fn parse(code: &str) -> Option<CountryCode> {
        match code.as_bytes() {
            [a, b] if a.is_ascii_alphabetic() && b.is_ascii_alphabetic() => {
                Some(CountryCode([a.to_ascii_uppercase(), b.to_ascii_uppercase()]))
            }
            _ => None,
        }
    }

    pub fn as_str(&self) -> &str {
        // Two ASCII letters, as `parse` and `from_bytes` make sure.
        std::str::from_utf8(&self.0).unwrap_or("??")
    }

    /// The two letters as bytes.
    pub fn letters(&self) -> [u8; 2] {
        self.0
    }

    fn from_bytes(bytes: [u8; 2]) -> Option<CountryCode> {
        bytes.iter().all(u8::is_ascii_uppercase).then_some(CountryCode(bytes))
    }
}

impl fmt::Debug for CountryCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Display for CountryCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TryFrom<String> for CountryCode {
    type Error = &'static str;

    fn try_from(code: String) -> Result<Self, Self::Error> {
        CountryCode::parse(&code).ok_or("not a country code")
    }
}

impl From<CountryCode> for String {
    fn from(code: CountryCode) -> String {
        code.as_str().to_string()
    }
}

/// Whether `ip` is an address on the internet, which has a country, rather
/// than one of the ranges kept for private networks, the computer itself,
/// links, documentation, shared address space or multicast.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_multicast()
                || a == 0
                || a >= 240
                // Shared address space (carrier-grade NAT), 100.64.0.0/10.
                || (a == 100 && (64..128).contains(&b)))
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // Unique local fc00::/7, link-local fe80::/10, and the
                // documentation range 2001:db8::/32.
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
                || (first == 0x2001 && v6.segments()[1] == 0x0db8))
        }
    }
}

/// Why bytes are not a country table.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the country table is damaged")]
pub struct TableError;

const MAGIC: &[u8] = b"UDMCTRY1";

/// Address ranges and their countries. Each list of starts is sorted; a
/// range runs from its start to the next one, and a range without a
/// country (between listed ones) has no code.
#[derive(Debug, Clone, Default)]
pub struct CountryTable {
    v4: Vec<u32>,
    v4_codes: Vec<Option<CountryCode>>,
    /// The first 64 bits of IPv6 range starts.
    v6: Vec<u64>,
    v6_codes: Vec<Option<CountryCode>>,
}

/// Reads the table's bytes in order.
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], TableError> {
        let (head, rest) = self.0.split_first_chunk::<N>().ok_or(TableError)?;
        self.0 = rest;
        Ok(*head)
    }

    /// A count, then that many steps between starts, then their codes.
    fn ranges<const N: usize, T>(&mut self, read: fn([u8; N]) -> u128) -> Result<Ranges<T>, TableError>
    where
        T: TryFrom<u128>,
    {
        let count = u32::from_le_bytes(self.take()?) as usize;
        if self.0.len() < count.saturating_mul(N + 2) {
            return Err(TableError);
        }
        let mut starts = Vec::with_capacity(count);
        let mut at: u128 = 0;
        for i in 0..count {
            let step = read(self.take()?);
            // Every start after the first is after the one before.
            if i > 0 && step == 0 {
                return Err(TableError);
            }
            at += step;
            starts.push(T::try_from(at).map_err(|_| TableError)?);
        }
        let codes = (0..count).map(|_| self.take::<2>().map(CountryCode::from_bytes)).collect::<Result<_, _>>()?;
        Ok((starts, codes))
    }
}

/// A table's starts and their countries.
type Ranges<T> = (Vec<T>, Vec<Option<CountryCode>>);

impl CountryTable {
    /// Read a table from its uncompressed bytes (see
    /// `tools/ip-country/make_table.py` for the format).
    pub fn parse(bytes: &[u8]) -> Result<CountryTable, TableError> {
        let mut r = Reader(bytes.strip_prefix(MAGIC).ok_or(TableError)?);
        let (v4, v4_codes) = r.ranges::<4, u32>(|b| u128::from(u32::from_le_bytes(b)))?;
        let (v6, v6_codes) = r.ranges::<8, u64>(|b| u128::from(u64::from_le_bytes(b)))?;
        if !r.0.is_empty() {
            return Err(TableError);
        }
        Ok(CountryTable { v4, v4_codes, v6, v6_codes })
    }

    /// A table of these ranges, for tests: each start and its country.
    pub fn from_ranges(v4: &[(u32, Option<CountryCode>)], v6: &[(u64, Option<CountryCode>)]) -> CountryTable {
        CountryTable {
            v4: v4.iter().map(|r| r.0).collect(),
            v4_codes: v4.iter().map(|r| r.1).collect(),
            v6: v6.iter().map(|r| r.0).collect(),
            v6_codes: v6.iter().map(|r| r.1).collect(),
        }
    }

    /// The country of `ip`, when it is on the internet and the table has it.
    pub fn lookup(&self, ip: IpAddr) -> Option<CountryCode> {
        if !is_public(ip) {
            return None;
        }
        match ip {
            IpAddr::V4(v4) => find(&self.v4, &self.v4_codes, u32::from(v4)),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => find(&self.v4, &self.v4_codes, u32::from(v4)),
                None => find(&self.v6, &self.v6_codes, (u128::from(v6) >> 64) as u64),
            },
        }
    }

    /// How many ranges it holds, IPv4 and IPv6.
    pub fn len(&self) -> (usize, usize) {
        (self.v4.len(), self.v6.len())
    }
}

/// The code of the range holding `key`: the last start at or before it.
fn find<T: Ord + Copy>(starts: &[T], codes: &[Option<CountryCode>], key: T) -> Option<CountryCode> {
    let at = starts.partition_point(|s| *s <= key).checked_sub(1)?;
    codes.get(at).copied().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(c: &str) -> Option<CountryCode> {
        CountryCode::parse(c)
    }

    /// The bytes `make_table.py` writes for these ranges.
    fn bytes(v4: &[(u32, &[u8; 2])], v6: &[(u64, &[u8; 2])]) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.extend((v4.len() as u32).to_le_bytes());
        let mut at = 0;
        for (s, _) in v4 {
            out.extend((s - at).to_le_bytes());
            at = *s;
        }
        v4.iter().for_each(|(_, c)| out.extend(*c));
        out.extend((v6.len() as u32).to_le_bytes());
        let mut at = 0;
        for (s, _) in v6 {
            out.extend((s - at).to_le_bytes());
            at = *s;
        }
        v6.iter().for_each(|(_, c)| out.extend(*c));
        out
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn ranges_run_to_the_next_start_and_gaps_have_no_country() {
        let v4 = [(0, b"\0\0"), (0x0808_0000, b"US"), (0x0809_0000, b"\0\0"), (0x5000_0000, b"DE")];
        let v6 = [(0, b"\0\0"), (0x2a00_1450_0000_0000, b"IE"), (0x2a00_1451_0000_0000, b"\0\0")];
        let t = CountryTable::parse(&bytes(&v4, &v6)).unwrap();
        assert_eq!(t.len(), (4, 3));
        assert_eq!(t.lookup(ip("8.8.8.8")), code("US"));
        assert_eq!(t.lookup(ip("8.8.255.255")), code("US"));
        assert_eq!(t.lookup(ip("8.9.0.0")), None, "a gap");
        assert_eq!(t.lookup(ip("80.0.0.1")), code("DE"));
        assert_eq!(t.lookup(ip("223.255.255.255")), code("DE"), "the last range runs to the end");
        assert_eq!(t.lookup(ip("2a00:1450:4001::5")), code("IE"));
        assert_eq!(t.lookup(ip("2a00:1452::1")), None);
        assert_eq!(t.lookup(ip("::ffff:8.8.4.4")), code("US"), "an IPv4-mapped address is the IPv4 one");
    }

    #[test]
    fn private_and_special_addresses_have_no_country() {
        let t = CountryTable::from_ranges(&[(0, code("ZZ"))], &[(0, code("ZZ"))]);
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.0.1",
            "172.16.5.4",
            "169.254.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "192.0.2.1",
            "::1",
            "fe80::1",
            "fd12::1",
            "2001:db8::1",
            "ff02::1",
            "::ffff:192.168.1.1",
        ] {
            assert_eq!(t.lookup(ip(private)), None, "{private}");
        }
        assert_eq!(t.lookup(ip("1.1.1.1")), code("ZZ"));
        assert_eq!(t.lookup(ip("2606:4700::1111")), code("ZZ"));
    }

    #[test]
    fn a_damaged_table_is_refused() {
        let good = bytes(&[(0, b"\0\0"), (5, b"US")], &[]);
        assert!(CountryTable::parse(&good).is_ok());
        assert_eq!(CountryTable::parse(&good[..good.len() - 1]).unwrap_err(), TableError);
        assert!(CountryTable::parse(b"NOTATABLE").is_err());
        let mut longer = good.clone();
        longer.push(0);
        assert!(CountryTable::parse(&longer).is_err());
        // Starts that do not go up.
        assert!(CountryTable::parse(&bytes(&[(0, b"US"), (0, b"DE")], &[])).is_err());
    }

    #[test]
    fn codes_are_two_letters() {
        assert_eq!(code("us").map(|c| c.to_string()).as_deref(), Some("US"));
        assert_eq!(code("USA"), None);
        assert_eq!(code("1A"), None);
        assert_eq!(serde_json::to_string(&code("DE").unwrap()).unwrap(), r#""DE""#);
        assert!(serde_json::from_str::<CountryCode>(r#""D""#).is_err());
    }
}
