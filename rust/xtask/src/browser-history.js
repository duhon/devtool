(async()=>{
  const {assert,wait,click,testid,cm,setSource,key,openConsole,activateSession,visible}=testDriver;
  await openConsole();
  await click(testid('console-service-rest'));
  const editor=cm('console-editor');
  setSource(editor,`#check History acceptance\n#endpoint\nPUT ${fixtureUrl}/history-fixture?value={{env:SERVICES_DEVTOOLS_ENV_PROBE}}\n#headers\nContent-Type: application/json\nAuthorization: Bearer history-secret-token\n#body\n{"text":"first","env":"{{env:SERVICES_DEVTOOLS_ENV_PROBE}}"}`);
  const current=Number(testid('console-session-name').dataset.sessionId);
  const snapshot=async()=>(await testDriver.sessionLibrary()).sessions.find(s=>s.id===current).snapshot.console;
  await wait(async()=>(await snapshot()).checkIdentities?.rest?.length===1);
  const identity=(await snapshot()).checkIdentities.rest[0].id;
  const workspace=(await snapshot()).workspaceId;
  const run=async()=>{
    editor.closeHint();editor.focus();editor.setCursor(7,5);
    const before=editor.getValue();
    await key('Shift+Enter');
    await wait(()=>editor.getValue()!==before && !testid('console-main-status').classList.contains('is-running')
      && testid('console-main-status').textContent.includes('OK'),
      ()=>`History request did not complete: ${testid('console-main-status').textContent}`);
    assert((await snapshot()).checkIdentities.rest[0].id===identity,'A response insertion must not change the check ID');
  };
  await run();await run();
  const line=editor.getLine(7),from=line.indexOf('first');
  editor.replaceRange('third',{line:7,ch:from},{line:7,ch:from+5},'+input');
  await run();
  const topBar=()=>document.querySelector('.console-top-bar');
  const initialHeaderHeight=topBar().getBoundingClientRect().height;
  const open=async()=>{
    if(testid('console-history-toggle').getAttribute('aria-pressed')!=='true') await click(testid('console-history-toggle'));
    await wait(()=>testid('history-panel') && document.querySelector('[data-testid="history-title"]'));
    const height=topBar().getBoundingClientRect().height;
    assert(Math.abs(height-initialHeaderHeight)<0.5,
      `Opening History must not increase the shared service header height: ${initialHeaderHeight} -> ${height}`);
    assert(testid('history-title').classList.contains('tab-button')
      && testid('history-title').classList.contains('active'),
      'History must use the same compact active heading style as other panels');
    assert(getComputedStyle(testid('history-title')).backgroundColor
      ===getComputedStyle(topBar().querySelector('.console-tab-strip > .tab-button.active')).backgroundColor,
      'History must have the same visible active background as the service tabs');
  };
  await open();
  await wait(()=>document.querySelector('[data-testid="history-entries"]')?.querySelectorAll('[data-testid="history-entry"]').length===3,
    ()=>`History must contain three executions: ${testid('history-panel').textContent}; ${testid('history-status').textContent}`);
  assert(testid('history-panel').dataset.checkId===identity,'History must follow the persisted active check ID');
  const entries=()=>[...testid('history-entries').querySelectorAll('[data-testid="history-entry"]')];
  assert(!document.querySelector('[data-testid="history-editor"]'),
    'History must not mount an empty editor before an execution is selected');
  assert(getComputedStyle(testid('history-entries')).maxHeight==='none',
    'The collapsed History list must not reserve snapshot space');
  assert(!document.querySelector('[data-testid="history-refresh"]')
    && testid('history-title').tagName==='SPAN',
    'History must have a non-interactive active heading without a Refresh button');
  await click(testid('console-history-toggle'));
  await run();
  assert(document.querySelector('[data-testid="history-entries"]').querySelectorAll('[data-testid="history-entry"]').length===3,
    'The hidden History list must not be reloaded by background execution notifications');
  await open();
  await wait(()=>document.querySelector('[data-testid="history-entries"]')?.querySelectorAll('[data-testid="history-entry"]').length===4,
    'Opening History must reread the database even when automatic updates did not refresh the hidden list');
  for(let count=5;count<=10;count++){
    await run();
    await wait(()=>entries().length===count && !testid('history-status').classList.contains('is-running'));
  }
  const detail=async index=>{
    const id=entries()[index].dataset.entryId;
    await click(entries()[index]);
    await wait(()=>{
      const element=document.querySelector('[data-testid="history-editor"]');
      const snapshotEditor=element?.CodeMirror || element?.nextElementSibling?.CodeMirror;
      return snapshotEditor?.getValue().includes('#endpoint') && !testid('history-status').classList.contains('is-running');
    });
    assert(entries()[index].dataset.entryId===id,'Opening a snapshot must retain its execution identity');
    assert(entries()[index].getAttribute('aria-expanded')==='true','The selected history record must be expanded');
    const list=testid('history-entries'),row=entries()[index].getBoundingClientRect();
    const top=list.getBoundingClientRect().top+list.clientTop;
    assert(row.top>=top-0.5 && row.bottom<=top+list.clientHeight+0.5,
      `The selected History entry must remain fully visible after the list shrinks: ${row.top}..${row.bottom}, list ${top}..${top+list.clientHeight}`);
    assert(entries()[index].getAttribute('aria-current')==='true'
      && list.querySelectorAll('.history-entry.active').length===1,
      'Exactly the displayed execution must have the visible and accessible current-entry state');
    return cm('history-editor').getValue();
  };
  const historyPanel=testid('history-panel'),list=testid('history-entries');
  historyPanel.style.maxHeight='400px';
  await wait(()=>list.clientHeight>=list.scrollHeight);
  const unsplitHeight=list.clientHeight;
  const sourceViewport=editor.getScrollInfo();
  const pageViewport={x:window.scrollX,y:window.scrollY};
  const oldestId=entries()[9].dataset.entryId;
  const oldest=await detail(9);
  assert(list.clientHeight<unsplitHeight && list.scrollHeight>list.clientHeight && list.scrollTop>0,
    'Opening the bottom execution must reveal it inside the newly split, overflowing History list');
  const activeStyle=getComputedStyle(entries()[9]),inactiveStyle=getComputedStyle(entries()[0]);
  assert(activeStyle.backgroundColor!==inactiveStyle.backgroundColor
    && activeStyle.backgroundColor===getComputedStyle(testid('history-title')).backgroundColor
    && activeStyle.boxShadow!=='none',
    'The open execution must retain a clearly distinct active background and left indicator, including while hovered');
  assert(editor.getScrollInfo().top===sourceViewport.top && editor.getScrollInfo().left===sourceViewport.left
    && window.scrollX===pageViewport.x && window.scrollY===pageViewport.y,
    'Revealing a History entry must scroll only its list, not the source editor or page');
  list.scrollTop=0;
  await click(testid('console-templates-toggle'));
  await open();
  await wait(()=>{
    const row=entries().find(entry=>entry.dataset.entryId===oldestId)?.getBoundingClientRect();
    const top=list.getBoundingClientRect().top+list.clientTop;
    return row && row.top>=top-0.5 && row.bottom<=top+list.clientHeight+0.5
      && !testid('history-status').classList.contains('is-running');
  },'Reopening History must reveal the retained current execution');
  assert(cm('history-editor').getValue()===oldest,'Reopening History must preserve the selected snapshot');
  await click(entries()[9]);
  await wait(()=>!document.querySelector('[data-testid="history-editor"]'));
  assert(!list.querySelector('.history-entry.active') && !list.querySelector('[aria-current]'),
    'Collapsing the current execution must remove its active indicator');
  assert(await detail(9)===oldest,'The oldest execution must remain visible and immutable when reopened');
  historyPanel.style.removeProperty('max-height');
  const latest=await detail(0);
  assert(latest.includes('third') && latest.includes('companion-fixture-only') && !latest.includes('{{env:'),
    `History must show full resolved sections/body rather than current variable references: ${latest}`);
  assert(!latest.includes('history-secret-token'),'History must not persist exposed Authorization secrets');
  assert(/#respon[cs]e/.test(latest),'History snapshot must include the recorded response');
  const expectedHeader=`\n#responce ${new Date(Number(entries()[0].dataset.endedAt)).toLocaleTimeString('en-US')} · OK\n`;
  assert(latest.includes(expectedHeader),'History must show the recorded completion time and Console response marker/outcome');
  const recordedResponse=latest.split(expectedHeader)[1];
  assert(recordedResponse===JSON.stringify(JSON.parse(recordedResponse),null,2),
    'History must display JSON using the same indentation as the main check, without transport metadata');
  assert(!latest.includes('\n#effective-parameters\n') && !recordedResponse.includes('"durationMs"'),
    'History must not add effective parameters or response transport/debug wrappers');
  assert(cm('history-editor').getOption('readOnly')===true,'History snapshots must be read-only');
  await click(entries()[0]);
  await wait(()=>!document.querySelector('[data-testid="history-editor"]'));
  assert(entries()[0].getAttribute('aria-expanded')==='false'
    && !testid('history-panel').classList.contains('has-selection')
    && !testid('history-status').classList.contains('is-running'),
    'Clicking the same record must collapse its snapshot and release the detail area');
  assert(await detail(0)===latest,'A collapsed history record must reopen its original immutable snapshot');
  const second=await detail(8),first=await detail(9);
  assert(first.includes('first') && !first.includes('"text":"third"') && first!==second,
    'Earlier requests and different server responses must remain immutable');
  editor.replaceRange('History renamed',{line:0,ch:7},{line:0,ch:editor.getLine(0).length},'+input');
  await wait(async()=>(await snapshot()).documents.rest.startsWith('#check History renamed'));
  assert((await snapshot()).checkIdentities.rest[0].id===identity,'Renaming a check must retain all its history');
  await click(testid('console-sessions-toggle'));
  await wait(()=>testid('console-sessions'));
  const input=testid('session-new-name');input.value='History copy';input.dispatchEvent(new InputEvent('input',{bubbles:true}));
  await click(testid(`session-copy-${current}`));
  await wait(()=>Number(testid('console-session-name').dataset.sessionId)!==current);
  await click(testid('console-service-rest'));
  await open();
  await wait(()=>!testid('history-status').classList.contains('is-running'));
  assert(testid('history-panel').dataset.checkId!==identity
    && testid('history-entries').querySelectorAll('[data-testid="history-entry"]').length===0,
    'Copying a Console session must create new check identities without inheriting execution history');
  assert(!document.querySelector('[data-testid="history-editor"]'),'An empty history must not show an editor');
  const copiedEditor=cm('console-editor');
  await activateSession(current);
  await wait(()=>!copiedEditor.getWrapperElement().isConnected);
  await click(testid('console-service-rest'));await open();
  await wait(()=>document.querySelector('[data-testid="history-entries"]')?.querySelectorAll('[data-testid="history-entry"]').length===10);
  assert((await snapshot()).workspaceId===workspace && testid('history-panel').dataset.checkId===identity,
    'Switching sessions must restore persisted workspace and check IDs');
  await click(testid('console-templates-toggle'));
  assert(!visible(document.querySelector('[data-testid="history-panel"]')),'Another right panel must completely hide History');
  await open();
  assert(!visible(document.querySelector('[data-testid="templates-panel"]')),'History must exclusively own the right pane');
  await click(testid('console-service-terminal'));
  const terminalEditor=cm('console-editor');
  const terminalOutput='+----+-------+\n| ID | Title |\n+----+-------+\n| 1  | Ready |\n+----+-------+\n';
  setSource(terminalEditor,"#check Terminal history\n#endpoint\nsh -c\n#command\nprintf '+----+-------+\\n| ID | Title |\\n+----+-------+\\n| 1  | Ready |\\n+----+-------+\\n'; printf 'warning\\n' >&2");
  terminalEditor.setCursor(terminalEditor.lastLine(),terminalEditor.getLine(terminalEditor.lastLine()).length);
  await key('Shift+Enter');
  await wait(()=>terminalEditor.getValue().includes(terminalOutput+'warning\n')
    && !testid('console-main-status').classList.contains('is-running'),
    ()=>`Terminal execution did not render plain output: ${terminalEditor.getValue()}`);
  assert(testid('console-main-status').textContent.includes('WARNING · warning'),
    `A Terminal command that exits 0 after writing to stderr must be a WARNING showing that line: ${testid('console-main-status').textContent}`);
  await wait(()=>document.querySelector('[data-testid="history-entries"]')?.querySelectorAll('[data-testid="history-entry"]').length===1
    && !testid('history-status').classList.contains('is-running'));
  const terminalSnapshot=await detail(0);
  assert(terminalSnapshot.endsWith(terminalOutput+'warning\n'),
    'Terminal History must match the main check output, joining stdout/stderr without added text');
  assert(!terminalSnapshot.includes('#terminal-output') && !terminalSnapshot.includes('"type":"stdout"')
    && !terminalSnapshot.includes('"durationMs"'),
    'Terminal History must hide stream-event and completion-metadata envelopes');
  await click(testid('console-service-rest'));
  await wait(()=>document.querySelector('[data-testid="history-entries"]')?.querySelectorAll('[data-testid="history-entry"]').length===10);
  const largeEditor=cm('console-editor');
  setSource(largeEditor,`#check Large History\n#endpoint\nGET ${fixtureUrl}/history-large-fixture`);
  largeEditor.closeHint();largeEditor.focus();largeEditor.setCursor(2,0);
  await key('Shift+Enter');
  await wait(()=>largeEditor.getValue().includes('response truncated')
    && !testid('console-main-status').classList.contains('is-running')
    && testid('console-main-status').textContent.includes('OK'),
    ()=>`Large History fixture did not complete: ${testid('console-main-status').textContent}; ${[...largeEditor.getValue()].slice(-200).join('')}`);
  await wait(()=>entries().length===1 && !testid('history-status').classList.contains('is-running'));
  const largeSnapshot=await detail(0);
  assert(!document.querySelector('[data-testid="history-more-text"]'),
    'History must load all snapshot chunks automatically without a Load more snapshot text button');
  assert(new TextEncoder().encode(largeSnapshot).length>3*128*1024,
    'The large History regression must span at least four native UTF-8 snapshot chunks');
  const largeResponse=JSON.parse(largeSnapshot.split(/\n#responce[^\n]*\n/)[1]);
  assert(largeResponse.tail==='history-auto-loaded-end' && largeResponse.rows.length===3000
    && largeResponse.rows.every((row,index)=>row.id===index && row.text==='chunk-é-🦀 '.repeat(10)),
    'Automatic snapshot loading must preserve every row, UTF-8 boundary and final response bytes');
  await click(testid('console-history-toggle'));
  return true;
})()
