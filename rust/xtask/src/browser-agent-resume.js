(async () => {
  const { assert, wait, click, testid, openConsole, key } = testDriver;
  await openConsole();
  await click(testid('console-agent-toggle'));
  await wait(() => document.querySelector('[data-testid="agent-composer"]'));
  assert(testid('agent-composer').value === 'Draft survives panel changes', `A new Console window must restore saved drafts: active=${localStorage.getItem('servicesConsole.agentActive')}; value=${JSON.stringify(testid('agent-composer').value)}; chats=${JSON.stringify(Object.keys(localStorage).filter(key => key.startsWith('servicesConsole.agentSession.')).map(key => { const chat = JSON.parse(localStorage.getItem(key)); return [chat.id, chat.title, chat.draft, chat.messages.length, chat.options]; }))}; closed=${localStorage.getItem('servicesConsole.agentClosedSessions')}`);
  assert(testid('agent-transcript').textContent.includes('Edited live check'), 'A new window must restore chat history');
  assert([...testid('agent-transcript').querySelectorAll('.agent-message-assistant .agent-markdown')]
    .every(message => message.textContent.trim()), 'Empty entries from saved older histories must remain hidden');
  assert(testid('agent-sessions').querySelector('.active .tab-label')?.textContent === 'Products discussion',
    'Inline chat names must survive a new window');
  const closed = JSON.parse(localStorage.getItem('servicesConsole.agentClosedSessions'));
  assert(testid('agent-sessions').querySelectorAll('[data-session-id]').length === 1
    && closed.every(id => !testid('agent-sessions').querySelector(`[data-session-id="${id}"]`)),
    'Closed tabs must not reopen in a new window');
  assert(!document.querySelector('[data-testid="agent-share-context"]'), 'Restored chats must not require sharing approval');
  assert(!document.querySelector('[data-testid="agent-send"]'), 'Restored chats must not show a Send button');
  const input = testid('agent-composer');
  input.value = 'agent hello resume';
  input.dispatchEvent(new InputEvent('input', { bubbles: true }));
  input.focus();
  await key('Enter');
  await wait(() => testid('agent-transcript').textContent.includes('Resumed Copilot session'),
    () => `SDK session was not resumed: ${testid('agent-transcript').textContent}`);
  await wait(() => testid('agent-stop').disabled);
  assert(testid('agent-transcript').textContent.includes('Edited live check'), 'Resuming must not replace earlier messages');
  const lastSession = testid('agent-sessions').querySelector('.active').dataset.sessionId;
  await click(testid('agent-sessions').querySelector('[data-testid="agent-session-close"]'));
  await wait(() => testid('agent-sessions').querySelectorAll('[data-session-id]').length === 1
    && testid('agent-sessions').querySelector('.active').dataset.sessionId !== lastSession);
  assert(testid('agent-transcript').querySelectorAll('.agent-message').length === 0 && input.value === '',
    'Closing the last tab must create a fresh empty chat');
  assert(JSON.parse(localStorage.getItem(`servicesConsole.agentSession.${lastSession}`)).messages
    .some(message => message.text.includes('Edited live check')), 'Closing the last tab must preserve its history');

  // Closed chats: newest first, Restore reopens a chat with its history, Delete removes the chat
  // and its native Copilot state; a failed delete keeps the chats and lights the list red.
  const closedIds = () => JSON.parse(localStorage.getItem('servicesConsole.agentClosedSessions') || '[]');
  const toggle = () => testid('agent-closed-chats');
  const rows = () => [...document.querySelectorAll('[data-testid="agent-closed-list"] [data-session-id]')]
    .map(row => row.dataset.sessionId);
  const row = id => document.querySelector(`[data-testid="agent-closed-list"] [data-session-id="${id}"]`);
  const status = () => testid('agent-status').textContent;
  const debug = () => document.querySelector('[data-testid="agent-status-debug"]').textContent;
  const otherClosed = closedIds().find(id => id !== lastSession);
  assert(otherClosed && closedIds().includes(lastSession), `Both closed chats must be stored: ${closedIds()}`);
  await click(toggle());
  await wait(() => rows().length === 2 && toggle().getAttribute('aria-pressed') === 'true',
    () => `Closed chats list: ${rows()}`);
  assert(rows()[0] === lastSession && rows()[1] === otherClosed, `Closed chats must be newest first: ${rows()}`);
  assert(toggle().textContent.includes('Closed (2)') && !document.querySelector('[data-testid="agent-composer"]'),
    'The closed-chats list replaces the chat');
  assert(row(lastSession).textContent.includes('Products discussion') && /\d+ messages · closed /.test(row(lastSession).textContent),
    `A closed chat shows its name, size and closing time: ${row(lastSession).textContent}`);
  await click(row(lastSession).querySelector('[data-testid="agent-closed-restore"]'));
  await wait(() => testid('agent-sessions').querySelector('.active')?.dataset.sessionId === lastSession
    && testid('agent-transcript').textContent.includes('Edited live check'), 'Restore reopens the chat with its history');
  assert(!closedIds().includes(lastSession) && toggle().textContent.includes('Closed (1)'), 'A restored chat is no longer closed');
  await click(testid('agent-sessions').querySelector(`[data-session-id="${lastSession}"] [data-testid="agent-session-close"]`));
  await wait(() => closedIds().includes(lastSession), 'The restored chat closes again');
  const confirm = window.confirm;
  try {
    window.confirm = () => true;
    localStorage.setItem('servicesConsole.agentClosedSessions', JSON.stringify([...closedIds(), 'bad.id']));
    await click(toggle());
    await wait(() => rows().length === 3, () => `Injected closed chat listed: ${rows()}`);
    // Leaving the list before the native reply arrives: the unseen failure lights its button red.
    testid('agent-closed-delete-all').click();
    toggle().click();
    await wait(() => toggle().querySelector('.console-state-light')?.dataset.state === 'error',
      () => `A failed delete must light the Closed button red: ${toggle().outerHTML}`);
    await click(toggle());
    await wait(() => testid('agent-status').classList.contains('is-error')
      && status().includes('Agent · Closed chats · Delete closed chats · ERROR'), () => `Delete error status: ${status()}`);
    assert(rows().length === 3 && localStorage.getItem(`servicesConsole.agentSession.${lastSession}`),
      'A failed delete keeps every chat for a retry');
    localStorage.setItem('servicesConsole.agentClosedSessions', JSON.stringify(closedIds().filter(id => id !== 'bad.id')));
    await click(toggle());
    await click(toggle());
    await wait(() => rows().length === 2, () => `Closed chats after cleanup: ${rows()}`);
    await click(row(lastSession).querySelector('[data-testid="agent-closed-delete"]'));
    await wait(() => status().includes('Delete closed chats · OK') && rows().join() === otherClosed,
      () => `Delete one closed chat: ${status()}; ${rows()}`);
    assert(/removed\D*1/.test(debug()), `The chat's native Copilot state must be removed: ${debug()}`);
    assert(!localStorage.getItem(`servicesConsole.agentSession.${lastSession}`) && !closedIds().includes(lastSession),
      'A deleted chat leaves no browser record');
    await click(testid('agent-closed-delete-all'));
    await wait(() => rows().length === 0 && closedIds().length === 0 && toggle().textContent.includes('Closed (0)'),
      () => `Delete all closed chats: ${status()}; ${rows()}`);
    assert(!localStorage.getItem(`servicesConsole.agentSession.${otherClosed}`), 'Delete all removes every closed chat');
  } finally {
    window.confirm = confirm;
  }
  await click(testid('agent-sessions').querySelector('[data-session-id]'));
  await wait(() => document.querySelector('[data-testid="agent-composer"]'), 'Selecting a chat leaves the closed-chats list');
  return true;
})()
