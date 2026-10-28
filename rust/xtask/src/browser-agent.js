(async () => {
  const { assert, wait, sleep, click, doubleClick, testid, cm, choose, setSource, key, assertNativeScrollbars } = testDriver;
  await choose('console');
  await click(testid('console-service-rest'));
  await click(testid('console-agent-toggle'));
  await wait(() => document.querySelector('[data-testid="agent-composer"]'),
    () => `Agent did not mount: ${document.querySelector('[data-testid="console-tool-panel"]')?.textContent}`);
  const source = `#check Agent acceptance\n#endpoint\nGET ${fixtureUrl}/agent-before\n\n#headers\nX-Token: {{second:TOKEN}}\n`;
  setSource(cm('console-editor'), source);
  await sleep(50);
  const textarea = testid('agent-composer');
  const input = text => {
    textarea.value = text;
    textarea.dispatchEvent(new InputEvent('input', { bubbles: true }));
  };
  const transcript = () => testid('agent-transcript').textContent;
  const send = async text => {
    input(text);
    await wait(() => testid('agent-stop').disabled);
    textarea.focus();
    await key('Shift+Enter');
  };
  const completed = async text => {
    await wait(() => transcript().includes(text),
      () => `Missing Agent reply "${text}": ${transcript()}; ${document.querySelector('.agent-error')?.textContent || ''}`);
    await wait(() => testid('agent-stop').disabled);
  };
  assert(!document.querySelector('[data-testid="agent-share-context"]'), 'Context sharing must not require a checkbox');
  assert(!document.querySelector('[data-testid="agent-send"]'), 'Sending must use Shift+Enter without a Send button');
  textarea.focus();
  await key('Shift+Enter');
  assert(testid('agent-stop').disabled && textarea.value === '', 'An empty keyboard send must not start a turn');
  input('agent hello');
  await key('Shift+Enter');
  await completed('Copilot session ready');
  assert(testid('agent-transcript').querySelectorAll('.agent-message-assistant').length === 1,
    'Final assistant messages must replace streamed deltas without duplication');
  const assistantCount = () => testid('agent-transcript').querySelectorAll('.agent-message-assistant').length;
  const progress = () => document.querySelector('[data-testid="agent-status-debug"]').textContent;
  let beforeProgress = assistantCount();
  await send('agent progress');
  await completed('Progress final answer.');
  assert(assistantCount() === beforeProgress + 1
    && !transcript().includes('Opening the form.') && !transcript().includes('Waiting for save.'),
    'A multi-step turn must publish only one final assistant answer');
  assert(progress().includes('Opening the form.') && progress().includes('Waiting for save.')
    && progress().includes('Progress final answer.'), `Completed Debug must retain intermediate messages: ${progress()}`);
  beforeProgress = assistantCount();
  await send('agent progress hold');
  await wait(() => progress().includes('Unfinished draft'));
  assert(!testid('agent-stop').disabled && assistantCount() === beforeProgress
    && !transcript().includes('Unfinished draft'), 'Running updates and incomplete answers must appear only in Debug');
  await click(testid('agent-stop'));
  await wait(() => testid('agent-stop').disabled);
  assert(assistantCount() === beforeProgress && progress().includes('Unfinished draft'),
    'Stopping must retain Debug without publishing an unfinished answer');
  await send('agent progress empty');
  await wait(() => progress().includes('Waiting for save.') && testid('agent-stop').disabled);
  assert(assistantCount() === beforeProgress, 'An empty final message must not publish earlier commentary as an answer');
  assert([...testid('agent-transcript').querySelectorAll('.agent-message-assistant .agent-markdown')]
    .every(message => message.textContent.trim()), 'Empty Copilot blocks must never be displayed');
  const activeChat = testid('agent-sessions').querySelector('.active').dataset.sessionId;
  assert(!JSON.parse(localStorage.getItem(`servicesConsole.agentSession.${activeChat}`)).messages
    .some(message => message.text === 'Opening the form.' || message.text === 'Waiting for save.'
      || message.text === 'Unfinished draft'), 'Progress must not be saved as transcript messages');
  const markdownPrompt = 'agent markdown **literal user** <b>not HTML</b>';
  input(markdownPrompt);
  textarea.focus();
  const composing = new KeyboardEvent('keydown', {
    key: 'Enter', shiftKey: true, isComposing: true, bubbles: true, cancelable: true,
  });
  textarea.dispatchEvent(composing);
  assert(!composing.defaultPrevented && testid('agent-stop').disabled,
    'IME composition must not trigger sending');
  const repeated = new KeyboardEvent('keydown', {
    key: 'Enter', shiftKey: true, repeat: true, bubbles: true, cancelable: true,
  });
  textarea.dispatchEvent(repeated);
  assert(repeated.defaultPrevented && textarea.value === markdownPrompt,
    'Holding Shift+Enter must not send repeatedly or insert newlines');
  await key('Enter');
  await wait(() => textarea.value === `${markdownPrompt}\n`, 'Enter must insert a newline');
  assert(testid('agent-stop').disabled, 'Enter alone must not send');
  await key('Shift+Enter');
  await completed('Markdown ready');
  assertNativeScrollbars(testid('agent-transcript'));
  assertNativeScrollbars(textarea);
  assert(textarea.value === '', 'Shift+Enter must send and clear the draft');
  const formatted = [...testid('agent-transcript').querySelectorAll('.agent-markdown')].at(-1);
  assert(formatted.querySelector('h2')?.textContent === 'Formatted reply', 'Markdown headings');
  assert(formatted.querySelector('li strong')?.textContent === 'Method and URL:', 'Markdown lists and bold text');
  assert(formatted.querySelector('li em')?.textContent === 'Body:', 'Markdown emphasis');
  assert(formatted.querySelector('p code')?.textContent === '#check test1', 'Inline request code');
  assert(formatted.querySelector('pre code.language-sql')?.textContent === 'SELECT id, name\nFROM products;\n',
    'Fenced code must preserve language, whitespace and line breaks');
  assert(getComputedStyle(formatted.querySelector('pre')).whiteSpace === 'pre',
    'Code blocks must preserve whitespace rather than wrap as prose');
  assertNativeScrollbars(formatted.querySelector('pre'));
  assert(formatted.querySelectorAll('th').length === 2 && formatted.querySelectorAll('td').length === 2,
    'Markdown tables must have header and body cells');
  assert(formatted.querySelector('blockquote')?.textContent.includes('Review the ID'), 'Markdown blockquotes');
  assert(formatted.querySelector('input[type="checkbox"]')?.disabled
    && formatted.querySelector('input[type="checkbox"]').checked, 'Markdown task markers must be read-only');
  const link = formatted.querySelector('a');
  assert(link?.href === 'https://example.invalid/docs' && link.target === '_blank'
    && link.rel.includes('noopener') && link.rel.includes('noreferrer'), 'Safe external Markdown links');
  assert(formatted.querySelectorAll('a').length === 1
    && formatted.textContent.includes('Unsafe link') && formatted.textContent.includes('Unsafe data'),
    'Unsafe link schemes must remain inert readable text');
  assert(!formatted.querySelector('img,script,iframe,svg,object')
    && !globalThis.agentMarkdownExecuted && formatted.textContent.includes('<img src=')
    && formatted.textContent.includes('External image'),
    'Raw HTML must remain literal and Markdown images must not load external resources');
  const userMessage = [...testid('agent-transcript').querySelectorAll('.agent-message-user pre')].at(-1);
  assert(userMessage.textContent.includes('**literal user** <b>not HTML</b>') && !userMessage.querySelector('b'),
    'User messages must retain literal text rather than render Markdown or HTML');
  await send('agent guide');
  await completed('Application knowledge ready: interface, discovery, services, variables.');
  const header = testid('agent-sessions').getBoundingClientRect();
  const serviceHeader = document.querySelector('.console-tab-strip').getBoundingClientRect();
  assert(Math.abs(header.top - serviceHeader.top) < 1, 'Agent sessions must use the shared top tab row');
  assert(!document.querySelector('.agent-session-title') && !testid('console-agent').querySelector('input[aria-label="Chat session name"]'),
    'Chat naming must not have a separate field above the transcript');
  const agentStyle = getComputedStyle(testid('agent-sessions').querySelector('.active'));
  const serviceStyle = getComputedStyle(document.querySelector('.console-tab-strip > .active'));
  for (const property of ['backgroundColor', 'color', 'borderTopWidth', 'borderBottomWidth', 'borderRadius', 'paddingTop', 'paddingBottom', 'fontSize', 'fontWeight'])
    assert(agentStyle[property] === serviceStyle[property], `Agent tabs must share Console styling: ${property}`);
  await send('agent edit');
  await completed('Edited live check');
  let editor = cm('console-editor');
  assert(editor.getValue().includes('X-Agent: edited'), 'Agent edits must reach the live editor');
  assert(editor.getValue().includes(`${fixtureUrl}/agent-before`), 'Omitted connection directives must be preserved locally');
  editor.undo();
  await wait(() => editor.getValue() === source, 'Agent edits must be one normal Undo step');
  await sleep(30);
  const before = editor.getValue();
  await send('agent stale edit');
  await completed('Stale edit rejected');
  assert(editor.getValue() === before, 'Stale Agent writes must not change the document');
  await send('agent stale rename');
  await completed('Stale edit rejected');
  assert(editor.getValue() === before, 'Stale Agent renames must not change the document');
  await send('agent bad title');
  await completed('Prefixed title rejected');
  assert(editor.getValue() === before, 'A title containing #check must be rejected without inserting a check');
  await send('agent create');
  await completed('Created live check');
  assert(editor.getValue().includes('#check Created by agent'), 'Agent must create a parsed check');
  await wait(() => document.querySelectorAll('[data-testid="console-check-menu"] button').length === 2);
  editor.undo();
  await wait(() => editor.getValue() === before, 'Creating a check must be undoable');
  const count = async () => (await (await fetch(`${fixtureUrl}/agent-count`)).json()).count;
  const initialCount = await count();
  const runSource = source.replace('/agent-before', '/agent-run');
  setSource(editor, runSource);
  await sleep(50);
  await send('agent run denied');
  await wait(() => document.querySelector('[data-testid="agent-run-approval"]'));
  await sleep(100);
  assert(await count() === initialCount, 'Agent must never execute before approval');
  const sentBeforeBusyShortcut = testid('agent-transcript').querySelectorAll('.agent-message-user').length;
  input('Keep this draft while running');
  textarea.focus();
  await key('Shift+Enter');
  assert(testid('agent-transcript').querySelectorAll('.agent-message-user').length === sentBeforeBusyShortcut
    && textarea.value === 'Keep this draft while running', 'Keyboard sending must respect an active turn');
  await click(testid('agent-deny-run'));
  await completed('Execution denied by the user');
  assert(await count() === initialCount, 'Denied runs must not reach the service');
  await send('agent run stale');
  await wait(() => document.querySelector('[data-testid="agent-run-approval"]'));
  editor.replaceRange('\n# changed while waiting', { line: editor.lastLine(), ch: editor.getLine(editor.lastLine()).length });
  await sleep(50);
  await click(testid('agent-approve-run'));
  await completed('CONFLICT');
  assert(await count() === initialCount, 'Approval must be bound to the exact request revision');
  setSource(editor, runSource);
  await sleep(50);
  await send('agent stop');
  await wait(() => document.querySelector('[data-testid="agent-run-approval"]'));
  await click(testid('agent-stop'));
  await completed('Agent stopped');
  assert(!document.querySelector('[data-testid="agent-run-approval"]'), 'Stop must revoke pending approval');
  assert(await count() === initialCount, 'Stop must not approve execution');
  await send('agent run approved');
  await wait(() => document.querySelector('[data-testid="agent-run-approval"]'));
  await click(testid('agent-approve-run'));
  await completed('Read response:');
  assert(transcript().includes('agent-approved-response'), `Agent must read the actual captured response: ${transcript()}`);
  assert(await count() === initialCount + 1, 'Approval must execute exactly once');
  const beforeRename = editor.getValue();
  const renamed = beforeRename.replace(/^#check[^\n]*/, '#check products overview');
  await send('agent rename');
  await completed('Renamed live check; captured response retained');
  assert(editor.getValue() === renamed, 'A title-only edit must preserve the full request and captured output byte-for-byte');
  editor.undo();
  await wait(() => editor.getValue() === beforeRename, 'Renaming must be one Undo step');
  editor.redo();
  await wait(() => editor.getValue() === renamed, 'Renaming must support Redo');
  editor.undo();
  await wait(() => editor.getValue() === beforeRename);
  await send('agent rename and edit');
  await completed('Renamed and edited live check');
  assert(editor.getLine(0) === '#check products updated' && editor.getValue().includes('X-Agent: renamed')
    && editor.getValue().includes(`${fixtureUrl}/agent-run`),
    'A combined title/request edit must preserve omitted connection directives');
  editor.undo();
  await wait(() => editor.getValue() === beforeRename, 'Combined title/request changes must be one Undo step');
  await send('agent switch');
  await completed('Switched to mysql');
  assert(testid('console-service-mysql').classList.contains('active'), 'Agent must visibly select services');
  await click(testid('console-service-rest'));
  input('Draft survives panel changes');
  await sleep(30);
  await click(testid('console-variables-toggle'));
  await click(testid('console-agent-toggle'));
  assert(testid('agent-composer').value === 'Draft survives panel changes', 'Closing Agent must preserve drafts');
  const firstSession = testid('agent-sessions').querySelector('.active').dataset.sessionId;
  const tab = id => testid('agent-sessions').querySelector(`[data-session-id="${id}"]`);
  const renameInput = async text => {
    await doubleClick(tab(firstSession));
    const field = await wait(() => document.querySelector('[data-testid="agent-session-rename"]'));
    await wait(() => document.activeElement === field, 'Inline chat rename must receive focus');
    assert(tab(firstSession).contains(field), 'The rename input must be inside the tab');
    assert(field.selectionStart === 0 && field.selectionEnd === field.value.length, 'The current name must be selected');
    field.value = text;
    field.dispatchEvent(new InputEvent('input', { bubbles: true }));
  };
  const messagesBeforeRename = testid('agent-transcript').querySelectorAll('.agent-message-user').length;
  await renameInput('Products chat');
  await key('Enter');
  await wait(() => !document.querySelector('[data-testid="agent-session-rename"]'));
  assert(tab(firstSession).querySelector('.tab-label').textContent === 'Products chat', 'Enter must save an inline chat name');
  assert(testid('agent-transcript').querySelectorAll('.agent-message-user').length === messagesBeforeRename,
    'Enter while renaming must not send a chat message');
  await renameInput('Do not save');
  await key('Escape');
  await wait(() => !document.querySelector('[data-testid="agent-session-rename"]'));
  assert(tab(firstSession).querySelector('.tab-label').textContent === 'Products chat', 'Escape must cancel renaming');
  await renameInput('Products discussion');
  await click(textarea);
  await wait(() => tab(firstSession).querySelector('.tab-label')?.textContent === 'Products discussion',
    'Blur must save an inline chat name');
  await click(testid('agent-new-session'));
  const secondSession = testid('agent-sessions').querySelector('.active').dataset.sessionId;
  assert(testid('agent-transcript').querySelectorAll('.agent-message').length === 0, 'Session tabs must have independent histories');
  input('Closed chat draft');
  await click(testid('agent-sessions').querySelector(`[data-session-id="${firstSession}"]`));
  await wait(() => testid('agent-composer').value === 'Draft survives panel changes',
    () => `Session draft was not restored: ${testid('agent-sessions').textContent}; ${testid('agent-composer').value}`);
  assert(transcript().includes('Edited live check'), 'Session history must survive tab switches');
  await click(tab(secondSession).querySelector('[data-testid="agent-session-close"]'));
  assert(!tab(secondSession) && tab(firstSession).classList.contains('active')
    && textarea.value === 'Draft survives panel changes', 'Closing an inactive tab must preserve the selected chat and its draft');
  assert(JSON.parse(localStorage.getItem(`servicesConsole.agentSession.${secondSession}`)).draft === 'Closed chat draft',
    'Closing a tab must retain its saved history and draft');
  await click(testid('agent-new-session'));
  const runningSession = testid('agent-sessions').querySelector('.active').dataset.sessionId;
  await send('agent run closing');
  await wait(() => document.querySelector('[data-testid="agent-run-approval"]'));
  await click(tab(runningSession).querySelector('[data-testid="agent-session-close"]'));
  await wait(() => !tab(runningSession) && !document.querySelector('[data-testid="agent-run-approval"]'));
  assert(tab(firstSession).classList.contains('active'), 'Closing the active chat must select an adjacent remaining chat');
  assert(await count() === initialCount + 1, 'Closing an active Agent chat must revoke approval without executing');
  const saved = Object.keys(localStorage).filter(key => key.startsWith('servicesConsole.agentSession.'))
    .map(key => JSON.parse(localStorage.getItem(key)));
  const firstSaved = saved.find(chat => chat.id === firstSession);
  const closed = JSON.parse(localStorage.getItem('servicesConsole.agentClosedSessions'));
  assert(closed.includes(secondSession) && closed.includes(runningSession)
    && saved.filter(chat => !closed.includes(chat.id)).length === 1 && firstSaved.draft === 'Draft survives panel changes'
    && firstSaved.title === 'Products discussion', 'Open/closed tabs, names and drafts must be persisted independently');
  assert(!firstSaved.messages.some(message => message.text.includes('resolved-fixture')), 'Agent history must not capture resolved credentials');
  await click(testid('console-agent-toggle'));
  return true;
})()
