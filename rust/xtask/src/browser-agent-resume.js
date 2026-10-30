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
  return true;
})()
