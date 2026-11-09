(async () => {
  const { assert, wait, click, testid, openConsole, openPanel, closePanel, key } = testDriver;
  await openConsole();
  const openAgent = () => openPanel('agent', 'console-agent');
  await openAgent();
  const chats = () => JSON.parse(localStorage.getItem('module:agent:chats') || '{}');
  const record = id => chats()[id] || null;
  const selected = () => document.querySelector('[data-testid="agent-sessions"] [role="tab"][aria-selected="true"]')?.dataset.testid.slice('agent-chat-'.length);
  const tab = id => document.querySelector(`[data-testid="agent-sessions"] [data-testid="agent-chat-${id}"]`);
  const openTabs = () => document.querySelectorAll('[data-testid="agent-sessions"] [role="tab"]').length;
  const transcript = () => testid('agent-transcript').textContent;
  const composer = () => testid('agent-composer');
  await wait(() => document.querySelector('[data-testid="agent-composer"]')?.value === 'Draft survives panel changes',
    () => `A new Console window must restore saved drafts: value=${JSON.stringify(document.querySelector('[data-testid="agent-composer"]')?.value)}; chats=${JSON.stringify(Object.values(chats()).map(chat => [chat.id, chat.title, chat.draft, chat.messages.length, chat.closed]))}`);
  const chat = selected();
  assert(transcript().includes('Edited live check'), 'A new window must restore chat history');
  assert([...testid('agent-transcript').querySelectorAll('[data-testid^="agent-message-assistant-"]')]
    .every(message => message.textContent.trim()), 'Empty entries from saved older histories must remain hidden');
  assert(tab(chat).querySelector('.tab-label')?.textContent === 'Products discussion', 'Chat names must survive a new window');
  const closedIds = () => Object.values(chats()).filter(chat => chat.closed).map(chat => chat.id);
  assert(openTabs() === 1 && closedIds().every(id => !tab(id)), 'Closed tabs must not reopen in a new window');
  assert(!document.querySelector('[data-testid="agent-share-context"]'), 'Restored chats must not require sharing approval');
  assert(!document.querySelector('[data-testid="agent-send"]'), 'Restored chats must not show a Send button');
  composer().value = 'agent hello resume';
  composer().dispatchEvent(new InputEvent('input', { bubbles: true }));
  composer().focus();
  await key('Enter');
  await wait(() => transcript().includes('Resumed Copilot session'),
    () => `SDK session was not resumed: ${transcript()}`);
  await wait(() => testid('agent-stop').disabled);
  assert(transcript().includes('Edited live check'), 'Resuming must not replace earlier messages');
  const lastSession = selected();
  await click(document.querySelector('[data-testid="agent-sessions"] [aria-selected="true"] [data-testid="agent-session-close"]'));
  await wait(() => openTabs() === 1 && selected() && selected() !== lastSession);
  assert(testid('agent-transcript').querySelectorAll('[data-testid^="agent-message-"]').length === 0 && composer().value === '',
    'Closing the last tab must create a fresh empty chat');
  assert(record(lastSession).messages.some(message => message.text.includes('Edited live check')),
    'Closing the last tab must preserve its history');

  // The # list shows every chat of every Console session, newest activity first: Close for open
  // chats, Restore for closed ones (into the current session), Delete without confirmation.
  const consoleSession = () => testid('console-session-name').dataset.sessionId;
  const toggle = () => testid('agent-chat-list-toggle');
  const listed = () => !!document.querySelector('[data-testid="agent-chat-list"]');
  const rows = () => [...document.querySelectorAll('[data-testid="agent-chat-list"] tbody tr')].map(row => row.dataset.testid.slice('agent-chat-row-'.length));
  const row = id => document.querySelector(`[data-testid="agent-chat-list"] tr[data-testid="agent-chat-row-${id}"]`);
  const cells = id => [...row(id).cells].map(cell => cell.textContent);
  const closedRow = id => cells(id)[3].startsWith('Closed');
  const button = (id, name) => row(id)?.querySelector(`[data-testid="agent-chat-${name}"]`);
  const status = () => testid('console-agent-status').textContent;
  const debug = () => document.querySelector('[data-testid="console-agent-status-debug"]')?.textContent || '';
  const openList = async () => {
    if (!listed()) await click(toggle());
    await wait(listed, 'The chat list opens');
  };
  const closeList = async () => {
    if (listed()) await click(toggle());
    await wait(() => !listed(), 'The chat list closes');
  };
  const fresh = selected();
  const otherClosed = closedIds().find(id => id !== lastSession);
  assert(otherClosed && closedIds().includes(lastSession), `Both closed chats must be stored: ${closedIds()}`);
  assert(!record(fresh), 'An empty chat is not saved');
  await openList();
  await wait(() => rows().length === 3, () => `Every chat is listed: ${rows()}`);
  assert(rows()[0] === lastSession && rows()[1] === otherClosed && rows()[2] === fresh,
    `Chats must be newest activity first: ${rows()}`);
  assert(!document.querySelector('[data-testid="agent-composer"]'), 'The chat list replaces the chat');
  assert(cells(lastSession)[0] === 'Products discussion' && cells(lastSession)[1] === 'Imported'
    && Number(cells(lastSession)[2]) >= 4 && cells(lastSession)[3].startsWith('Closed · '),
    `A closed chat shows its name, session, size and closing time: ${cells(lastSession)}`);
  assert(!closedRow(fresh) && cells(fresh)[2] === '0' && button(fresh, 'close') && !button(fresh, 'restore'),
    `An open chat can be closed: ${row(fresh).outerHTML}`);
  assert(button(lastSession, 'restore') && !button(lastSession, 'close'), 'A closed chat can be restored');
  const clipped = [...testid('agent-chat-list').querySelectorAll('tbody button')].filter(button => {
    const cell = button.closest('td').getBoundingClientRect(), box = button.getBoundingClientRect();
    return box.left < cell.left - 0.5 || box.right > cell.right + 0.5;
  }).map(button => `${button.closest('tr').dataset.testid} ${button.textContent}`);
  assert(!clipped.length, `Row buttons must be whole: ${clipped}`);
  await click(button(lastSession, 'restore'));
  await wait(() => selected() === lastSession && transcript().includes('Edited live check'), 'Restore reopens the chat with its history');
  assert(!closedIds().includes(lastSession) && !listed(), 'A restored chat is open');
  await openList();
  await wait(() => row(lastSession) && !closedRow(lastSession), 'The restored chat is listed as open');
  await click(button(lastSession, 'close'));
  await wait(() => closedIds().includes(lastSession) && row(lastSession) && closedRow(lastSession) && listed(),
    'Close in the list closes the chat and keeps the list open');
  // A chat the native host cannot delete keeps its row and reports the failure.
  const injected = chats();
  injected['bad.id'] = { ...injected[lastSession], id: 'bad.id', title: 'Broken chat' };
  localStorage.setItem('module:agent:chats', JSON.stringify(injected));
  await closePanel('agent');
  await openAgent();
  await openList();
  await wait(() => row('bad.id'), () => `Injected closed chat listed: ${rows()}`);
  await click(button('bad.id', 'delete'));
  await wait(() => testid('console-agent-status').classList.contains('is-error')
    && status().includes('Agent · Chats · Delete chat · ERROR'), () => `Delete error status: ${status()}`);
  assert(row('bad.id') && closedIds().includes('bad.id'), 'A failed delete keeps the chat for a retry');
  const cleaned = chats();
  delete cleaned['bad.id'];
  localStorage.setItem('module:agent:chats', JSON.stringify(cleaned));
  await closePanel('agent');
  await openAgent();
  await openList();
  await wait(() => !row('bad.id') && rows().length, () => `Chats after cleanup: ${rows()}`);
  await click(button(lastSession, 'delete'));
  await wait(() => status().includes('Delete chat · OK') && !row(lastSession), () => `Delete a closed chat: ${status()}; ${rows()}`);
  assert(/removed\D*1/.test(debug()), `The chat's native Copilot state must be removed: ${debug()}`);
  assert(!record(lastSession), 'A deleted chat leaves no browser record');
  await click(button(otherClosed, 'restore'));
  await wait(() => tab(otherClosed) && composer().value === 'Closed chat draft', 'The other chat is restored');
  await openList();
  await click(button(otherClosed, 'delete'));
  await wait(() => status().includes('Delete chat · OK') && !row(otherClosed),
    () => `Delete an open chat: ${status()}; ${rows()}`);
  assert(!record(otherClosed), 'Deleting an open chat leaves no browser record');
  await closeList();
  assert(!tab(otherClosed), 'A deleted chat has no tab');

  // Chats belong to their Console session; Restore moves a chat into the current session.
  composer().value = 'Moved chat draft';
  composer().dispatchEvent(new InputEvent('input', { bubbles: true }));
  const moved = selected();
  const home = consoleSession();
  await wait(() => record(moved)?.console_session === Number(home), 'A chat with a draft is saved in its session');
  await click(document.querySelector('[data-testid="agent-sessions"] [aria-selected="true"] [data-testid="agent-session-close"]'));
  await wait(() => closedIds().includes(moved), 'The chat to move is closed');
  await testDriver.activateSession(0);
  await openAgent();
  await wait(() => selected(), 'The other session shows a chat');
  assert(!tab(moved), 'Another session does not show the chat');
  await openList();
  await wait(() => row(moved) && closedRow(moved) && cells(moved)[1] === 'Imported',
    () => `The list shows chats of other sessions: ${rows()}`);
  await click(button(moved, 'restore'));
  await wait(() => selected() === moved && composer().value === 'Moved chat draft', 'Restore opens the chat in this session');
  await wait(() => record(moved).console_session === 0 && !closedIds().includes(moved), 'Restore moves the chat into this session');
  await testDriver.activateSession(Number(home));
  await openAgent();
  await wait(() => selected() && selected() !== moved, 'The former session shows its own chats');
  assert(!tab(moved), 'A moved chat leaves its former session');
  await openList();
  await wait(() => row(moved) && !closedRow(moved) && cells(moved)[1] === 'Temporary' && button(moved, 'close'),
    () => `A chat open in another session can be closed here: ${row(moved)?.outerHTML}`);
  await click(button(moved, 'close'));
  await wait(() => row(moved) && closedRow(moved) && closedIds().includes(moved), 'Close works across sessions');
  await closeList();
  await wait(() => document.querySelector('[data-testid="agent-composer"]'), 'Leaving the list shows the chat');

  // Agent settings saved in an earlier window show the saved Reasoning once the model catalog loads.
  await testDriver.settingsTab('agent');
  await wait(() => document.querySelector('[data-testid="agent-model-input"]')?.value === 'Sample Powerful'
    && document.querySelector('[data-testid="agent-reasoning"]'),
    () => `Saved Agent settings did not load: ${document.querySelector('[data-testid="agent-model-input"]')?.value}`);
  assert(testid('agent-reasoning').value === '0' && testid('agent-reasoning-value').textContent === 'low',
    `A new window must show the saved Reasoning: ${testid('agent-reasoning').value}; ${testid('agent-reasoning-value').textContent}`);
  return true;
})()
