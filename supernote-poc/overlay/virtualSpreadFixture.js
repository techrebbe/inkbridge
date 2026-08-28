export const PAGE_143_VIRTUAL_SPREAD_FIXTURE = Object.freeze({
  schemaVersion: 1,
  documentId:
    'inkbridge-doc-v1-c9271098e6d98f7fff378c4d630dc9c179cf45cb5283f3559eee910e3afafeb4',
  viewId:
    'inkbridge-view-v1-7cb2c2fda17d5510d33b0a97e702cbc66d5124735be45f810aef6053c1775f30',
  cacheBasename:
    'inkbridge-doc-v1-c9271098e6d98f7fff378c4d630dc9c179cf45cb5283f3559eee910e3afafeb4.inkbridge-view-v1-7cb2c2fda17d5510d33b0a97e702cbc66d5124735be45f810aef6053c1775f30.virtual-spread.pdf',
  generatedPdfSha256:
    '0c895249809a36f382312ae42547ec2f9755e0b4095ce2b8e8a5f6145be3a32f',
  sidecarSha256:
    '37cda3d96db8b2f8f311df60ccfbbd397bbb446b9e4a7451dcbbffc283aff9df',
  mappingAuthoritySha256:
    '646b905c12266774882e0c4d7ebbbca77b2f386f432979ebcbfcda1d9ace268a',
  sourceFileName: 'page-143-source-v1.pdf',
  sourcePageCount: 3,
  spreadSize: [864, 648],
  mappings: [
    {
      sourcePageIndex: 0,
      virtualPageIndex: 0,
      side: 'right',
      sourceRotation: 90,
      sourceBox: [18, 36, 594, 756],
      destination: [432, 151.20000000000002, 864, 496.79999999999995],
      transform: [0, -0.6, 0.6, 0, 410.4, 507.6],
    },
    {
      sourcePageIndex: 1,
      virtualPageIndex: 1,
      side: 'right',
      sourceRotation: 90,
      sourceBox: [18, 36, 594, 756],
      destination: [432, 151.20000000000002, 864, 496.79999999999995],
      transform: [0, -0.6, 0.6, 0, 410.4, 507.6],
    },
    {
      sourcePageIndex: 2,
      virtualPageIndex: 1,
      side: 'left',
      sourceRotation: 90,
      sourceBox: [18, 36, 594, 756],
      destination: [0, 151.20000000000002, 432, 496.79999999999995],
      transform: [0, -0.6, 0.6, 0, -21.599999999999998, 507.6],
    },
  ],
});

export function fixtureForOpenPath(filePath) {
  if (typeof filePath !== 'string') return null;
  const separator = Math.max(filePath.lastIndexOf('/'), filePath.lastIndexOf('\\'));
  const basename = separator >= 0 ? filePath.slice(separator + 1) : filePath;
  return basename === PAGE_143_VIRTUAL_SPREAD_FIXTURE.cacheBasename
    ? PAGE_143_VIRTUAL_SPREAD_FIXTURE
    : null;
}

export function fixtureNativeDescriptor(fixture = PAGE_143_VIRTUAL_SPREAD_FIXTURE) {
  return JSON.stringify({
    schemaVersion: fixture.schemaVersion,
    documentId: fixture.documentId,
    viewId: fixture.viewId,
    cacheBasename: fixture.cacheBasename,
    generatedPdfSha256: fixture.generatedPdfSha256,
    sidecarSha256: fixture.sidecarSha256,
    mappingAuthoritySha256: fixture.mappingAuthoritySha256,
    sourceFileName: fixture.sourceFileName,
    sourcePageCount: fixture.sourcePageCount,
  });
}
