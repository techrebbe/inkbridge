# Virtual Spread contract fixtures

`scaffold-manifest-v3.json` is deliberately synthetic and non-authoritative. It exercises the
schema-v3 fixture loader without freezing RTL Reader's provisional page-143 digest, view ID, or
cache basename.

After RTL Reader v0.0.25 is merged, copy its normative page-143 contract and generated sidecar into
this directory, add their expected PDF-tail evidence, and replace the scaffold-only integration
test with exact cross-project golden verification. Production cache activation must remain disabled
until that import is reviewed and the Nomad hidden-cache gate passes.
