(async () => {
  const {assert, wait, testid, click, openConsole, input, cm, key, setSource} = testDriver;
  await openConsole();
  const shipped = ['artemis', 'graphql', 'grpc', 'mysql', 'rest', 'splunk', 'terminal'];
  const status = () => document.querySelector('[data-testid="settings-update-status"] .console-status-text')?.textContent ?? '';
  const rows = () => [...document.querySelectorAll('[data-testid="settings-module"]')];
  const listed = () => rows().map(row => row.dataset.module).sort().join();
  const row = id => rows().find(row => row.dataset.module === id);
  const remove = id => row(id).querySelector('[data-testid="settings-module-remove"]');
  const tab = id => document.querySelector(`[data-testid="console-service-${id}"]`);
  const choose = file => input('Test.chooseFile', {selector: '[data-testid="settings-module-file"]', file});
  const settings = async open => {
    if ((testid('console-settings-toggle').getAttribute('aria-pressed') === 'true') !== open)
      await click(testid('console-settings-toggle'));
  };
  const kept = '#check kept\n#endpoint\nhttps://kept.fixture.invalid\n';
  await click(tab('graphql'));
  setSource(cm('console-editor'), kept);
  await settings(true);
  await wait(() => listed() === shipped.join() && !status().includes('Checking'),
    () => `Settings must list every shipped module: ${listed()}; ${status()}`);
  for (const id of shipped) {
    const detail = row(id).querySelector('.module-settings-detail').textContent;
    assert(/^\S+ \d+\.\d+\.\d+$/.test(detail) && detail.startsWith(`${id} `), `${id}: id and version expected, got ${detail}`);
  }
  const confirmation = id => row(id)?.querySelector('[data-testid="settings-module-confirm"]');
  const part = (id, name) => confirmation(id)?.querySelector(`[data-testid="settings-module-${name}"]`);
  const ask = async id => {
    await click(remove(id));
    await wait(() => part(id, 'remove-confirm') && remove(id).disabled, () => `Removal of ${id} must ask first`);
  };
  await ask('graphql');
  const question = confirmation('graphql').querySelector('p').textContent;
  assert(question.includes('Remove module "GraphQL"') && question.includes('kept') && !part('graphql', 'forget').checked,
    `Removal must say consoles are kept and keep them by default: ${question}`);
  await click(part('graphql', 'remove-cancel'));
  await wait(() => !confirmation('graphql') && !remove('graphql').disabled, 'Cancel must close the confirmation');
  await testDriver.sleep(300);
  assert(row('graphql') && tab('graphql') && !status().includes('Remove'), 'A cancelled removal must change nothing');
  await ask('graphql');
  await click(part('graphql', 'remove-confirm'));
  await wait(() => status() === 'Removed graphql' && !row('graphql') && !tab('graphql') && !remove('rest').disabled,
    () => `Removal must restart the host without the module: ${status()}; ${listed()}`);
  assert(listed() === shipped.filter(id => id !== 'graphql').join() && tab('rest'), 'Other modules must stay');
  await choose('broken');
  await wait(() => status().startsWith('Install module · ERROR · Invalid package file'),
    () => `An invalid package file must be reported: ${status()}`);
  assert(!row('graphql') && testid('settings-update-status').classList.contains('is-error'),
    'A failed install must change nothing and show an error');
  await choose('graphql');
  await wait(() => /^Installed GraphQL \d+\.\d+\.\d+$/.test(status()) && row('graphql') && tab('graphql'),
    () => `A package file must install its module: ${status()}; ${listed()}`);
  const version = status().split(' ').pop();
  await wait(() => !remove('graphql').disabled);
  await choose('graphql');
  await wait(() => status() === `Installed GraphQL ${version} (was ${version})`,
    () => `Reinstalling must replace the module: ${status()}`);
  await wait(() => listed() === shipped.join() && !remove('graphql').disabled);
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
  await choose('echo');
  await wait(() => status() === 'Installed Echo 0.1.0' && row('echo') && tab('echo') && !remove('echo').disabled,
    () => `The example module must install from its package file: ${status()}; ${listed()}`);
  assert(row('echo').querySelector('.module-settings-detail').textContent === 'echo 0.1.0', 'The example module must list its id and version');
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
  await ask('echo');
  await click(part('echo', 'forget'));
  await wait(() => part('echo', 'forget').checked, 'The data checkbox must toggle');
  await click(part('echo', 'remove-confirm'));
  await wait(() => status() === 'Removed echo and its data' && !row('echo') && !tab('echo'),
    () => `The example module must be removable with its data: ${status()}; ${listed()}`);
  const leftovers = async () => (await testDriver.sessionLibrary()).sessions.filter(session =>
    ['documents', 'responses', 'checkIdentities'].some(key => session.snapshot.console?.[key]?.echo !== undefined)
    || (session.snapshot.console?.order ?? []).includes('echo') || session.snapshot.hints?.echo !== undefined);
  let left = [];
  await wait(async () => (left = await leftovers()).length === 0,
    () => `No session may keep the console of a module removed with its data: ${left.map(s => s.id)}`);
  assert(!draftKeys().some(key => key.startsWith('echo/')), `Its Templates drafts must be deleted: ${draftKeys()}`);
  await choose('echo');
  await wait(() => status() === 'Installed Echo 0.1.0' && row('echo') && tab('echo') && !remove('echo').disabled,
    () => `The example module must install again: ${status()}; ${listed()}`);
  await settings(false);
  await click(tab('echo'));
  assert(!cm('console-editor').getValue().includes('sent through a module'),
    `The deleted console must not come back: ${cm('console-editor').getValue().slice(0, 300)}`);
  await settings(true);
  await ask('echo');
  await click(part('echo', 'remove-confirm'));
  await wait(() => status() === 'Removed echo' && !row('echo') && !tab('echo'),
    () => `The example module must be removable: ${status()}; ${listed()}`);
  assert(listed() === shipped.join(), `Only the shipped modules must stay: ${listed()}`);
  return true;
})()
