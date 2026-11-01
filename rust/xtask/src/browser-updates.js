(async () => {
  const {assert, wait, testid, click, openConsole, nativeHold, addFolderSource} = testDriver;
  // A fresh panel: wait for the restored Console session before using its toolbar.
  await openConsole();
  const status = () => testid('settings-update-status');
  const text = () => status().querySelector('.console-status-text').textContent;
  const debug = () => document.querySelector('[data-testid="settings-update-status-debug"]');
  const body = () => testid('settings-panel');
  const light = id => testid(id).querySelector('.console-state-light').dataset.state;
  const noBodyErrors = () => assert(!body().querySelector('[role="alert"], [role="status"]'),
    'Update progress and errors belong in the shared footer, never in the Settings body');
  const showDebug = async () => {
    if (debug().hidden) await click(status());
    await wait(() => !debug().hidden);
  };
  const app = source => document.querySelector(`[data-testid="settings-module"][data-module="app"][data-source="${source}"]`);
  const part = (row, name) => row?.querySelector(`[data-testid="settings-module-${name}"]`);
  // Details are the rows following a module's row.
  const detail = (row, name) => {
    for (let next = row?.nextElementSibling; next && next.dataset.testid !== 'settings-module'; next = next.nextElementSibling)
      if (next.dataset.testid === `settings-module-${name}`) return next;
  };
  if (testid('console-settings-toggle').getAttribute('aria-pressed') !== 'true')
    await click(testid('console-settings-toggle'));
  await wait(() => testid('agent-model-input') && !testid('agent-model-input').disabled && app(''),
    () => `Settings did not load: ${text()}`);
  await showDebug();
  assert(status().classList.contains('console-status')
    && status().closest('.console-right-status')
    && !status().closest('[data-testid="settings-panel"]'),
    'Settings must reuse the same bottom status region as other panels');
  assert([...body().querySelectorAll('button:not(.module-settings-link)')].every(button => button.classList.contains('tab-button')),
    'Settings actions must reuse shared panel button styling');
  const bodyBox = body().getBoundingClientRect(), footerBox = status().getBoundingClientRect();
  assert(footerBox.top >= bodyBox.bottom - 1 && footerBox.width > 0,
    'Settings status must be visible below the panel body');

  // The application is the first module: no source yet, the running version, never removable.
  const current = app('');
  assert(current === testid('settings-modules').querySelector('[data-testid="settings-module"]'),
    'The application must lead the Modules table');
  assert(part(current, 'name').textContent === 'Services DevTools'
    && part(current, 'source').textContent === '—'
    && part(current, 'version').textContent === fixtureVersion
    && !part(current, 'new-version') && !part(current, 'install') && !part(current, 'update')
    && part(current, 'remove').disabled,
    `The application row must show its version and refuse removal: ${current.textContent}`);
  await click(part(current, 'name'));
  await wait(() => detail(app(''), 'description')?.textContent.includes("it can't be removed here"),
    'The name must show the description');
  await click(part(app(''), 'version'));
  await wait(() => detail(app(''), 'notes') && !detail(app(''), 'notes').textContent.includes('No details.'),
    () => `The version must show its release notes: ${detail(app(''), 'notes')?.textContent}`);
  await click(part(app(''), 'name'));
  await click(part(app(''), 'version'));
  await wait(() => !detail(app(''), 'description') && !detail(app(''), 'notes'), 'Details must toggle off');

  // A running check animates the shared footer and the panel light.
  nativeHold.match = message => message.service === 'sources' && message.op === 'check';
  try {
    await click(testid('settings-modules-check'));
    await wait(() => status().classList.contains('is-running') && status().querySelector('.console-status-spinner'),
      'A check must show the running spinner');
    assert(testid('settings-modules-check').disabled && light('settings-title') === 'running'
      && debug().textContent.includes('Check sources'),
      () => `Checking must disable repeated clicks, animate the light and show in Debug: ${debug().textContent}`);
  } finally {
    nativeHold.release();
  }
  await wait(() => text() === 'Check sources · OK' && !status().querySelector('.console-status-spinner'),
    () => `The check must finish: ${text()}`);
  noBodyErrors();

  // A folder source offering a newer application.
  await addFolderSource('Updates', fixtureUpdates);
  await wait(() => part(app('updates'), 'new-version')?.textContent === '65000.0.0',
    () => `The source must offer the new version: ${testid('settings-modules').textContent}; ${text()}`);
  const offered = app('updates');
  assert(part(offered, 'source').textContent === 'Updates' && part(offered, 'version').textContent === '—'
    && part(offered, 'install') && !part(offered, 'remove'),
    `Another source's application must be installable: ${offered.textContent}`);
  await click(part(offered, 'new-version'));
  await wait(() => detail(app('updates'), 'new-notes')?.textContent.includes('Fixture notes of 65000.0.0'),
    'The new version must show its release notes');
  assert(updateFixture.installs.length === 0, 'Checking must not install without user consent');

  const install = () => part(app('updates'), 'install');
  const getViews = chrome.extension.getViews;
  const reload = chrome.runtime.reload;
  try {
    chrome.extension.getViews = () => [window, {document}];
    await click(install());
    await wait(() => text().includes('Close other Services DevTools windows'));
    assert(debug().textContent.includes('Close other Services DevTools windows'),
      'A blocked update must report its guard failure in status and Debug');
    assert(updateFixture.installs.length === 0, 'Failed update guards must not send an installation request');
    noBodyErrors();
    chrome.extension.getViews = () => [window];

    updateFixture.installError = 'Fixture installer failed before activation';
    await click(install());
    await wait(() => text().includes(updateFixture.installError));
    assert(debug().textContent.includes(updateFixture.installError) && status().classList.contains('is-error'),
      'Native installation failures must be visible in status and Debug');
    const request = updateFixture.installs.at(-1);
    assert(request.source === 'updates' && request.version === '65000.0.0',
      `The install must name the source and version: ${JSON.stringify(request)}`);
    noBodyErrors();
    updateFixture.installError = null;

    updateFixture.installResult = {reload:true, installedVersion:'65000.0.0'};
    updateFixture.hold = true;
    chrome.runtime.reload = () => { throw new Error('Fixture automatic reload failed'); };
    await click(install());
    await wait(() => updateFixture.deliver, 'The install request must be sent');
    assert(testid('update-progress').textContent.includes('Keep this window open')
      && status().classList.contains('is-running') && light('settings-title') === 'running'
      && debug().textContent.includes('installing'),
      () => `An install must show its progress: ${text()}; ${debug().textContent}`);
    updateFixture.deliver();
    await wait(() => text().includes('automatic reload failed'));
    assert(debug().textContent.includes('Fixture automatic reload failed')
      && status().classList.contains('is-error') && part(app('updates'), 'reload')
      && !document.querySelector('[data-testid="update-progress"]'),
      'Post-install reload errors must remain in status and Debug with a retry action');
    noBodyErrors();
  } finally {
    chrome.extension.getViews = getViews;
    chrome.runtime.reload = reload;
    Object.assign(updateFixture, {installError:null, installResult:null, hold:false, deliver:null});
  }
  return true;
})()
