(async () => {
  // Starts two Terminal checks, a running REST check and a pending Agent approval in this window,
  // then returns the Terminal process IDs so the harness can close the window and watch them exit.
  const { wait, click, testid, cm, openConsole, setSource, key } = testDriver;
  const editor = () => cm('console-editor');
  const source = () => editor().getValue();
  const status = () => document.querySelector('.console-status');
  const checks = () => [...testid('console-check-menu').querySelectorAll('button')];
  const select = async index => {
    await click(checks()[index]);
    await wait(() => checks()[index]?.classList.contains('active'), `Selecting check ${index + 1}`);
    editor().setCursor(Number(checks()[index].dataset.line) - 1, 0);
  };
  const run = async () => {
    editor().focus();
    await key('Shift+Enter');
  };
  const terminal = name => `#check ${name}\n#endpoint\nsh -c\n#command\nprintf '${name}-pid:%s\\n' $$; while :; do printf '${name}-tick\\n'; sleep 0.2; done`;
  const rest = name => `#check ${name}\n#endpoint\nGET ${fixtureUrl}/stop-slow?${name}`;
  const send = async text => {
    const textarea = testid('agent-composer');
    textarea.value = text;
    textarea.dispatchEvent(new InputEvent('input', {bubbles: true}));
    textarea.focus();
    await key('Enter');
  };
  await openConsole();
  await click(testid('console-service-terminal'));
  setSource(editor(), `${terminal('closingfirst')}\n\n${terminal('closingsecond')}`);
  await wait(() => checks().length === 2, 'Two Terminal checks');
  await select(0);
  await run();
  await wait(() => /closingfirst-pid:\d+/.test(source()), 'First Terminal check prints its PID');
  await select(1);
  await run();
  await wait(() => /closingsecond-pid:\d+/.test(source()), 'Second Terminal check prints its PID');
  const pids = [...source().matchAll(/closing(?:first|second)-pid:(\d+)/g)].map(match => Number(match[1]));
  await click(testid('console-service-rest'));
  setSource(editor(), `${rest('closingrest')}\n\n${rest('closingagent')}`);
  await wait(() => checks().length === 2, 'Two REST checks');
  await select(0);
  await run();
  await wait(() => status().classList.contains('is-running'), 'REST check running');
  if (!testid('console-agent-toggle').classList.contains('active')) await click(testid('console-agent-toggle'));
  await wait(() => document.querySelector('[data-testid="agent-composer"]'), 'Agent composer');
  // Same-origin windows share the stored Agent chat; each window needs its own Copilot session.
  const chat = () => testid('agent-sessions').querySelector('.active')?.dataset.sessionId;
  const previous = chat();
  await click(testid('agent-new-session'));
  await wait(() => chat() && chat() !== previous, 'New Agent chat for this window');
  await select(1);
  await send('agent stop');
  await wait(() => document.querySelector('[data-testid="agent-run-approval"]'),
    () => `Agent run approval; transcript: ${document.querySelector('[data-testid="agent-transcript"]')?.textContent}`);
  await select(0);
  return pids;
})()
