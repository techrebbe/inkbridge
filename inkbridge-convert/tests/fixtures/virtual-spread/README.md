# Virtual Spread contract fixtures

`page-143-contract-v1.json` is an exact byte-for-byte copy of the frozen synthetic contract fixture
merged in `techrebbe/supernote-rtl-reader` commit
`025d870bd73f1133664aa37b8443feb7ce10d12d` at
`virtual_spread/fixtures/page-143-contract-v1.json`.
Its exact file SHA-256 is
`2a47cd7a461bacb9e0b441ca4ab0e6fc720cf927c667d68b1ad6f44a473cf539`; the integration test
pins that digest so even a different self-consistent vector set is rejected.

It is normative for schema-v3 canonical mapping/view bytes, binary64 and signed-zero behavior,
synthetic point/stroke vectors, document identity, view identity, and cache naming.

`page-143-v1/` is the exact byte-level real-PDF bundle merged in RTL Reader commit
`ebdb7d1108aa4159a02ea0cdcfdfaab82d69e25b` (PR #18). It contains the immutable source PDF,
generated Virtual Spread PDF, exact schema-v3 sidecar, artifact descriptor, and PDF-tail evidence.
The production-fixture verifier pins all artifact hashes, opens both PDFs, strictly validates the
sidecar, derives the inverse locally, and matches the PDF-tail authorities at their frozen offsets.

The generated files are intentionally tracked under short names for Windows checkout portability.
Materialization must publish the PDF under the authenticated `cacheBasename` and publish its exact
`<cacheBasename>.json` sibling. Passing this fixture gate does not assert that native `.mark`
hydration or rollback is hardware-proven; production activation remains disabled until the shared
page-143 annotation and regeneration gate completes.
