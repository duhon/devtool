(async () => {
  // Setup for agent-resume: creates through the UI the saved chat a later Console window must restore
  // (a live SDK session with an edit, a name, a draft, a closed tab and an empty legacy entry).
  const { assert, wait, click, doubleClick, testid, editorOf, openConsole, openPanel, setSource, key } = testDriver;
  await openConsole();
  await click(testid('console-service-rest'));
  await openPanel('agent', 'console-agent');
  setSource(editorOf('console-editor'), `#check Agent resume\n#endpoint\nGET ${fixtureUrl}/agent-before\n`);
  const input = text => {
    const textarea = testid('agent-composer');
    textarea.value = text;
    textarea.dispatchEvent(new InputEvent('input', { bubbles: true }));
  };
  const transcript = () => testid('agent-transcript').textContent;
  const send = async (text, reply) => {
    input(text);
    await wait(() => testid('agent-stop').disabled, 'Agent idle before sending');
    testid('agent-composer').focus();
    await key('Enter');
    await wait(() => transcript().includes(reply),
      () => `Agent reply "${reply}": ${transcript()}; ${document.querySelector('[data-testid="agent-error"]')?.textContent || ''}`);
    await wait(() => testid('agent-stop').disabled, 'Agent idle after reply');
  };
  const selected = () => testid('agent-sessions').querySelector('[role="tab"][aria-selected="true"]')?.dataset.testid.slice('agent-chat-'.length);
  const chats = () => JSON.parse(localStorage.getItem('module:agent:chats') || '{}');
  await send('agent hello', 'Copilot session ready');
  await send('agent edit', 'Edited live check');
  const chat = selected();
  await click(testid('agent-new-session'));
  await wait(() => selected() !== chat, 'A new chat tab must be selected');
  const closedChat = selected();
  input('Closed chat draft');
  await click(testid('agent-session-close'));
  await wait(() => !testid('agent-sessions').querySelector(`[data-testid="agent-chat-${closedChat}"]`)
    && selected() === chat, 'Closing the new chat selects the first one');
  await doubleClick(testid(`agent-chat-${chat}`));
  const field = await wait(() => document.querySelector('[data-testid="agent-sessions-rename"]'), 'Chat rename');
  field.value = 'Products discussion';
  field.dispatchEvent(new InputEvent('input', { bubbles: true }));
  field.focus();
  await key('Enter');
  await wait(() => testid(`agent-chat-${chat}`).querySelector('.tab-label')?.textContent === 'Products discussion', 'Chat rename saved');
  input('Draft survives panel changes');
  const session = testid('console-session-name').dataset.sessionId;
  await wait(() => chats()[chat]?.draft === 'Draft survives panel changes' && chats()[chat]?.title === 'Products discussion'
    && chats()[closedChat]?.closed && JSON.parse(localStorage.getItem(`module:agent:active.${session}`) || 'null') === chat,
    () => `Chat name, draft and closed tab persisted: ${JSON.stringify(chats())}`);
  const saved = chats();
  saved[chat].messages.push({ id: 'legacy-empty', role: 'assistant', text: ' \n ' });
  localStorage.setItem('module:agent:chats', JSON.stringify(saved));
  localStorage.setItem('servicesConsole.agentOptions', JSON.stringify({ model: 'sample-powerful', reasoningEffort: 'low' }));
  return true;
})()
