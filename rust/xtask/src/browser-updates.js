(async () => {
  const {assert, wait, testid, click, openConsole, activateSession} = testDriver;
  // A fresh panel: wait for the restored Console session before using its toolbar.
  await openConsole();
  const status = () => testid('settings-update-status');
  const text = () => status().querySelector('.console-status-text').textContent;
  const debug = () => document.querySelector('[data-testid="settings-update-status-debug"]');
  const body = () => testid('settings-panel');
  const noBodyErrors = () => assert(!body().querySelector('[role="alert"], [role="status"]'),
    'Update progress and errors belong in the shared footer, never in the Settings body');
  const showDebug = async () => {
    if (debug().hidden) await click(status());
    await wait(() => !debug().hidden);
  };
  const open = async () => {
    if (testid('console-settings-toggle').getAttribute('aria-pressed') !== 'true')
      await click(testid('console-settings-toggle'));
    await wait(() => !text().includes('Checking'));
    await showDebug();
  };
  await open();
  await wait(() => testid('agent-model-input') && !testid('agent-model-input').disabled,
    () => `Model settings did not load: ${text()}`);
  await click(testid('settings-update-check'));
  await wait(() => text() === 'Up to date');
  assert(text() === 'Up to date',
    'Equal release versions must not claim an available update');
  assert(status().classList.contains('console-status')
    && status().closest('.console-right-status')
    && !status().closest('[data-testid="settings-panel"]'),
    'Settings must reuse the same bottom status region as other panels');
  assert([...body().querySelectorAll('button')].every(button => button.classList.contains('tab-button')),
    'Settings actions must reuse shared panel button styling');
  const bodyBox = body().getBoundingClientRect(), footerBox = status().getBoundingClientRect();
  assert(footerBox.top >= bodyBox.bottom - 1 && footerBox.width > 0,
    'Settings status must be visible below the panel body');
  noBodyErrors();
  updateFixture.pauseCheck = true;
  await click(testid('settings-update-check'));
  await wait(() => status().classList.contains('is-running')
    && status().querySelector('.console-status-spinner'));
  assert(testid('settings-update-check').disabled
    && testid('settings-title').querySelector('.console-state-light').dataset.state === 'running',
    'Checking must disable repeated clicks and animate the shared panel indicator');
  await wait(() => updateFixture.resumeCheck);
  try {
    await showDebug();
    await wait(() => !debug().hidden && debug().textContent.includes('Check for updates')
      && debug().textContent.includes('checking'),
      () => `Debug must show a running check: hidden=${debug().hidden}; ${debug().textContent}`);
  } finally {
    updateFixture.pauseCheck = false;
    updateFixture.resumeCheck();
    delete updateFixture.resumeCheck;
  }
  await wait(() => text() === 'Up to date');
  assert(debug().textContent.includes('current') && !status().querySelector('.console-status-spinner'),
    'An open Debug must update with the result and stop the running spinner');
  assert(!document.querySelector('[data-testid="settings-update-install"]'),
    'An equal version must not be installable');
  const repository = new URL(updateFixture.requests.at(-1)).pathname.split('/').slice(1,3).join('/');
  const version = '65000.0.0', base = `https://github.com/${repository}/releases/download/devtool-v${version}`;
  const release = {
    schemaVersion:1,application:'ServicesDevTools',version,tag:`devtool-v${version}`,
    commit:'a'.repeat(40),repository,branch:updateFixture.branch,platform:'darwin',arch:'arm64',
    minimumMacOS:'13.5',archive:'services-macos-arm64.tar.gz',sha256:'b'.repeat(64),
    archiveUrl:`${base}/services-macos-arm64.tar.gz`,
    checksumsUrl:`${base}/checksums.txt`,installerUrl:`${base}/install.sh`,
  };
  updateFixture.error = 'signed out';
  await click(testid('settings-update-check'));
  await wait(() => text().includes('HTTP 404'));
  assert(status().classList.contains('is-error') && debug().textContent.includes('HTTP 404')
    && !document.querySelector('[data-testid="settings-update-install"]'),
    'Missing private access must be an explicit error, never up to date');
  noBodyErrors();
  updateFixture.error = null;
  updateFixture.release = {...release,archiveUrl:'https://foreign.invalid/update.tar.gz'};
  await click(testid('settings-update-check'));
  await wait(() => text().includes('trusted'));
  assert(debug().textContent.includes('trusted'),
    'Rejected update metadata must also be visible in Debug');
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
  await wait(() => text() === 'Up to date');
  assert(updateFixture.installs.length === 0,
    'Checking releases must not download or install without user consent');
  const available = async () => {
    updateFixture.release = release;
    await click(testid('settings-update-check'));
    await wait(() => document.querySelector('[data-testid="settings-update-install"]'),
      () => `Approved update must become available: ${text()}`);
  };
  await available();
  const getViews = chrome.extension.getViews;
  const reload = chrome.runtime.reload;
  try {
    chrome.extension.getViews = () => [window, {document}];
    await click(testid('settings-update-install'));
    await wait(() => text().includes('Close other Services DevTools windows'));
    assert(debug().textContent.includes('Close other Services DevTools windows'),
      'A blocked update must report its guard failure in status and Debug');
    noBodyErrors();
    chrome.extension.getViews = () => [window];
    assert(updateFixture.installs.length === 0,
      'Failed update guards must not send an installation request');
    await available();
    updateFixture.installError = 'Fixture installer failed before activation';
    await click(testid('settings-update-install'));
    await wait(() => text().includes(updateFixture.installError));
    assert(debug().textContent.includes(updateFixture.installError)
      && status().classList.contains('is-error'),
      'Native installation failures must be visible in status and Debug');
    noBodyErrors();
    updateFixture.installError = null;
    await available();
    updateFixture.installResult = {reload:true,installedVersion:version};
    chrome.runtime.reload = () => { throw new Error('Fixture automatic reload failed'); };
    await click(testid('settings-update-install'));
    await wait(() => text().includes('automatic reload failed'));
    assert(debug().textContent.includes('Fixture automatic reload failed')
      && status().classList.contains('is-error') && testid('settings-update-reload'),
      'Post-install reload errors must remain in status and Debug with a retry action');
    noBodyErrors();
  } finally {
    chrome.extension.getViews = getViews;
    chrome.runtime.reload = reload;
    updateFixture.installError = null;
    updateFixture.installResult = null;
  }
  await click(testid('console-settings-toggle'));
  // A session switch rechecks updates in the background; the model catalog that
  // loads later when Settings opens must not hide that unseen failure.
  const light = id => testid(id).querySelector('.console-state-light').dataset.state;
  updateFixture.error = 'signed out';
  try {
    await activateSession(0);
    await wait(() => light('console-settings-toggle') === 'error',
      () => `A failed background update check must illuminate Settings red: ${light('console-settings-toggle')}`);
    await click(testid('console-settings-toggle'));
    await wait(() => testid('agent-model-input') && !testid('agent-model-input').disabled,
      () => `Model settings did not load: ${text()}`);
    assert(text().includes('HTTP 404') && light('settings-title') === 'error',
      `A newer successful Settings operation must not hide an unseen update error: ${text()}`);
    await click(testid('console-settings-toggle'));
    assert(light('console-settings-toggle') === 'ready',
      'Viewing the update error must acknowledge the traffic-light notification');
  } finally {
    updateFixture.error = null;
  }
  return true;
})()
