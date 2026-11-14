# ip-country.bin

The table of IP address ranges and their countries that UDM shows each
download's server country from (`udm_infrastructure::country`, read by
`udm_domain::country::CountryTable`). It is built into the programs, so no
address is ever sent anywhere to be looked up.

- License: the data is licensed under
  [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) by the Number
  Resource Organization ([nro.net](https://www.nro.net/)): it is the five
  regional internet registries' WHOIS and ASN country data, which the
  [ip-location-db](https://github.com/sapics/ip-location-db) project merges
  into its `geo-whois-asn-country` database. That license governs the table.
  UDM converts the database into this table of ranges (the change the license
  asks to be noted), and carries the attribution in
  `THIRD-PARTY-NOTICES.txt`, which every package ships beside the programs,
  and in the About window.
- Source of this copy: the npm package
  [`@ip-location-db/geo-whois-asn-country`](https://www.npmjs.com/package/@ip-location-db/geo-whois-asn-country),
  version `2.3.2026061719` (data of 17 June 2026), its
  `geo-whois-asn-country-ipv4.csv` and `geo-whois-asn-country-ipv6.csv`. The
  package's own metadata says CC0-1.0, but its README names CC BY 4.0 by the
  NRO for the data, which is what UDM follows.
- Made with `tools/ip-country/make_table.py`, which describes the format.
  IPv6 ranges are kept to /64 prefixes.

**Make it again before every release**, from the project's GitHub releases
rather than npm: the package's README says the data published to npm has
wrong rows and that npm updates stop, and recommends the GitHub releases. The
GitHub releases could not be reached when this copy was made. From the
repository's root:

```bash
gh release download --repo sapics/ip-location-db --dir ipdb --pattern 'geo-whois-asn-country-ipv4.csv' --pattern 'geo-whois-asn-country-ipv6.csv' && python3 tools/ip-country/make_table.py ipdb crates/infrastructure/data/ip-country.bin && rm -r ipdb
```

then replace the source above with the release's tag and date.
[Building UDM](../../../docs/building.md#making-a-release) has the rest of the
release steps. A wrong row only shows a wrong country beside a download.
