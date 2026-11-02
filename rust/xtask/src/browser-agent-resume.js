(async () => {
  const { assert, wait, click, testid, openConsole, key } = testDriver;
  await openConsole();
  await click(testid('console-agent-toggle'));
  await wait(() => document.querySelector('[data-testid="agent-composer"]'));
  assert(testid('agent-composer').value === 'Draft survives panel changes', `A new Console window must restore saved drafts: active=${localStorage.getItem(`servicesConsole.agentActive.${testid('console-session-name').dataset.sessionId}`)}; value=${JSON.stringify(testid('agent-composer').value)}; chats=${JSON.stringify(Object.keys(localStorage).filter(key => key.startsWith('servicesConsole.agentSession.')).map(key => { const chat = JSON.parse(localStorage.getItem(key)); return [chat.id, chat.title, chat.draft, chat.messages.length, chat.options]; }))}; closed=${localStorage.getItem('servicesConsole.agentClosedSessions')}`);
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

  // The # list shows every chat of every Console session, newest activity first: Close for open
  // chats, Restore for closed ones (into the current session), Delete without confirmation.
  const consoleSession = () => testid('console-session-name').dataset.sessionId;
  const closedIds = () => JSON.parse(localStorage.getItem('servicesConsole.agentClosedSessions') || '[]');
  const record = id => JSON.parse(localStorage.getItem(`servicesConsole.agentSession.${id}`) || 'null');
  const toggle = () => testid('agent-chat-list-toggle');
  const rows = () => [...document.querySelectorAll('[data-testid="agent-chat-list"] tbody tr')].map(row => row.dataset.sessionId);
  const row = id => document.querySelector(`[data-testid="agent-chat-list"] tr[data-session-id="${id}"]`);
  const tab = id => testid('agent-sessions').querySelector(`[data-session-id="${id}"]`);
  const status = () => testid('agent-status').textContent;
  const debug = () => document.querySelector('[data-testid="agent-status-debug"]').textContent;
  const openAgent = async () => {
    if (testid('console-agent-toggle').getAttribute('aria-pressed') !== 'true') await click(testid('console-agent-toggle'));
    await wait(() => document.querySelector('[data-testid="agent-chat-list-toggle"]'), 'Agent panel');
  };
  const openList = async () => {
    if (toggle().getAttribute('aria-pressed') !== 'true') await click(toggle());
    await wait(() => document.querySelector('[data-testid="agent-chat-list"]'), 'The chat list opens');
  };
  const fresh = testid('agent-sessions').querySelector('.active').dataset.sessionId;
  const otherClosed = closedIds().find(id => id !== lastSession);
  assert(otherClosed && closedIds().includes(lastSession), `Both closed chats must be stored: ${closedIds()}`);
  assert(!record(fresh), 'An empty chat is not saved');
  assert(toggle() === testid('agent-sessions').firstElementChild && toggle().textContent.trim() === '#',
    `The # list is the first tab: ${testid('agent-sessions').textContent}`);
  await openList();
  await wait(() => rows().length === 3, () => `Every chat is listed: ${rows()}`);
  assert(rows()[0] === lastSession && rows()[1] === otherClosed && rows()[2] === fresh,
    `Chats must be newest activity first: ${rows()}`);
  assert(!document.querySelector('[data-testid="agent-composer"]'), 'The chat list replaces the chat');
  const cells = id => [...row(id).cells].map(cell => cell.textContent);
  assert(cells(lastSession)[0] === 'Products discussion' && cells(lastSession)[1] === 'Imported'
    && Number(cells(lastSession)[2]) >= 4 && cells(lastSession)[3].startsWith('Closed · ')
    && row(lastSession).dataset.state === 'closed' && !row(lastSession).classList.contains('current'),
    `A closed chat shows its name, session, size and closing time: ${cells(lastSession)}`);
  assert(row(fresh).dataset.state === 'open' && row(fresh).classList.contains('current') && cells(fresh)[2] === '0'
    && row(fresh).querySelector('[data-testid="agent-chat-close"]') && !row(fresh).querySelector('[data-testid="agent-chat-restore"]'),
    `An open chat of this session is highlighted and can be closed: ${row(fresh).outerHTML}`);
  assert(row(lastSession).querySelector('[data-testid="agent-chat-restore"]') && !row(lastSession).querySelector('[data-testid="agent-chat-close"]'),
    'A closed chat can be restored');
  const clipped = [...testid('agent-chat-list').querySelectorAll('tbody button')].filter(button => {
    const cell = button.closest('td').getBoundingClientRect(), box = button.getBoundingClientRect();
    return box.left < cell.left - 0.5 || box.right > cell.right + 0.5;
  }).map(button => `${button.closest('tr').dataset.state} ${button.textContent}`);
  const list = document.querySelector('.agent-chats');
  assert(!clipped.length && list.scrollWidth <= list.clientWidth,
    `Row buttons must be whole and the list must not scroll sideways: ${clipped}; ${list.scrollWidth}/${list.clientWidth}`);
  await click(row(lastSession).querySelector('[data-testid="agent-chat-restore"]'));
  await wait(() => testid('agent-sessions').querySelector('.active')?.dataset.sessionId === lastSession
    && testid('agent-transcript').textContent.includes('Edited live check'), 'Restore reopens the chat with its history');
  assert(!closedIds().includes(lastSession) && toggle().getAttribute('aria-pressed') === 'false', 'A restored chat is open');
  await openList();
  await wait(() => row(lastSession)?.dataset.state === 'open', 'The restored chat is listed as open');
  await click(row(lastSession).querySelector('[data-testid="agent-chat-close"]'));
  await wait(() => closedIds().includes(lastSession) && row(lastSession)?.dataset.state === 'closed' && !tab(lastSession),
    'Close in the list closes the chat and keeps the list open');
  // Leaving the list before the native reply arrives: the unseen failure lights its button red.
  localStorage.setItem('servicesConsole.agentClosedSessions', JSON.stringify([...closedIds(), 'bad.id']));
  await click(toggle());
  await openList();
  await wait(() => row('bad.id'), () => `Injected closed chat listed: ${rows()}`);
  row('bad.id').querySelector('[data-testid="agent-chat-delete"]').click();
  toggle().click();
  await wait(() => toggle().querySelector('.console-state-light')?.dataset.state === 'error',
    () => `A failed delete must light the # button red: ${toggle().outerHTML}`);
  await openList();
  await wait(() => testid('agent-status').classList.contains('is-error')
    && status().includes('Agent · Chats · Delete chat · ERROR'), () => `Delete error status: ${status()}`);
  assert(row('bad.id') && closedIds().includes('bad.id'), 'A failed delete keeps the chat for a retry');
  localStorage.setItem('servicesConsole.agentClosedSessions', JSON.stringify(closedIds().filter(id => id !== 'bad.id')));
  await click(toggle());
  await openList();
  await wait(() => !row('bad.id'), () => `Chats after cleanup: ${rows()}`);
  await click(row(lastSession).querySelector('[data-testid="agent-chat-delete"]'));
  await wait(() => status().includes('Delete chat · OK') && !row(lastSession), () => `Delete a closed chat: ${status()}; ${rows()}`);
  assert(/removed\D*1/.test(debug()), `The chat's native Copilot state must be removed: ${debug()}`);
  assert(!record(lastSession) && !closedIds().includes(lastSession), 'A deleted chat leaves no browser record');
  await click(row(otherClosed).querySelector('[data-testid="agent-chat-restore"]'));
  await wait(() => tab(otherClosed) && testid('agent-composer').value === 'Closed chat draft', 'The other chat is restored');
  await openList();
  await click(row(otherClosed).querySelector('[data-testid="agent-chat-delete"]'));
  await wait(() => status().includes('Delete chat · OK') && !row(otherClosed) && !tab(otherClosed),
    () => `Delete an open chat: ${status()}; ${rows()}`);
  assert(!record(otherClosed) && !closedIds().includes(otherClosed), 'Deleting an open chat leaves no browser record');

  // Chats belong to their Console session; Restore moves a chat into the current session.
  await click(testid('agent-sessions').querySelector('[data-session-id]'));
  const composer = await wait(() => document.querySelector('[data-testid="agent-composer"]'), 'Selecting a chat shows it');
  composer.value = 'Moved chat draft';
  composer.dispatchEvent(new InputEvent('input', { bubbles: true }));
  const moved = testid('agent-sessions').querySelector('.active').dataset.sessionId;
  const home = consoleSession();
  await wait(() => record(moved)?.console_session === Number(home), 'A chat with a draft is saved in its session');
  await click(tab(moved).querySelector('[data-testid="agent-session-close"]'));
  await wait(() => closedIds().includes(moved), 'The chat to move is closed');
  await testDriver.activateSession(0);
  await openAgent();
  assert(!tab(moved), 'Another session does not show the chat');
  await openList();
  await wait(() => row(moved)?.dataset.state === 'closed' && [...row(moved).cells][1].textContent === 'Imported',
    () => `The list shows chats of other sessions: ${rows()}`);
  await click(row(moved).querySelector('[data-testid="agent-chat-restore"]'));
  await wait(() => testid('agent-sessions').querySelector('.active')?.dataset.sessionId === moved
    && testid('agent-composer').value === 'Moved chat draft', 'Restore opens the chat in this session');
  assert(record(moved).console_session === 0 && !closedIds().includes(moved), 'Restore moves the chat into this session');
  await testDriver.activateSession(Number(home));
  await openAgent();
  assert(!tab(moved), 'A moved chat leaves its former session');
  await openList();
  await wait(() => row(moved)?.dataset.state === 'open' && [...row(moved).cells][1].textContent === 'Temporary'
    && row(moved).querySelector('[data-testid="agent-chat-close"]') && !row(moved).classList.contains('current'),
    () => `A chat open in another session can be closed here: ${row(moved)?.outerHTML}`);
  await click(row(moved).querySelector('[data-testid="agent-chat-close"]'));
  await wait(() => row(moved)?.dataset.state === 'closed' && closedIds().includes(moved), 'Close works across sessions');
  await click(testid('agent-sessions').querySelector('[data-session-id]'));
  await wait(() => document.querySelector('[data-testid="agent-composer"]'), 'Selecting a chat leaves the chat list');

  // Agent settings saved in an earlier window show the saved Reasoning once the model catalog loads.
  await testDriver.settingsTab('agent');
  await wait(() => testid('agent-model-input')?.value === 'Sample Powerful' && !testid('agent-reasoning').disabled,
    () => `Saved Agent settings did not load: ${testid('agent-model-input')?.value}`);
  assert(testid('agent-reasoning').value === '0' && testid('agent-reasoning').getAttribute('aria-valuetext') === 'low'
    && testid('agent-reasoning-value').textContent === 'low',
    `A new window must show the saved Reasoning: ${testid('agent-reasoning').value}; ${testid('agent-reasoning-value').textContent}`);
  return true;
})()
