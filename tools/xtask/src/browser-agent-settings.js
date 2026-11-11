(async () => {
  const {assert, wait, testid, click, editorOf, editorAt, openConsole, openPanel, choose, settle} = testDriver;
  // A fresh panel: wait for the restored Console session before using its toolbar.
  await openConsole();
  const agent = () => openPanel('agent', 'console-agent');
  await agent();
  const selected = () => testid('agent-sessions').querySelector('[role="tab"][aria-selected="true"]').dataset.testid;
  const chatId = key => key.slice('agent-chat-'.length);
  const chats = () => JSON.parse(localStorage.getItem('module:agent:chats') || '{}');
  const originalChat = selected();
  const originalOptions = localStorage.getItem('servicesConsole.agentOptions');
  const created = [];
  const status = () => document.querySelector('[data-testid="console-settings-status"]')?.textContent || '';
  const settings = async () => {
    await testDriver.settingsTab('agent');
    await wait(() => document.querySelector('[data-testid="agent-model-input"]'),
      () => `Model catalog did not load: ${status()}; ${testid('settings-tab-agent-body').textContent}`);
  };
  const model = () => testid('agent-model-input');
  const category = () => document.querySelector('[data-testid="agent-model-input-detail"]')?.textContent;
  const modelOption = id => document.querySelector(`[data-testid="agent-model-input-option-${id}"]`);
  const selectModel = async (id, expected, name) => {
    model().focus();
    await wait(() => modelOption(id), () => `Model ${id} must be listed: ${testid('agent-model-input-options').textContent}`);
    await click(modelOption(id));
    await wait(() => model().value === name && category() === expected,
      () => `Model ${id} must be selected with its category: ${model().value}; ${category()}`);
  };
  const assistants = () => testid('agent-transcript').querySelectorAll('[data-testid^="agent-message-assistant-"]');
  const ask = async prompt => {
    const before = assistants().length;
    const field = testid('agent-composer');
    field.value = prompt;
    field.dispatchEvent(new InputEvent('input', {bubbles:true}));
    field.focus();
    await testDriver.key('Enter');
    await wait(() => assistants().length > before && testid('agent-stop').disabled,
      () => `Turn "${prompt}" did not finish: ${testid('agent-transcript').textContent}; ${document.querySelector('[data-testid="agent-error"]')?.textContent}`);
    return [...assistants()].at(-1).textContent;
  };
  const configuration = async () => {
    const message = await ask('agent model configuration');
    return JSON.parse(message.match(/Model configuration: (\{[^\n]+\})/)?.[1] || '{}');
  };
  const newChat = async () => {
    const before = selected();
    await click(testid('agent-new-session'));
    await wait(() => selected() !== before);
    const id = selected();
    created.push(id);
    return id;
  };
  try {
    await settings();
    assert(model().value.startsWith('Copilot Fusion'),
      `An unconfigured client must default to Copilot Fusion: ${model().value}`);
    assert(category() === 'Automatic', 'Fusion must be identified as the automatic router');
    assert(testid('agent-reasoning').disabled && testid('agent-reasoning-value').textContent === 'Not supported',
      'Fusion must manage Reasoning automatically');
    model().focus();
    await wait(() => modelOption('sample-powerful'), 'The model list must open');
    assert(!modelOption('sample-disabled'), 'Disabled models must not be selectable');
    model().blur();
    assert(localStorage.getItem('servicesConsole.agentOptions') === originalOptions, 'Opening Settings must not save a model');
    await selectModel('sample-powerful', 'Powerful', 'Sample Powerful');
    const reasoning = () => testid('agent-reasoning');
    const levels = () => [...document.querySelectorAll('[data-testid^="agent-reasoning-level-"]')].map(level => level.textContent).join(',');
    const reasoningValue = () => testid('agent-reasoning-value').textContent;
    const stored = () => JSON.parse(localStorage.getItem('servicesConsole.agentOptions'));
    assert(levels() === 'low,high' && reasoning().max === '1',
      `The Reasoning slider must have one step per model level: ${levels()}`);
    assert(reasoningValue() === 'high · default', `A new model must show its default Reasoning: ${reasoningValue()}`);
    reasoning().value = '0';
    reasoning().dispatchEvent(new Event('change', {bubbles:true}));
    await wait(() => stored()?.reasoningEffort === 'low' && reasoningValue() === 'low',
      () => `Reasoning must be saved: ${JSON.stringify(stored())}; ${reasoningValue()}`);
    await wait(() => status().includes('New Agent chat settings'), () => `Settings results belong in the status: ${status()}`);
    await agent();
    const old = await configuration();
    assert(old.model === 'auto' && old.reasoningEffort === null, `Existing chats must not adopt new defaults: ${JSON.stringify(old)}`);
    const powerfulChat = await newChat();
    const powerful = await configuration();
    assert(powerful.model === 'sample-powerful' && powerful.reasoningEffort === 'low',
      `The SDK must receive the selected model and reasoning: ${JSON.stringify(powerful)}`);
    const saved = chats()[chatId(powerfulChat)];
    assert(saved?.options.model === 'sample-powerful' && saved.options.reasoningEffort === 'low',
      'A chat must persist its captured defaults');
    await settings();
    assert(model().value === 'Sample Powerful' && reasoning().value === '0' && reasoningValue() === 'low',
      'Reopening Settings must retain choices');
    await click(testid('agent-reasoning-level-high'));
    await wait(() => stored().model === 'sample-powerful' && stored().reasoningEffort === null
      && reasoning().value === '1' && reasoningValue() === 'high · default',
      () => `Choosing the model default must follow the model again: ${JSON.stringify(stored())}; ${reasoningValue()}`);
    await selectModel('sample-light', 'Lightweight', 'Sample Light');
    assert(reasoning().disabled && !levels() && reasoningValue() === 'Not supported',
      'Models without Reasoning must disable and reset the slider');
    await agent();
    await click(testid('agent-sessions').querySelector(`[data-testid="${powerfulChat}"]`));
    await wait(() => selected() === powerfulChat);
    const retained = await configuration();
    assert(retained.model === 'sample-powerful' && retained.reasoningEffort === 'low',
      'Changing defaults must not change an already-created SDK session');
    await newChat();
    const light = await configuration();
    assert(light.model === 'sample-light' && light.reasoningEffort === null,
      'New chats must capture the latest model without unsupported Reasoning');

    await testDriver.settingsTab('sources');
    assert(!document.querySelector('[data-testid^="source-name-"]'), 'The scenario starts without sources');
    await testDriver.addFolderSource('Team', fixtureSource);
    await testDriver.settingsTab('sources');
    await wait(() => document.querySelector('[data-testid="source-apply-team"]'),
      () => `The added source must be checked and offer its settings: ${testid('settings-tab-sources-body').textContent}; ${status()}`);
    await click(await wait(() => document.querySelector('button[data-testid="source-status-team"]'), 'The checked status opens what changed'), 4);
    const changes = (await wait(() => document.querySelector('[data-testid="source-changes-team"]'), 'The status must show what changed')).textContent;
    assert(changes.includes('skill team-shop-deploy added') && !changes.includes('dictionary'),
      `The check must list the skills applying changes: ${changes}`);
    await click(testid('source-apply-team'));
    await wait(() => !document.querySelector('[data-testid="source-apply-team"]')
      && !testid('source-status-team').textContent.includes('skills'),
      () => `Applying must bring the source's skills: ${document.body.innerText.slice(0,600)}`);
    await agent();
    await newChat();
    const used = await ask('agent skills');
    assert(/skills: .*team-shop-deploy/.test(used) && used.includes('sources/team/skills'),
      `The next turn must hand the source's skill to Copilot: ${used}`);
    assert(document.querySelector('[data-testid="console-agent-status-debug"]')?.textContent.includes('team-shop-deploy'),
      'The Agent status debug must list the skills Copilot used');

    await testDriver.settingsTab('sources');
    await click(await wait(() => document.querySelector('[data-testid="source-sync-team"]'),
      () => `The source's Dictionary must wait to be synced: ${document.body.innerText.slice(0, 600)}`));
    // Local has its own Dictionary and the new source no cache: the difference is a conflict; Done takes the source's.
    await click(await wait(() => document.querySelector('[data-testid="diff-done"]'), 'The Dictionary conflict must open in Diff'));
    await testDriver.settingsTab('sources');
    await wait(() => document.querySelector('[data-testid="source-status-team"]')?.textContent === 'In sync',
      () => `Syncing must bring the source's settings to Local: ${document.body.innerText.slice(0,700)}`);
    await openPanel('dictionary');
    const set = await wait(() => document.querySelector('[data-testid="dictionary-set-team"]'),
      () => `The synced Dictionary set must be listed: ${testid('dictionary-tabs').textContent}`);
    await click(set);
    await wait(() => editorAt('console-dictionary')?.getValue().includes('team.fixture.invalid'),
      'The synced set must hold the source values');
    assert(!document.querySelector('[data-testid="dictionary-reset"]') && !document.querySelector('[data-testid="dictionary-origin"]'),
      'Dictionary has no source layers any more');

    await testDriver.settingsTab('sources');
    await click(testid('source-remove-team'));
    await click(await wait(() => document.querySelector('[data-testid="source-remove-confirm-team"]')));
    await wait(() => !document.querySelector('[data-testid="source-team"]'), 'Removing the source must remove its row');
    await agent();
    const cleared = await ask('agent skills');
    assert(!cleared.includes('team-shop-deploy'), `A removed source must stop reaching Copilot: ${cleared}`);
  } finally {
    if (originalOptions === null) localStorage.removeItem('servicesConsole.agentOptions');
    else localStorage.setItem('servicesConsole.agentOptions', originalOptions);
    await agent();
    for (const id of created) {
      const tab = testid('agent-sessions').querySelector(`[data-testid="${id}"]`);
      if (!tab) continue;
      await click(tab);
      await wait(() => selected() === id);
      await click(document.querySelector('[data-testid="agent-sessions"] [aria-selected="true"] [data-testid="agent-session-close"]'));
      await wait(() => !testid('agent-sessions').querySelector(`[data-testid="${id}"]`));
    }
    const original = testid('agent-sessions').querySelector(`[data-testid="${originalChat}"]`);
    if (original) await click(original);
    await settle();
  }
  globalThis.agentSettingsReceipt = {categories:3,reasoning:['low','high'],newChats:created.length,existingChatPreserved:true,sourceSkills:['team-shop-deploy'],dictionaryReset:true};
  return true;
})()
