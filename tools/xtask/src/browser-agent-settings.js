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
  const selectModel = async (id, category) => {
    choose(model(), id);
    await wait(() => model().value === id && testid('agent-model-category').textContent === category,
      () => `Model ${id} must be selected with its category: ${model().value}; ${document.querySelector('[data-testid="agent-model-category"]')?.textContent}`);
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
    assert(model().value === 'auto' && model().selectedOptions[0].textContent.startsWith('Copilot Fusion'),
      `An unconfigured client must default to Copilot Fusion: ${model().value}`);
    assert(testid('agent-model-category').textContent === 'Automatic', 'Fusion must be identified as the automatic router');
    assert(!document.querySelector('[data-testid="agent-reasoning"]') && testid('agent-reasoning-value').textContent === 'Not supported',
      'Fusion must manage Reasoning automatically');
    assert(![...model().options].some(option => option.value === 'sample-disabled'), 'Disabled models must not be selectable');
    assert(localStorage.getItem('servicesConsole.agentOptions') === originalOptions, 'Opening Settings must not save a model');
    await selectModel('sample-powerful', 'Powerful');
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
    assert(model().value === 'sample-powerful' && reasoning().value === '0' && reasoningValue() === 'low',
      'Reopening Settings must retain choices');
    await click(testid('agent-reasoning-level-high'));
    await wait(() => stored().model === 'sample-powerful' && stored().reasoningEffort === null
      && reasoning().value === '1' && reasoningValue() === 'high · default',
      () => `Choosing the model default must follow the model again: ${JSON.stringify(stored())}; ${reasoningValue()}`);
    await selectModel('sample-light', 'Lightweight');
    assert(!document.querySelector('[data-testid="agent-reasoning"]') && !levels() && reasoningValue() === 'Not supported',
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
    const changes = testid('source-changes-team').textContent;
    assert(changes.includes('skill team-shop-deploy added') && changes.includes('dictionary team/HOST added'),
      `The check must list what applying changes: ${changes}`);
    await click(testid('source-apply-team'));
    await wait(() => document.querySelector('[data-testid="source-up-to-date-team"]'),
      () => `Applying must bring the source's settings: ${testid('source-team').textContent}`);
    await agent();
    await newChat();
    const used = await ask('agent skills');
    assert(/skills: .*team-shop-deploy/.test(used) && used.includes('sources/team/skills'),
      `The next turn must hand the source's skill to Copilot: ${used}`);
    assert(document.querySelector('[data-testid="console-agent-status-debug"]')?.textContent.includes('team-shop-deploy'),
      'The Agent status debug must list the skills Copilot used');

    await openPanel('dictionary');
    const profile = await wait(() => document.querySelector('[data-testid="dictionary-set-team"]'),
      () => `The source's Dictionary set must be listed: ${testid('dictionary-tabs').textContent}`);
    await click(profile);
    const dictionary = () => editorAt('console-dictionary');
    await wait(() => document.querySelector('[data-testid="dictionary-origin-label"]')?.textContent === 'From source Team'
      && dictionary()?.getValue().includes('team.fixture.invalid'),
      () => `A source set must name its source: ${document.querySelector('[data-testid="dictionary-origin"]')?.textContent}`);
    assert(!document.querySelector('[data-testid="dictionary-reset"]'), 'An untouched source set has nothing to reset');
    dictionary().setValue(dictionary().getValue().replace('team.fixture.invalid', 'mine.fixture.invalid'));
    await wait(() => document.querySelector('[data-testid="dictionary-reset"]')
      && testid('dictionary-origin-label').textContent === 'Changed; from source Team',
      () => `A changed set must become the user's own: ${testid('dictionary-origin').textContent}`);
    await click(testid('dictionary-reset'));
    await wait(() => !document.querySelector('[data-testid="dictionary-reset"]') && dictionary()?.getValue().includes('team.fixture.invalid'),
      () => `Reset must restore the source's values: ${dictionary()?.getValue()}`);

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
      await click(testid('agent-session-close'));
      await wait(() => !testid('agent-sessions').querySelector(`[data-testid="${id}"]`));
    }
    const original = testid('agent-sessions').querySelector(`[data-testid="${originalChat}"]`);
    if (original) await click(original);
    await settle();
  }
  globalThis.agentSettingsReceipt = {categories:3,reasoning:['low','high'],newChats:created.length,existingChatPreserved:true,sourceSkills:['team-shop-deploy'],dictionaryReset:true};
  return true;
})()
