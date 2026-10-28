(async () => {
  const { assert, wait, sleep, click, testid, cm, choose, setSource, key, assertSignalGeometry } = testDriver;
  const editor = () => cm('console-editor');
  const source = () => editor().getValue();
  const status = () => document.querySelector('.console-status');
  const agentStatus = () => testid('agent-status');
  const light = element => element.querySelector('.console-state-light')?.dataset.state;
  const checks = () => [...testid('console-check-menu').querySelectorAll('button')];
  const select = async index => {
    await click(checks()[index]);
    await wait(() => checks()[index]?.classList.contains('active'),
      () => `Selecting check ${index + 1} did not update viewport ownership: ${JSON.stringify({
        checks: checks().map(check => ({line: check.dataset.line, active: check.classList.contains('active')})),
        scroll: editor().getScrollInfo(), cursor: editor().getCursor(),
        status: status().textContent, source: source().slice(-1800)
      })}`);
    editor().setCursor(Number(checks()[index].dataset.line) - 1, 0);
  };
  const run = async () => {
    editor().focus();
    await key('Shift+Enter');
  };
  const terminal = name => `#check ${name}\n#endpoint\nsh -c\n#command\nprintf '${name}-pid:%s\\n' $$; ${name === 'first' ? "printf '#check second\\n#endpoint\\nsh -c\\n#command\\nneighbor-copy\\n'; " : ""}while :; do printf '${name}-tick\\n'; sleep 0.2; done`;
  const rest = name => `#check ${name}\n#endpoint\nGET ${fixtureUrl}/stop-slow?${name}`;
  const count = async () => (await (await fetch(`${fixtureUrl}/stop-count`)).json()).count;
  const openAgent = async () => {
    if (!testid('console-agent-toggle').classList.contains('active')) {
      await click(testid('console-agent-toggle'));
    }
    await wait(() => document.querySelector('[data-testid="agent-composer"]'));
  };
  const ticks = name => (source().match(new RegExp(`${name}-tick`, 'g')) || []).length;
  const section = name => {
    const lines = source().split('\n');
    const starts = checks().map(check => Number(check.dataset.line) - 1);
    const index = starts.findIndex(line => lines[line] === `#check ${name}`);
    return index < 0 ? '' : lines.slice(starts[index] + 1, starts[index + 1] ?? lines.length).join('\n');
  };
  const stopped = name => /#responce [^\n]+ · Stopped/.test(section(name));
  await choose('console');
  await click(testid('console-service-terminal'));
  setSource(editor(), `${terminal('duplicate-valid')}\n\n${terminal('duplicate-invalid')}\n\n${terminal('cursor-third')}`);
  await wait(() => checks().length === 3);
  await select(0);
  await run();
  await wait(() => ticks('duplicate-valid') >= 2);
  editor().setCursor(Number(checks()[1].dataset.line) + 3, 0);
  editor().scrollTo(null, 0);
  await wait(() => checks()[1].classList.contains('active'),
    'A visible cursor in check 2 must control active status while check 1 remains at the top');
  await run();
  await wait(() => ticks('duplicate-invalid') >= 2 && status().textContent.includes('#check duplicate-invalid')
    && !testid('console-stop').disabled,
    ()=>`Second check did not keep its active request: ${JSON.stringify({status:status().textContent,source:source().slice(-3000),
      cursor:editor().getCursor(),scroll:editor().getScrollInfo(),editor:editor().getWrapperElement().getBoundingClientRect(),
      checks:checks().map(check=>({line:check.dataset.line,active:check.classList.contains('active')}))})}`);
  const rejectRepeatedRun = async name => {
    await run();
    await wait(() => status().classList.contains('is-error') && testid('console-stop').disabled
      && /#responce [^\n]+ · ERROR/.test(section(name)),
      'A repeated run must stop its process and report a persistent local error, not a successful stop or global banner');
    assert(status().textContent.includes('Repeated run rejected')
      && ![...document.querySelectorAll('.banner[role="alert"]')].some(el => el.textContent.includes('already running')),
      'Duplicate execution must report its error only in the owning check status');
    assert((section(name).match(new RegExp(`${name}-pid:\\d+`, 'g')) || []).length === 1,
      'Rejecting a repeated run must not start a replacement process');
    await click(status());
    const debug = testid('console-main-status-debug').textContent;
    assert(debug.includes('"phase": "duplicate-run"') && debug.includes('"previousOperation"')
      && debug.includes('"cancellationRequested": true'),
      'Local Debug must retain the duplicate-run cause and the real cancellation reply');
    await click(status());
    const finalTicks = ticks(name);
    await sleep(350);
    assert(ticks(name) === finalTicks && status().classList.contains('is-error'),
      'A rejected process must stop producing output and its cancellation reply must not overwrite the local error');
  };
  await select(0);
  await rejectRepeatedRun('duplicate-valid');
  const siblingTicks = ticks('duplicate-invalid');
  await wait(() => ticks('duplicate-invalid') > siblingTicks,
    'Cancelling a repeated run must preserve a concurrently running sibling check');
  await select(1);
  const endpointLine = Number(checks()[1].dataset.line) + 1;
  editor().replaceRange('', {line:endpointLine,ch:0}, {line:endpointLine,ch:5});
  await rejectRepeatedRun('duplicate-invalid');
  editor().replaceRange('sh -c', {line:endpointLine,ch:0});
  await select(1);
  await run();
  await wait(() => status().classList.contains('is-running') && ticks('duplicate-invalid') >= 2,
    () => `A fresh explicit run after cancellation must work without retaining the duplicate-run failure: ${JSON.stringify({
      status: status().textContent, cursor: editor().getCursor(),
      checks: checks().map(check => ({line:check.dataset.line,active:check.classList.contains('active')})), source: source()
    })}`);
  await click(testid('console-stop'));
  await wait(() => stopped('duplicate-invalid') && testid('console-stop').disabled);
  setSource(editor(), `${terminal('first')}\n\n${terminal('second')}`);
  await wait(() => checks().length === 2);
  await select(0);
  await run();
  await wait(() => ticks('first') >= 2,
    () => `First Terminal stream did not start: ${JSON.stringify({status: status().textContent, source: source()})}`);
  await select(1);
  await run();
  await wait(() => ticks('second') >= 2);
  assert(checks().length === 2 && checks().every(check => light(check) === 'running'),
    'Concurrent checks must keep blinking signals, including the viewed check, even when output copies a neighboring check');
  const runningLight = checks()[1].querySelector('.console-state-light');
  assert(matchMedia('(prefers-reduced-motion: reduce)').matches || runningLight.getAnimations()[0]?.playState === 'running',
    'A running check must have a genuinely animated green signal even while viewed');
  assertSignalGeometry();
  await select(0);
  assert(status().textContent.includes('#check first') && !testid('console-stop').disabled,
    'Status and shared Stop must follow the highlighted check');
  await click(testid('console-stop'));
  await wait(() => stopped('first') && testid('console-stop').disabled);
  assert(light(checks()[0]) === 'ready' && light(checks()[1]) === 'running'
    && light(testid('console-service-terminal')) === 'running',
  'A viewed stop must go dark without suppressing another running check or its service signal');
  const secondTicks = ticks('second');
  await wait(() => ticks('second') > secondTicks,
    'Stopping check 1 must leave the second Terminal process running');
  await select(1);
  assert(!testid('console-stop').disabled && status().textContent.includes('#check second'),
    'Selecting the second check must expose its own running status and Stop');
  await click(testid('console-stop'));
  await wait(() => stopped('second'));
  assert(checks().every(check => light(check) === 'ready')
    && light(testid('console-service-terminal')) === 'ready',
    'Viewing both stop results must acknowledge the aggregate without clearing either status');

  const timed = (name, code) => `#check ${name}\n#endpoint\nsh -c\n#command\nsleep 2; printf '${name}-finished'; exit ${code}`;
  setSource(editor(), `${timed('unseen-success', 0)}\n\n${timed('unseen-error', 1)}`);
  await wait(() => checks().length === 2);
  await select(0);
  await run();
  await wait(() => status().classList.contains('is-running'));
  await select(1);
  await run();
  await wait(() => status().classList.contains('is-running'));
  await click(testid('console-service-rest'));
  await wait(() => light(testid('console-service-terminal')) === 'error',
    'A background failure must light its service after concurrent requests finish');
  await click(testid('console-service-terminal'));
  const restored = checks().findLastIndex(check => Number(check.dataset.line) - 1 <= editor().getCursor().line);
  const remaining = 1 - restored;
  const unseenState = remaining === 0 ? 'success' : 'error';
  await wait(() => checks()[restored].classList.contains('active') && light(checks()[restored]) === 'ready');
  assert(light(checks()[remaining]) === unseenState
    && light(testid('console-service-terminal')) === unseenState,
    'Restoring the cursor must acknowledge only its displayed check, preserving the other unseen outcome');
  await select(remaining);
  assert(checks().every(check => light(check) === 'ready') && light(testid('console-service-terminal')) === 'ready',
    'Opening the remaining check must acknowledge the service without clearing either status');
  await select(1);
  assert(status().classList.contains('is-error') && checks().every(check => light(check) === 'ready')
    && light(testid('console-service-terminal')) === 'ready',
    'Opening the remaining failed check must acknowledge the service without clearing its error status');
  const failedStatus = status().textContent;
  await click(status());
  assert(testid('console-main-status-debug').textContent.includes('"code": 1'),
    'Acknowledging a failure must preserve its actual Debug diagnostics');
  await click(status());
  await select(0);
  assert(status().textContent.includes('OK') && checks().every(check => light(check) === 'ready')
    && light(testid('console-service-terminal')) === 'ready',
    'The service signal must go black only after all completed check outcomes have been viewed');
  await select(1);
  assert(status().textContent === failedStatus && light(checks()[1]) === 'ready',
    'Revisiting an acknowledged failure must restore the status without relighting its signal');
  await select(0);
  await run();
  await wait(() => status().classList.contains('is-running') && light(checks()[0]) === 'running');
  await click(testid('console-service-rest'));
  await wait(() => light(testid('console-service-terminal')) === 'success',
    'A new background run must relight a previously acknowledged check when it completes');
  await click(testid('console-service-terminal'));
  await wait(() => status().textContent.includes('OK') && light(testid('console-service-terminal')) === 'ready');

  setSource(editor(), terminal('background'));
  await run();
  await wait(() => ticks('background') >= 2);
  await click(testid('console-service-rest'));
  setSource(editor(), rest('ordinary'));
  await run();
  await wait(() => status().classList.contains('is-running'));
  await sleep(100);
  await click(testid('console-stop'));
  await wait(() => stopped('ordinary') && testid('console-stop').disabled);
  assert(source().includes('remote server may still finish'),
    'HTTP cancellation must explain the remote-processing limitation rather than claim rollback');
  await click(testid('console-service-terminal'));
  const backgroundTicks = ticks('background');
  await wait(() => ticks('background') > backgroundTicks,
    'Stopping a different service must not stop the background Terminal process');

  await click(testid('console-service-rest'));
  assert(light(testid('console-service-terminal')) === 'running',
    'The service tab must expose a background Terminal process');
  setSource(editor(), `${rest('manual')}\n\n${rest('agentchild')}\n\n#check success\n#endpoint\nGET ${fixtureUrl}/status-success\n\n#check failure\n#endpoint\nGET`);
  await wait(() => checks().length === 4);
  await select(2);
  await run();
  await wait(() => status().textContent.includes('OK'));
  const successfulStatus = status().textContent;
  assert(light(checks()[2]) === 'ready', 'A completed successful check that is already visible must have a black signal');
  await select(3);
  await run();
  await wait(() => status().classList.contains('is-error'));
  assert(light(checks()[3]) === 'ready' && light(testid('console-service-rest')) === 'ready',
    'A visible validation failure must keep its error status but acknowledge both check and service signals');
  await select(2);
  assert(status().textContent === successfulStatus && light(checks()[2]) === 'ready',
    'Selecting a completed check must restore its own status while its acknowledged signal stays black');
  const beforeConcurrent = await count();
  await select(0);
  await run();
  await wait(() => status().classList.contains('is-running'));
  assert(light(testid('console-service-rest')) === 'running'
    && testid('console-service-rest').querySelector('.console-state-light').title.includes('0 errors'),
  'The service signal must prioritize running without counting a previously viewed error as unseen');
  await sleep(300);
  await select(1);
  await openAgent();
  const send = async text => {
    const textarea = testid('agent-composer');
    textarea.value = text;
    textarea.dispatchEvent(new InputEvent('input', {bubbles: true}));
    textarea.focus();
    await key('Shift+Enter');
  };
  await send('agent run');
  await wait(() => document.querySelector('[data-testid="agent-run-approval"]'),
    'An Agent must be able to request another check while the user query is running');
  assert(agentStatus().classList.contains('is-warning')
    && light(testid('agent-sessions').querySelector('[aria-selected="true"]')) === 'ready',
  'Viewed approval must remain yellow in the Agent status without an unread chat signal');
  await select(0);
  await click(testid('agent-approve-run'));
  await wait(async () => await count() >= beforeConcurrent + 2,
    'Both the ordinary and approved Agent requests must actually reach the service before Stop');
  await wait(() => status().classList.contains('is-running') && agentStatus().classList.contains('is-running'));
  await sleep(150);
  const request = status();
  const agent = agentStatus();
  const requestTime = Number.parseInt(request.querySelector('.console-status-elapsed').textContent, 10);
  const agentTime = Number.parseInt(agent.querySelector('.console-status-elapsed').textContent, 10);
  assert(requestTime > agentTime && request.textContent.includes('#check manual')
    && !request.textContent.includes('Agent') && agent.textContent.includes('Agent'),
    `Main and Agent statuses must retain separate identities and clocks: ${JSON.stringify({request: request.textContent, agent: agent.textContent, requestTime, agentTime})}`);
  await click(status());
  await click(agentStatus());
  assert(testid('console-main-status-debug').textContent.includes('/stop-slow?manual')
    && !testid('console-main-status-debug').textContent.includes('sessionId')
    && testid('agent-status-debug').textContent.includes('sessionId'),
  'Opening both Debug panels must show independent check and Agent diagnostics');
  const mainBounds = status().getBoundingClientRect();
  const agentBounds = agentStatus().getBoundingClientRect();
  assert(Math.abs(mainBounds.top - agentBounds.top) < 1 && mainBounds.right <= agentBounds.left,
    'Both independent status strips must align underneath their own editor columns');
  await click(status());
  await click(agentStatus());
  await select(2);
  assert(status().textContent === successfulStatus && testid('console-stop').disabled
    && agentStatus().classList.contains('is-running'),
  'Switching to a successful check must restore its status while the independent Agent remains running');
  await select(0);
  await click(testid('console-stop'));
  await wait(() => stopped('manual') && testid('console-stop').disabled);
  assert(!stopped('agentchild') && !testid('agent-stop').disabled && light(checks()[1]) === 'running',
    'The main S must stop only its check, preserving the Agent and its approved child request');
  await click(testid('console-sessions-toggle'));
  await click(testid('session-switch-0'));
  await wait(()=>testid('console-sessions-status').textContent.includes('ERROR'));
  assert(testid('console-session-name').dataset.sessionId==='1',
    'Console sessions must not switch while a global Agent turn or its child is running');
  await openAgent();
  await select(0);
  await run();
  await wait(async () => await count() >= beforeConcurrent + 3 && status().classList.contains('is-running'));
  await click(testid('agent-stop'));
  await wait(() => stopped('agentchild') && testid('agent-stop').disabled);
  assert(status().classList.contains('is-running') && !testid('console-stop').disabled,
    'The Agent S must stop that chat and its child without stopping a concurrent manual request');
  await click(testid('console-stop'));
  await wait(() => stopped('manual') && testid('console-stop').disabled);
  assert(!document.querySelector('[data-testid="agent-run-approval"]'),
    'Stopping the owning Agent must revoke its approvals');
  const agentDebugText = () => document.querySelector('[data-testid="agent-status-debug"]').textContent;
  await send('agent hold first');
  await wait(() => agentDebugText().includes('Fixture is holding'));
  assert(!testid('agent-transcript').textContent.includes('Fixture is holding'),
    'Running commentary must stay in Debug, not the chat transcript');
  const firstChat = testid('agent-sessions').querySelector('[aria-selected="true"]').dataset.sessionId;
  await click(testid('agent-new-session'));
  await send('agent hold second');
  await wait(() => agentDebugText().includes('Fixture is holding'));
  const secondChat = testid('agent-sessions').querySelector('[aria-selected="true"]').dataset.sessionId;
  const chatTab = id => testid('agent-sessions').querySelector(`[data-session-id="${id}"]`);
  assert(light(chatTab(firstChat)) === 'running' && light(chatTab(secondChat)) === 'running',
    'Two concurrently running chats must keep separate blinking signals even while selected');
  assertSignalGeometry();
  await click(testid('agent-stop'));
  await wait(() => testid('agent-stop').disabled);
  assert(light(chatTab(firstChat)) === 'running' && light(chatTab(secondChat)) === 'ready'
    && light(testid('console-agent-toggle')) === 'running',
  'Stopping the selected chat must preserve another running chat and its aggregate tool indicator');
  await click(chatTab(firstChat));
  await wait(() => !testid('agent-stop').disabled && agentStatus().classList.contains('is-running'));
  await click(testid('agent-stop'));
  await wait(() => testid('agent-stop').disabled);
  await send('agent failure');
  await wait(() => agentStatus().classList.contains('is-error'));
  assert(light(chatTab(firstChat)) === 'ready' && light(chatTab(secondChat)) === 'ready'
    && light(testid('console-agent-toggle')) === 'ready',
  'A viewed Agent failure must keep its error status without relighting acknowledged chat/tool signals');
  const failedAgentStatus = agentStatus().textContent;
  await click(chatTab(secondChat));
  await wait(() => agentStatus().textContent.includes('Stopped'));
  await click(chatTab(firstChat));
  await wait(() => agentStatus().textContent === failedAgentStatus);
  assert(light(chatTab(firstChat)) === 'ready' && agentStatus().classList.contains('is-error'),
    'Revisiting an acknowledged chat must preserve its failure without a new signal');
  await send('agent markdown');
  await wait(() => testid('agent-transcript').textContent.includes('Markdown ready') && testid('agent-stop').disabled);
  assert(light(chatTab(firstChat)) === 'ready',
    'A successful subsequent Agent turn must retain its visible status with a black acknowledged signal');
  const transcript=testid('agent-transcript');
  testid('agent-composer').focus({preventScroll:true});
  transcript.scrollTop=Math.max(0,(transcript.scrollHeight-transcript.clientHeight)/3);
  await sleep(80);
  const debugGeometry=()=>({
    leftEditorHeight:editor().getWrapperElement().getBoundingClientRect().height,
    leftStatusTop:status().getBoundingClientRect().top,
    rightHeight:transcript.clientHeight,
    rightScroll:transcript.scrollTop,
    rightStatusTop:agentStatus().getBoundingClientRect().top,
    pageScroll:document.scrollingElement.scrollTop,
    oldTop:testid('console-old').getBoundingClientRect().top,
  });
  const bothClosed=debugGeometry();
  await click(status());
  await sleep(100);
  const leftOpen=debugGeometry();
  await click(agentStatus());
  await sleep(100);
  const bothOpen=debugGeometry();
  const mainDebug=testid('console-main-status-debug');
  const agentDebug=testid('agent-status-debug');
  mainDebug.style.height='100px';
  await sleep(100);
  const leftResized=debugGeometry();
  agentDebug.style.height='80px';
  await sleep(100);
  const rightResized=debugGeometry();
  mainDebug.style.removeProperty('height');
  agentDebug.style.removeProperty('height');
  await sleep(100);
  await click(status());
  await sleep(100);
  const rightOpen=debugGeometry();
  await click(agentStatus());
  await sleep(100);
  const closedAgain=debugGeometry();
  transcript.scrollTop=transcript.scrollHeight;
  await sleep(80);
  const bottomClosed=debugGeometry();
  await click(agentStatus());
  await sleep(100);
  const bottomOpen=debugGeometry();
  await click(agentStatus());
  await sleep(100);
  const near=(a,b)=>Math.abs(a-b)<=1;
  assert(near(leftOpen.rightHeight,bothClosed.rightHeight)
    &&near(leftOpen.rightStatusTop,bothClosed.rightStatusTop)
    &&near(leftOpen.rightScroll,bothClosed.rightScroll)
    &&near(rightOpen.leftEditorHeight,bothClosed.leftEditorHeight)
    &&near(rightOpen.leftStatusTop,bothClosed.leftStatusTop),
    `Debug expansion must affect only its own column: ${JSON.stringify({bothClosed,leftOpen,bothOpen,rightOpen,closedAgain,bottomClosed,bottomOpen})}`);
  assert([leftOpen,bothOpen,rightOpen,closedAgain].every(value=>near(value.rightScroll,bothClosed.rightScroll)
    &&near(value.pageScroll,bothClosed.pageScroll)&&near(value.oldTop,value.leftStatusTop)),
    `Debug must not scroll Agent history/page to reveal elements, or detach O from the left status: ${JSON.stringify({bothClosed,leftOpen,bothOpen,rightOpen,closedAgain})}`);
  assert(leftResized.leftEditorHeight>bothOpen.leftEditorHeight
    &&near(leftResized.rightHeight,bothOpen.rightHeight)
    &&rightResized.rightHeight>leftResized.rightHeight
    &&near(rightResized.leftEditorHeight,leftResized.leftEditorHeight)
    &&[leftResized,rightResized].every(value=>near(value.rightScroll,bothClosed.rightScroll)
      &&near(value.pageScroll,bothClosed.pageScroll)&&near(value.oldTop,value.leftStatusTop)),
    `Resizing either Debug must keep the other column and Agent reading position unchanged: ${JSON.stringify({bothOpen,leftResized,rightResized})}`);
  assert(near(bottomOpen.rightScroll,bottomClosed.rightScroll)&&near(bottomOpen.pageScroll,bottomClosed.pageScroll),
    `Opening Debug must not automatically scroll even when Agent history was at the bottom: ${JSON.stringify({bottomClosed,bottomOpen})}`);
  await send('agent hold hidden first');
  await wait(() => agentStatus().classList.contains('is-running'));
  await click(testid('agent-new-session'));
  await send('agent hold hidden second');
  await wait(() => agentStatus().classList.contains('is-running'));
  const hiddenChat = testid('agent-sessions').querySelector('[aria-selected="true"]').dataset.sessionId;
  await click(testid('console-service-terminal'));
  const afterAgentStop = ticks('background');
  await wait(() => ticks('background') > afterAgentStop,
    'Stopping the Agent and user request must still preserve an unrelated background service');
  window.dispatchEvent(new Event('services-console-hidden'));
  await wait(() => stopped('background') && testid('agent-stop').disabled
    && light(testid('console-agent-toggle')) === 'warning');
  window.dispatchEvent(new Event('services-console-shown'));
  await wait(() => light(chatTab(hiddenChat)) === 'ready' && light(chatTab(firstChat)) === 'warning');
  assert(light(testid('console-agent-toggle')) === 'warning',
    'Showing the selected stopped chat must not acknowledge another chat that completed while hidden');
  await click(chatTab(firstChat));
  await wait(() => agentStatus().textContent.includes('Stopped')
    && light(testid('console-agent-toggle')) === 'ready');
  assert(light(chatTab(firstChat)) === 'ready' && light(chatTab(hiddenChat)) === 'ready',
    'The Agent tool must go dark after both stopped chat outcomes are viewed, without erasing their statuses');
  setSource(editor(), `${terminal('hiddenfirst')}\n\n${terminal('hiddensecond')}`);
  await wait(() => checks().length === 2);
  await select(0);
  await run();
  await wait(() => ticks('hiddenfirst') >= 2);
  await select(1);
  await run();
  await wait(() => ticks('hiddensecond') >= 2);
  await click(testid('console-service-rest'));
  setSource(editor(), `${rest('hiddenrest')}\n\n${rest('hiddenagent')}`);
  await wait(() => checks().length === 2);
  await select(0);
  await run();
  await wait(() => status().classList.contains('is-running'));
  await openAgent();
  await select(1);
  await send('agent stop');
  await wait(() => document.querySelector('[data-testid="agent-run-approval"]'));
  await select(0);
  window.dispatchEvent(new Event('services-console-hidden'));
  await wait(() => stopped('hiddenrest') && testid('agent-stop').disabled);
  window.dispatchEvent(new Event('services-console-shown'));
  await click(testid('console-service-terminal'));
  await wait(() => stopped('hiddenfirst') && stopped('hiddensecond'));
  setSource(editor(), '#check after-show\n#endpoint\nsh -c\n#command\nprintf after-show-ready');
  await run();
  await wait(() => source().includes('#responce') && source().includes('after-show-ready')
    && testid('console-stop').disabled,
  'Showing the panel again must allow fresh operations after stopping all hidden work');

  globalThis.startClosingStopFixture = async () => {
    await choose('console');
    await click(testid('console-service-terminal'));
    setSource(editor(), `${terminal('closingfirst')}\n\n${terminal('closingsecond')}`);
    await wait(() => checks().length === 2);
    await select(0);
    await run();
    await wait(() => /closingfirst-pid:\d+/.test(source()));
    await select(1);
    await run();
    await wait(() => /closingsecond-pid:\d+/.test(source()));
    const pids = [...source().matchAll(/closing(?:first|second)-pid:(\d+)/g)].map(match => Number(match[1]));
    await click(testid('console-service-rest'));
    setSource(editor(), `${rest('closingrest')}\n\n${rest('closingagent')}`);
    await wait(() => checks().length === 2);
    await select(0);
    await run();
    await wait(() => status().classList.contains('is-running'));
    await openAgent();
    await select(1);
    await send('agent stop');
    await wait(() => document.querySelector('[data-testid="agent-run-approval"]'));
    await select(0);
    return pids;
  };
  return true;
})()
