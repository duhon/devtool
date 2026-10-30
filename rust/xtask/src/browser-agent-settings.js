(async () => {
  const {assert, wait, testid, click, key, openConsole} = testDriver;
  // A fresh panel: wait for the restored Console session before using its toolbar.
  await openConsole();
  if (testid('console-agent-toggle').getAttribute('aria-pressed') !== 'true')
    await click(testid('console-agent-toggle'));
  const originalChat = testid('agent-sessions').querySelector('[aria-selected="true"]').dataset.sessionId;
  const originalDraft = testid('agent-composer').value;
  const originalOptions = localStorage.getItem('servicesConsole.agentOptions');
  const created = [];
  const input = (element, value) => {
    element.value = value;
    element.dispatchEvent(new InputEvent('input', {bubbles:true}));
  };
  const settings = async () => {
    if (testid('console-settings-toggle').getAttribute('aria-pressed') !== 'true')
      await click(testid('console-settings-toggle'));
    await wait(() => testid('agent-model-input') && !testid('agent-model-input').disabled,
      () => `Model catalog did not load: ${document.querySelector('[data-testid="settings-update-status"]')?.textContent}`);
  };
  const agent = async () => {
    if (testid('console-agent-toggle').getAttribute('aria-pressed') !== 'true')
      await click(testid('console-agent-toggle'));
  };
  const selectModel = async (query, name, category) => {
    const field = testid('agent-model-input');
    field.focus();
    input(field, query);
    await wait(() => document.querySelectorAll('[data-testid="agent-model-options"] [role="option"]').length === 1);
    assert(testid('agent-model-options').textContent.includes(category), 'Suggestions must include the model category');
    await key('Enter');
    await wait(() => field.value === name && !document.querySelector('[data-testid="agent-model-options"]'));
    assert(testid('agent-model-category').textContent === category, 'The selected model must keep its category');
  };
  const configuration = async () => {
    const field = testid('agent-composer');
    const before = testid('agent-transcript').querySelectorAll('.agent-message-assistant').length;
    input(field, 'agent model configuration');
    field.focus();
    await key('Enter');
    await wait(() => testid('agent-stop').disabled
      && testid('agent-transcript').querySelectorAll('.agent-message-assistant').length > before,
      () => `Model-config turn did not finish: ${testid('agent-transcript').textContent}; ${document.querySelector('.agent-error')?.textContent}`);
    const message = [...testid('agent-transcript').querySelectorAll('.agent-message-assistant')].at(-1).textContent;
    return JSON.parse(message.match(/Model configuration: (\{[^\n]+\})/)?.[1] || '{}');
  };
  const newChat = async () => {
    await click(testid('agent-new-session'));
    const id = testid('agent-sessions').querySelector('[aria-selected="true"]').dataset.sessionId;
    created.push(id);
    return id;
  };
  try {
    await settings();
    assert(testid('agent-model-input').value === 'Copilot Fusion', 'An unconfigured client must default to Copilot Fusion');
    assert(testid('agent-model-category').textContent === 'Automatic', 'Fusion must be identified as the automatic router');
    assert(testid('agent-reasoning').disabled, 'Fusion must manage Reasoning automatically');
    const field = testid('agent-model-input');
    field.focus();
    await wait(() => document.querySelector('[data-testid="agent-model-options"]'));
    assert(!document.querySelector('[data-model-id="sample-disabled"]'), 'Disabled models must not be selectable');
    await key('ArrowDown');
    await key('ArrowDown');
    assert(testid('agent-model-options').querySelector('[aria-selected="true"]').dataset.modelId === 'sample-powerful',
      'ArrowDown must move the active suggestion');
    await key('ArrowUp');
    await key('Escape');
    assert(!document.querySelector('[data-testid="agent-model-options"]'), 'Escape must close suggestions');
    input(field, 'does-not-exist');
    await wait(() => document.querySelector('[data-testid="agent-model-options"]')?.textContent.includes('No matching models'),
      () => `No-match state missing: value=${field.value}; expanded=${field.getAttribute('aria-expanded')}; settingsHidden=${testid('settings-panel')?.hidden}; active=${document.activeElement?.dataset?.testid}`);
    assert(localStorage.getItem('servicesConsole.agentOptions') === originalOptions, 'Typing a search must not save an invalid model');
    await key('Escape');
    await selectModel('powerful', 'Sample Powerful', 'Powerful');
    const reasoning = testid('agent-reasoning');
    assert([...reasoning.options].map(option => option.value).join(',') === ',low,high',
      'Reasoning must offer only the selected model levels');
    reasoning.value = 'high';
    reasoning.dispatchEvent(new Event('change', {bubbles:true}));
    await wait(() => JSON.parse(localStorage.getItem('servicesConsole.agentOptions')).reasoningEffort === 'high');
    assert(testid('settings-update-status').textContent.includes('New Agent chat settings')
      && !testid('settings-panel').querySelector('[role="alert"]'), 'Settings results belong in the shared footer');
    await agent();
    const old = await configuration();
    assert(old.model === 'auto' && old.reasoningEffort === null, 'Existing chats must not adopt new defaults');
    const powerfulChat = await newChat();
    const powerful = await configuration();
    assert(powerful.model === 'sample-powerful' && powerful.reasoningEffort === 'high',
      `The SDK must receive the selected model and reasoning: ${JSON.stringify(powerful)}`);
    const saved = JSON.parse(localStorage.getItem(`servicesConsole.agentSession.${powerfulChat}`));
    assert(saved.options.model === 'sample-powerful' && saved.options.reasoningEffort === 'high',
      'A chat must persist its captured defaults');
    await settings();
    assert(field.value === 'Sample Powerful' && reasoning.value === 'high', 'Reopening Settings must retain choices');
    await selectModel('light', 'Sample Light', 'Lightweight');
    assert(reasoning.disabled && reasoning.value === '', 'Models without Reasoning must disable and reset the selector');
    await agent();
    const retained = await configuration();
    assert(retained.model === 'sample-powerful' && retained.reasoningEffort === 'high',
      'Changing defaults must not change an already-created SDK session');
    await newChat();
    const light = await configuration();
    assert(light.model === 'sample-light' && light.reasoningEffort === null,
      'New chats must capture the latest model without unsupported Reasoning');
  } finally {
    if (originalOptions === null) localStorage.removeItem('servicesConsole.agentOptions');
    else localStorage.setItem('servicesConsole.agentOptions', originalOptions);
    await agent();
    for (const id of created)
      await click(testid('agent-sessions').querySelector(`[data-session-id="${id}"] [data-testid="agent-session-close"]`));
    await click(testid('agent-sessions').querySelector(`[data-session-id="${originalChat}"]`));
    input(testid('agent-composer'), originalDraft);
  }
  globalThis.agentSettingsReceipt = {categories:3,reasoning:['low','high'],newChats:created.length,existingChatPreserved:true};
  return true;
})()
