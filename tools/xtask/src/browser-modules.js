(async () => {
  const {assert, wait, testid, click, openConsole, editorOf, key, setSource, addFolderSource} = testDriver;
  await openConsole();
  // The service modules and the panel modules (Settings is built into the App).
  const shipped = ['agent', 'artemis', 'dictionary', 'graphql', 'grpc', 'hints', 'history', 'mysql', 'opensearch', 'rest',
    'sessions', 'splunk', 'templates', 'terminal', 'variables'];
  const status = () => document.querySelector('[data-testid="console-settings-status"] .status-bar-text')?.textContent ?? '';
  // A module row is keyed `settings-module-{source}/{module}`; its parts `settings-module-{part}-{source}/{module}`.
  const rows = () => [...document.querySelectorAll('[data-testid="settings-modules"] tbody > tr')];
  const keyOf = row => row.dataset.testid.slice('settings-module-'.length);
  const moduleOf = row => keyOf(row).split('/')[1];
  const part = (row, name) => row && document.querySelector(`[data-testid="settings-module-${name}-${keyOf(row)}"]`);
  // The modules installed, besides the application.
  const listed = () => rows().filter(row => moduleOf(row) !== 'app' && part(row, 'remove'))
    .map(moduleOf).sort().join();
  const row = (id, source = '') => rows().find(row => keyOf(row) === `${source}/${id}`);
  const detail = part;
  const tab = id => document.querySelector(`[data-testid="console-service-${id}"]`);
  const settings = async open => {
    if (open) return testDriver.settingsTab('modules');
    if (testid('console-settings-toggle').getAttribute('aria-pressed') === 'true')
      await click(testid('console-settings-toggle'));
  };
  const kept = '#check kept\n#endpoint\nhttps://kept.fixture.invalid\n';
  await click(tab('terminal'));
  setSource(editorOf('console-editor'), kept);
  await settings(true);
  await wait(() => listed() === shipped.join() && !status().includes('Running'),
    () => `Settings must list every shipped module: ${listed()}; ${status()}`);
  for (const id of shipped) {
    const shippedRow = row(id);
    assert(part(shippedRow, 'source').textContent === '—' && /^\d+\.\d+\.\d+$/.test(part(shippedRow, 'version').textContent)
      && part(shippedRow, 'name').textContent && !part(shippedRow, 'new-version'),
      `${id}: name, no source and its version expected, got ${shippedRow.textContent}`);
  }
  const columns = () => [...row('rest').cells].map(cell => Math.round(cell.getBoundingClientRect().left)).join();
  const closedColumns = columns();
  await click(part(row('rest'), 'name'));
  await wait(() => detail(row('rest'), 'description') && !detail(row('rest'), 'description').textContent.includes('No details.'),
    'The name must show the module description');
  // A long, multi-line description wraps inside the table instead of resizing its columns.
  editorOf('settings-module-description-/rest').setValue(
    `${'A long module description line. '.repeat(12)}\nhttps://github.com/example/${'long-path-'.repeat(30)}`);
  const table = testid('settings-modules');
  assert(columns() === closedColumns && table.getBoundingClientRect().width <= table.parentElement.clientWidth,
    `Opening a description must not move the columns: ${closedColumns} -> ${columns()}; table ${table.getBoundingClientRect().width} of ${table.parentElement.clientWidth}`);
  await click(part(row('rest'), 'name'));

  const confirmation = () => document.querySelector('[data-testid="settings-module-confirm"]');
  const confirmPart = (id, name) => confirmation()?.querySelector(`[data-testid="settings-module-${name}"]`);
  const forget = () => confirmPart('', 'forget')?.querySelector('input');
  const ask = async (id, source) => {
    await click(part(row(id, source), 'remove'));
    await wait(() => confirmPart(id, 'remove-confirm', source) && part(row(id, source), 'remove').disabled,
      () => `Removal of ${id} must ask first`);
  };
  await ask('terminal');
  const question = confirmation().querySelector('.ui-text').textContent;
  assert(question.includes('Remove module "Terminal"') && question.includes('kept') && !forget().checked,
    `Removal must say consoles are kept and keep them by default: ${question}`);
  await click(confirmPart('terminal', 'remove-cancel'));
  await wait(() => !confirmation() && !part(row('terminal'), 'remove').disabled, 'Cancel must close the confirmation');
  await testDriver.sleep(300);
  assert(row('terminal') && tab('terminal') && !status().includes('Remove'), 'A cancelled removal must change nothing');
  await ask('terminal');
  await click(confirmPart('terminal', 'remove-confirm'));
  await wait(() => status() === 'Removed terminal' && !row('terminal') && !tab('terminal') && !part(row('rest'), 'remove').disabled,
    () => `Removal must restart the host without the module: ${status()}; ${listed()}`);
  assert(listed() === shipped.filter(id => id !== 'terminal').join() && tab('rest'), 'Other modules must stay');

  // A folder source offering modules; its invalid file is a problem of the source, not a module.
  await addFolderSource('Modules', fixtureModules);
  await testDriver.settingsTab('sources');
  const problems = await wait(() => document.querySelector('[data-testid="source-problems-modules"]'),
    () => `The folder source must be checked: ${testid('source-settings').textContent}`);
  assert(problems.textContent.includes('broken.module.tgz'),
    () => `An invalid package file must be a source problem: ${problems.textContent}`);
  await testDriver.settingsTab('modules');
  await wait(() => part(row('terminal', 'modules'), 'install') && part(row('echo', 'modules'), 'install'),
    () => `The source must offer its modules: ${testid('settings-modules').textContent}; ${status()}`);
  assert(!rows().some(row => moduleOf(row) === 'broken'), 'An invalid package file must not be a module');
  const offered = row('terminal', 'modules');
  assert(part(offered, 'source').textContent === 'Modules'
    && part(offered, 'version').textContent === '—' && !part(offered, 'remove'),
    `A module offered but not installed has no version: ${offered.textContent}`);
  await click(part(offered, 'new-version'));
  await wait(() => detail(row('terminal', 'modules'), 'new-notes'), 'The new version must show its release notes');
  await click(part(row('terminal', 'modules'), 'install'));
  await wait(() => /^Installed Terminal \d+\.\d+\.\d+$/.test(status()) && tab('terminal'),
    () => `The source must install its module: ${status()}; ${listed()}`);
  const version = status().split(' ').pop();
  await wait(() => part(row('terminal', 'modules'), 'version')?.textContent === version
    && !part(row('terminal', 'modules'), 'remove').disabled && !part(row('terminal', 'modules'), 'install'),
    () => `The installed module must show its version from the source: ${row('terminal', 'modules')?.textContent}`);
  assert(listed() === shipped.join() && !row('terminal'), `Its row must now be the source's: ${listed()}`);
  await settings(false);
  await click(tab('terminal'));
  const editor = editorOf('console-editor');
  assert(editor.getValue() === kept, `The console of a removed module must come back: ${JSON.stringify(editor.getValue())}`);
  setSource(editor, '#check installed\n#endpoint\nbash -c\n#command\necho "{{var:sku}}"');
  await key('Shift+Enter');
  await wait(() => editor.getValue().includes('#responce') && editor.getValue().includes('fixture-materialized-sku'),
    () => `The installed module must run requests: ${editor.getValue().slice(-500)}`);

  // A module the distribution does not ship: the SDK example.
  await settings(true);
  const echoInstalled = () => status() === 'Installed Echo 0.1.0' && tab('echo')
    && part(row('echo', 'modules'), 'version')?.textContent === '0.1.0' && !part(row('echo', 'modules'), 'remove').disabled;
  await click(part(row('echo', 'modules'), 'install'));
  await wait(echoInstalled, () => `The example module must install from the source: ${status()}; ${listed()}`);
  await settings(false);
  await click(tab('echo'));
  const echo = editorOf('console-editor');
  setSource(echo, '#check echo\n#endpoint\nlocal\n#text\nsent through a module\n');
  await key('Shift+Enter');
  const echoed = () => echo.getValue().split('#responce')[1] ?? '';
  await wait(() => echoed().includes('sent through a module'),
    () => `The example module must answer with its text: ${echo.getValue().slice(-500)}`);
  await settings(true);
  await ask('echo', 'modules');
  await click(forget());
  await wait(() => forget().checked, 'The data checkbox must toggle');
  await click(confirmPart('echo', 'remove-confirm'));
  await wait(() => status() === 'Removed echo and its data' && !tab('echo') && part(row('echo', 'modules'), 'install'),
    () => `The example module must be removable with its data and stay offered: ${status()}; ${listed()}`);
  const leftovers = async () => (await testDriver.sessionLibrary()).sessions.filter(session =>
    ['documents', 'responses', 'checkIdentities'].some(key => session.snapshot.console?.[key]?.echo !== undefined)
    || (session.snapshot.console?.order ?? []).includes('echo'));
  let left = [];
  await wait(async () => (left = await leftovers()).length === 0,
    () => `No session may keep the console of a module removed with its data: ${left.map(s => s.id)}`);
  await click(part(row('echo', 'modules'), 'install'));
  await wait(echoInstalled, () => `The example module must install again: ${status()}; ${listed()}`);
  await settings(false);
  await click(tab('echo'));
  assert(!editorOf('console-editor').getValue().includes('sent through a module'),
    `The deleted console must not come back: ${editorOf('console-editor').getValue().slice(0, 300)}`);
  await settings(true);
  await ask('echo', 'modules');
  await click(confirmPart('echo', 'remove-confirm'));
  await wait(() => status() === 'Removed echo' && !tab('echo') && part(row('echo', 'modules'), 'install'),
    () => `The example module must be removable: ${status()}; ${listed()}`);
  assert(listed() === shipped.join(), `Only the shipped modules must stay: ${listed()}`);
  return true;
})()
