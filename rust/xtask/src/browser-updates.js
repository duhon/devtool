(async () => {
  const {assert, wait, testid, click} = testDriver;
  const open = async () => {
    if (testid('console-settings-toggle').getAttribute('aria-pressed') !== 'true')
      await click(testid('console-settings-toggle'));
    await wait(() => !testid('settings-update-status').textContent.includes('Checking'));
  };
  await open();
  assert(testid('settings-update-status').textContent === 'Up to date',
    'Equal release versions must not claim an available update');
  assert(!document.querySelector('[data-testid="settings-update-install"]'),
    'An equal version must not be installable');
  const repository = new URL(updateFixture.requests.at(-1)).pathname.split('/').slice(1,3).join('/');
  const version = '65000.0.0', base = `https://github.com/${repository}/releases/download/devtool-v${version}`;
  const release = {
    schemaVersion:1,application:'ServicesDevTools',version,tag:`devtool-v${version}`,
    commit:'a'.repeat(40),repository,branch:'devtool',platform:'darwin',arch:'arm64',
    minimumMacOS:'13.5',archive:'services-macos-arm64.tar.gz',sha256:'b'.repeat(64),
    archiveUrl:`${base}/services-macos-arm64.tar.gz`,
    checksumsUrl:`${base}/checksums.txt`,installerUrl:`${base}/install.sh`,
  };
  updateFixture.error = 'signed out';
  await click(testid('settings-update-check'));
  await wait(() => testid('settings-update-status').textContent.includes('HTTP 404'));
  assert(testid('settings-update-status').getAttribute('role') === 'alert'
    && !document.querySelector('[data-testid="settings-update-install"]'),
    'Missing private access must be an explicit error, never up to date');
  updateFixture.error = null;
  updateFixture.release = {...release,archiveUrl:'https://foreign.invalid/update.tar.gz'};
  await click(testid('settings-update-check'));
  await wait(() => testid('settings-update-status').textContent.includes('trusted'));
  assert(!document.querySelector('[data-testid="settings-update-install"]'),
    'Foreign archive URLs must not become installable');
  updateFixture.release = release;
  testid('settings-update-check').click();
  testid('console-settings-toggle').click();
  await wait(() => testid('console-settings-toggle').querySelector('.console-state-light').dataset.state === 'success',
    'An unseen new release must illuminate Settings green');
  await open();
  assert(testid('settings-update-version').textContent === version
    && testid('settings-update-install').textContent === 'Update',
    'Settings must show the exact approved major version and an installation action');
  await click(testid('console-settings-toggle'));
  assert(testid('console-settings-toggle').querySelector('.console-state-light').dataset.state === 'ready',
    'Viewing the update must acknowledge the traffic-light notification');
  updateFixture.release = null;
  await open();
  await click(testid('settings-update-check'));
  await wait(() => testid('settings-update-status').textContent === 'Up to date');
  await click(testid('console-settings-toggle'));
  assert(updateFixture.downloads.length === 0,
    'Checking releases must not download or install without user consent');
  return true;
})()
