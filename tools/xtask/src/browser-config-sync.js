(async () => {
  const {assert, wait, click, testid, editorAt, openPanel, settingsTab, addFolderSource, visible} = testDriver;
  const file = async (name, text) => {
    const url = `${fixtureUrl}/sync-file?name=${name}`;
    if (text === undefined) return (await fetch(url)).text();
    await fetch(url, {method: 'POST', body: text});
  };
  const sources = () => document.querySelector('[data-testid="source-sync"]')?.textContent ?? '';
  const state = () => document.querySelector('[data-testid="source-sync-state-sync"]')?.textContent ?? '';
  const check = async () => {
    await settingsTab('sources');
    await wait(() => !testid('sources-check').disabled);
    await click(testid('sources-check'));
    await wait(() => !testid('sources-check').disabled);
  };
  // Saving is asynchronous: checks repeat until the edit has reached Local.
  const checkUntil = async (expected, message) => {
    for (let attempt = 0; attempt < 10; attempt++) {
      await check();
      if (state().includes(expected)) return;
      await testDriver.sleep(300);
    }
    assert(false, `${message}: ${state()} | ${sources()} | ${document.querySelector('[data-testid="settings-tab-sources-body"]')?.textContent}`);
  };
  const dictionary = () => editorAt('console-dictionary');
  const showSet = async () => {
    await openPanel('dictionary');
    await click(await wait(() => document.querySelector('[data-testid="dictionary-set-sync"]'), 'The synced set'));
    await wait(() => dictionary()?.getValue().includes('HOST'), 'The set text');
  };
  const edit = async (from, to) => {
    await showSet();
    dictionary().setValue(dictionary().getValue().replace(from, to));
    await wait(() => dictionary().getValue().includes(to));
    await wait(() => [...document.querySelectorAll('.console-right-status .status-bar')].find(visible)?.textContent.includes('Dictionary save · OK'), 'Dictionary saved');
    await testDriver.sleep(1500);
  };
  const synced = () => wait(() => document.querySelector('[data-testid="source-in-sync-sync"]'),
    () => `Sync must end in sync: ${sources()} ${state()}`);

  await testDriver.openConsole();
  await addFolderSource('Sync', fixtureSync);
  await settingsTab('sources');
  assert(testid('source-local-name').textContent === 'Local' && !document.querySelector('[data-testid="source-remove-local"]'),
    'Local is always listed and cannot be removed');
  await click(await wait(() => document.querySelector('[data-testid="source-sync-sync"]'),
    () => `A new source waits to be synced: ${testid('source-sync-row-sync')?.textContent}`));
  // Local already has a Dictionary and a new source has no cache: the difference is a conflict.
  await click(await wait(() => document.querySelector('[data-testid="diff-done"]'), 'A new source conflicts with the Local file'));
  await settingsTab('sources');
  await synced();
  await showSet();
  assert(dictionary().getValue().includes('a.invalid'), 'Done took the source text for Local');

  // Local edit: pushed to the folder.
  await edit('a.invalid', 'local.invalid');
  await checkUntil('Changes in Local', 'Local change expected');
  await click(testid('source-sync-sync'));
  await synced();
  await wait(async () => (await file('dictionary.json')).includes('local.invalid'), 'The folder must get the Local edit');

  // Source edit: pulled.
  const pushed = JSON.parse(await file('dictionary.json'));
  pushed.sets.sync.PORT = '2';
  await file('dictionary.json', JSON.stringify(pushed));
  await checkUntil('not in Local', 'Source change expected');
  await click(testid('source-sync-sync'));
  await synced();
  await showSet();
  await wait(() => dictionary().getValue().includes('PORT: 2'), () => `Local must get the source edit: ${dictionary().getValue()}`);

  // The same variable changed on both sides: a conflict settled in Diff.
  await edit('local.invalid', 'l2.invalid');
  const theirs = JSON.parse(await file('dictionary.json'));
  theirs.sets.sync.HOST = 's2.invalid';
  await file('dictionary.json', JSON.stringify(theirs));
  await checkUntil('Conflicts', 'Conflict expected');
  assert(testid('source-sync-sync').textContent === 'Resolve conflicts…', 'The button offers to resolve');
  await click(testid('source-sync-sync'));
  await wait(() => document.querySelector('[data-testid="diff-done"]'), 'The conflict must open in Diff');
  const left = testDriver.editorOf('diff-left').getValue();
  const right = testDriver.editorOf('diff-right');
  assert(left.includes('l2.invalid') && right.getValue().includes('s2.invalid'), `Local left, source right: ${left}`);
  right.setValue(right.getValue().replace('s2.invalid', 'm.invalid'));
  await click(testid('diff-done'));
  await wait(async () => (await file('dictionary.json')).includes('m.invalid'), 'Done must write the folder');
  await openPanel('dictionary');
  await showSet();
  await wait(() => dictionary().getValue().includes('m.invalid'), 'Done must write Local');
  await settingsTab('sources');
  await synced();
  await click(testid('source-remove-sync'));
  await click(await wait(() => document.querySelector('[data-testid="source-remove-confirm-sync"]')));
  return true;
})();
