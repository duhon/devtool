(async () => {
  const {assert, wait, testid, click, openConsole, cm, key, setSource, addFolderSource} = testDriver;
  await openConsole();
  const shipped = ['artemis', 'graphql', 'grpc', 'mysql', 'opensearch', 'rest', 'splunk', 'terminal'];
  const status = () => document.querySelector('[data-testid="settings-update-status"] .console-status-text')?.textContent ?? '';
  const rows = () => [...document.querySelectorAll('[data-testid="settings-module"]')];
  const part = (row, name) => row?.querySelector(`[data-testid="settings-module-${name}"]`);
  // The modules installed, besides the application.
  const listed = () => rows().filter(row => row.dataset.module !== 'app' && part(row, 'remove'))
    .map(row => row.dataset.module).sort().join();
  const row = (id, source = '') => rows().find(row => row.dataset.module === id && row.dataset.source === source);
  // Details are the rows following a module's row.
  const detail = (row, name) => {
    const testid = `settings-module-${name}`;
    for (let next = row?.nextElementSibling; next && next.dataset.testid !== 'settings-module'; next = next.nextElementSibling) {
      const found = next.dataset.testid === testid ? next : next.querySelector(`[data-testid="${testid}"]`);
      if (found) return found;
    }
  };
  const tab = id => document.querySelector(`[data-testid="console-service-${id}"]`);
  const settings = async open => {
    if (open) return testDriver.settingsTab('modules');
    if (testid('console-settings-toggle').getAttribute('aria-pressed') === 'true')
      await click(testid('console-settings-toggle'));
  };
  const kept = '#check kept\n#endpoint\nhttps://kept.fixture.invalid\n';
  await click(tab('graphql'));
  setSource(cm('console-editor'), kept);
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
  detail(row('rest'), 'description').querySelector('pre').textContent =
    `${'A long module description line. '.repeat(12)}\nhttps://github.com/example/${'long-path-'.repeat(30)}`;
  const table = testid('settings-modules');
  assert(columns() === closedColumns && table.getBoundingClientRect().width <= table.parentElement.clientWidth,
    `Opening a description must not move the columns: ${closedColumns} -> ${columns()}; table ${table.getBoundingClientRect().width} of ${table.parentElement.clientWidth}`);
  await click(part(row('rest'), 'name'));

  const confirmation = (id, source) => detail(row(id, source), 'confirm');
  const confirmPart = (id, name, source) => confirmation(id, source)?.querySelector(`[data-testid="settings-module-${name}"]`);
  const ask = async (id, source) => {
    await click(part(row(id, source), 'remove'));
    await wait(() => confirmPart(id, 'remove-confirm', source) && part(row(id, source), 'remove').disabled,
      () => `Removal of ${id} must ask first`);
  };
  await ask('graphql');
  const question = confirmation('graphql').querySelector('p').textContent;
  assert(question.includes('Remove module "GraphQL"') && question.includes('kept') && !confirmPart('graphql', 'forget').checked,
    `Removal must say consoles are kept and keep them by default: ${question}`);
  await click(confirmPart('graphql', 'remove-cancel'));
  await wait(() => !confirmation('graphql') && !part(row('graphql'), 'remove').disabled, 'Cancel must close the confirmation');
  await testDriver.sleep(300);
  assert(row('graphql') && tab('graphql') && !status().includes('Remove'), 'A cancelled removal must change nothing');
  await ask('graphql');
  await click(confirmPart('graphql', 'remove-confirm'));
  await wait(() => status() === 'Removed graphql' && !row('graphql') && !tab('graphql') && !part(row('rest'), 'remove').disabled,
    () => `Removal must restart the host without the module: ${status()}; ${listed()}`);
  assert(listed() === shipped.filter(id => id !== 'graphql').join() && tab('rest'), 'Other modules must stay');

  // A folder source offering modules; its invalid file is a problem of the source, not a module.
  await addFolderSource('Modules', fixtureModules);
  const source = () => document.querySelector('[data-testid="source"][data-source="modules"]');
  await wait(() => part(row('graphql', 'modules'), 'install') && part(row('echo', 'modules'), 'install'),
    () => `The source must offer its modules: ${testid('settings-modules').textContent}; ${status()}`);
  assert(source().querySelector('[data-testid="source-problems"]')?.textContent.includes('broken.module.tgz')
    && !rows().some(row => row.dataset.module === 'broken'),
    () => `An invalid package file must be a source problem: ${source().textContent}`);
  const offered = row('graphql', 'modules');
  assert(part(offered, 'source').textContent === 'Modules' && part(offered, 'source').title === fixtureModules
    && part(offered, 'version').textContent === '—' && !part(offered, 'remove'),
    `A module offered but not installed has no version: ${offered.textContent}`);
  await click(part(offered, 'new-version'));
  await wait(() => detail(row('graphql', 'modules'), 'new-notes'), 'The new version must show its release notes');
  await click(part(row('graphql', 'modules'), 'install'));
  await wait(() => /^Installed GraphQL \d+\.\d+\.\d+$/.test(status()) && tab('graphql'),
    () => `The source must install its module: ${status()}; ${listed()}`);
  const version = status().split(' ').pop();
  await wait(() => part(row('graphql', 'modules'), 'version')?.textContent === version
    && !part(row('graphql', 'modules'), 'remove').disabled && !part(row('graphql', 'modules'), 'install'),
    () => `The installed module must show its version from the source: ${row('graphql', 'modules')?.textContent}`);
  assert(listed() === shipped.join() && !row('graphql'), `Its row must now be the source's: ${listed()}`);
  await settings(false);
  await click(tab('graphql'));
  const editor = cm('console-editor');
  assert(editor.getValue() === kept, `The console of a removed module must come back: ${JSON.stringify(editor.getValue())}`);
  setSource(editor, `#check installed\n#endpoint\n${fixtureUrl}/graphql-materialization\n#variables\n{"skus":["{{var:sku}}"]}\n`
    + '#query\nquery ($skus: [String]) { products(skus: $skus) { sku images { url } } }');
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
  const echo = cm('console-editor');
  setSource(echo, '#check echo\n#endpoint\nlocal\n#text\nsent through a module\n');
  await key('Shift+Enter');
  const echoed = () => echo.getValue().split('#responce')[1] ?? '';
  await wait(() => echoed().includes('sent through a module'),
    () => `The example module must answer with its text: ${echo.getValue().slice(-500)}`);
  await settings(true);
  const drafts = 'servicesConsole.templateDrafts';
  const draftKeys = () => Object.keys(JSON.parse(localStorage.getItem(drafts) ?? '{}'));
  localStorage.setItem(drafts, JSON.stringify({...JSON.parse(localStorage.getItem(drafts) ?? '{}'),
    'echo/text': {text: 'draft of a removed module', base: ''}}));
  await ask('echo', 'modules');
  await click(confirmPart('echo', 'forget', 'modules'));
  await wait(() => confirmPart('echo', 'forget', 'modules').checked, 'The data checkbox must toggle');
  await click(confirmPart('echo', 'remove-confirm', 'modules'));
  await wait(() => status() === 'Removed echo and its data' && !tab('echo') && part(row('echo', 'modules'), 'install'),
    () => `The example module must be removable with its data and stay offered: ${status()}; ${listed()}`);
  const leftovers = async () => (await testDriver.sessionLibrary()).sessions.filter(session =>
    ['documents', 'responses', 'checkIdentities'].some(key => session.snapshot.console?.[key]?.echo !== undefined)
    || (session.snapshot.console?.order ?? []).includes('echo') || session.snapshot.hints?.echo !== undefined);
  let left = [];
  await wait(async () => (left = await leftovers()).length === 0,
    () => `No session may keep the console of a module removed with its data: ${left.map(s => s.id)}`);
  assert(!draftKeys().some(key => key.startsWith('echo/')), `Its Templates drafts must be deleted: ${draftKeys()}`);
  await click(part(row('echo', 'modules'), 'install'));
  await wait(echoInstalled, () => `The example module must install again: ${status()}; ${listed()}`);
  await settings(false);
  await click(tab('echo'));
  assert(!cm('console-editor').getValue().includes('sent through a module'),
    `The deleted console must not come back: ${cm('console-editor').getValue().slice(0, 300)}`);
  await settings(true);
  await ask('echo', 'modules');
  await click(confirmPart('echo', 'remove-confirm', 'modules'));
  await wait(() => status() === 'Removed echo' && !tab('echo') && part(row('echo', 'modules'), 'install'),
    () => `The example module must be removable: ${status()}; ${listed()}`);
  assert(listed() === shipped.join(), `Only the shipped modules must stay: ${listed()}`);
  return true;
})()
