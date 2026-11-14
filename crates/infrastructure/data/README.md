# ip-country.bin

The table of IP address ranges and their countries that UDM shows each
download's server country from (`udm_infrastructure::country`, read by
`udm_domain::country::CountryTable`). It is built into the programs, so no
address is ever sent anywhere to be looked up.

- Source: the npm package
  [`@ip-location-db/geo-whois-asn-country`](https://www.npmjs.com/package/@ip-location-db/geo-whois-asn-country),
  version `2.3.2026061719` (data of 17 June 2026), its
  `geo-whois-asn-country-ipv4.csv` and `geo-whois-asn-country-ipv6.csv`.
- License: the package is published under
  [CC0-1.0](https://creativecommons.org/publicdomain/zero/1.0/). Its README
  says the WHOIS part of the data comes from the five regional internet
  registries through the Number Resource Organization, which licenses it
  under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/):
  contains data from the Number Resource Organization
  ([nro.net](https://www.nro.net/)).
- Made with `tools/ip-country/make_table.py`, which describes the format and
  how to refresh it. IPv6 ranges are kept to /64 prefixes.

The package's README warns that its npm releases contain some wrong rows and
will stop being updated, in favor of the project's GitHub releases. A wrong
row only shows a wrong country beside a download; refreshing the table from
the GitHub releases' CSVs, which have the same columns, works the same way.
