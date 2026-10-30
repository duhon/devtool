(async () => {
  // Setup for agent-resume: creates through the UI the saved chat a later Console window must restore
  // (a live SDK session with an edit, an inline name, a draft, a closed tab and an empty legacy entry).
  const { assert, wait, click, doubleClick, testid, cm, openConsole, setSource, key } = testDriver;
  await openConsole();
  await click(testid('console-service-rest'));
  await click(testid('console-agent-toggle'));
  const textarea = await wait(() => document.querySelector('[data-testid="agent-composer"]'), 'Agent composer');
  setSource(cm('console-editor'), `#check Agent resume\n#endpoint\nGET ${fixtureUrl}/agent-before\n`);
  const input = text => {
    textarea.value = text;
    textarea.dispatchEvent(new InputEvent('input', { bubbles: true }));
  };
  const transcript = () => testid('agent-transcript').textContent;
  const send = async (text, reply) => {
    input(text);
    await wait(() => testid('agent-stop').disabled, 'Agent idle before sending');
    textarea.focus();
    await key('Enter');
    await wait(() => transcript().includes(reply),
      () => `Agent reply "${reply}": ${transcript()}; ${document.querySelector('.agent-error')?.textContent || ''}`);
    await wait(() => testid('agent-stop').disabled, 'Agent idle after reply');
  };
  await send('agent hello', 'Copilot session ready');
  await send('agent edit', 'Edited live check');
  const sessions = () => testid('agent-sessions');
  const chat = sessions().querySelector('.active').dataset.sessionId;
  await click(testid('agent-new-session'));
  const closedChat = sessions().querySelector('.active').dataset.sessionId;
  assert(closedChat !== chat, 'A new chat tab must be selected');
  input('Closed chat draft');
  await click(sessions().querySelector(`[data-session-id="${closedChat}"] [data-testid="agent-session-close"]`));
  await wait(() => !sessions().querySelector(`[data-session-id="${closedChat}"]`)
    && sessions().querySelector('.active')?.dataset.sessionId === chat, 'Closing the new chat selects the first one');
  await doubleClick(sessions().querySelector(`[data-session-id="${chat}"]`));
  const field = await wait(() => document.querySelector('[data-testid="agent-session-rename"]'), 'Inline chat rename');
  field.value = 'Products discussion';
  field.dispatchEvent(new InputEvent('input', { bubbles: true }));
  await key('Enter');
  await wait(() => sessions().querySelector(`[data-session-id="${chat}"] .tab-label`)?.textContent === 'Products discussion',
    'Chat rename saved');
  input('Draft survives panel changes');
  const saved = () => JSON.parse(localStorage.getItem(`servicesConsole.agentSession.${chat}`) || 'null');
  await wait(() => saved()?.draft === 'Draft survives panel changes' && saved()?.title === 'Products discussion'
    && JSON.parse(localStorage.getItem('servicesConsole.agentClosedSessions') || '[]').includes(closedChat)
    && localStorage.getItem('servicesConsole.agentActive') === chat, 'Chat name, draft and closed tab persisted');
  const record = saved();
  record.messages.push({ id: 'legacy-empty', role: 'assistant', text: ' \n ' });
  localStorage.setItem(`servicesConsole.agentSession.${chat}`, JSON.stringify(record));
  return true;
})()
