(async () => {
  const {assert, wait, testid, click, openConsole, input, cm, key, setSource} = testDriver;
  await openConsole();
  const shipped = ['graphql', 'grpc', 'mysql', 'rest', 'splunk', 'terminal'];
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
  const confirm = window.confirm;
  const asked = [];
  let answer = false;
  window.confirm = question => { asked.push(question); return answer; };
  try {
    await click(remove('graphql'));
    assert(asked.length === 1 && asked[0].includes('Remove module "GraphQL"') && asked[0].includes('kept'),
      `Removal must ask first and say consoles are kept: ${JSON.stringify(asked)}`);
    await testDriver.sleep(300);
    assert(row('graphql') && tab('graphql') && !status().includes('Remove'), 'A cancelled removal must change nothing');
    answer = true;
    await click(remove('graphql'));
    await wait(() => status() === 'Removed graphql' && !row('graphql') && !tab('graphql') && !remove('rest').disabled,
      () => `Removal must restart the host without the module: ${status()}; ${listed()}`);
    assert(listed() === shipped.filter(id => id !== 'graphql').join() && tab('rest'), 'Other modules must stay');
  } finally {
    window.confirm = confirm;
  }
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
  window.confirm = () => true;
  try {
    await click(remove('echo'));
    await wait(() => status() === 'Removed echo' && !row('echo') && !tab('echo'),
      () => `The example module must be removable: ${status()}; ${listed()}`);
  } finally {
    window.confirm = confirm;
  }
  assert(listed() === shipped.join(), `Only the shipped modules must stay: ${listed()}`);
  return true;
})()
