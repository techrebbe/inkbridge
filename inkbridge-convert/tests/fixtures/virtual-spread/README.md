# Virtual Spread contract fixtures

`page-143-contract-v1.json` is an exact byte-for-byte copy of the frozen synthetic contract fixture
merged in `techrebbe/supernote-rtl-reader` commit
`025d870bd73f1133664aa37b8443feb7ce10d12d` at
`virtual_spread/fixtures/page-143-contract-v1.json`.
Its exact file SHA-256 is
`2a47cd7a461bacb9e0b441ca4ab0e6fc720cf927c667d68b1ad6f44a473cf539`; the integration test
pins that digest so even a different self-consistent vector set is rejected.

It is normative for schema-v3 canonical mapping/view bytes, binary64 and signed-zero behavior,
synthetic point/stroke vectors, document identity, view identity, and cache naming. It does not
contain a real generated PDF, sidecar hash, or PDF-tail authority and cannot enable production cache
activation.

RTL Reader is preparing those real-PDF artifacts in a separate fixture-only pull request. Import
them only after that exact head is reviewed and merged.
