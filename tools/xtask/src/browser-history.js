(async()=>{
  const {assert,wait,click,testid,editorOf,setSource,key,openConsole,activateSession,visible}=testDriver;
  await openConsole();
  await click(testid('console-service-rest'));
  const editor=editorOf('console-editor');
  setSource(editor,`#check History acceptance\n#target\n${fixtureUrl}/history-fixture?value={{env:SERVICES_DEVTOOLS_ENV_PROBE}}\n#action\nPUT\n#headers\nContent-Type: application/json\nAuthorization: Bearer history-secret-token\n#payload\n{"text":"first","env":"{{env:SERVICES_DEVTOOLS_ENV_PROBE}}"}`);
  const current=Number(testid('console-session-name').dataset.sessionId);
  const snapshot=async()=>(await testDriver.sessionLibrary()).sessions.find(s=>s.id===current).snapshot.console;
  await wait(async()=>(await snapshot()).checkIdentities?.rest?.length===1);
  const identity=(await snapshot()).checkIdentities.rest[0].id;
  const workspace=(await snapshot()).workspaceId;
  const run=async()=>{
    editor.closeHint();editor.focus();editor.setCursor(9,5);
    const before=editor.getValue();
    await key('Shift+Enter');
    await wait(()=>editor.getValue()!==before && !testid('console-main-status').classList.contains('is-running')
      && testid('console-main-status').textContent.includes('OK'),
      ()=>`History request did not complete: ${testid('console-main-status').textContent}`);
    assert((await snapshot()).checkIdentities.rest[0].id===identity,'A response insertion must not change the check ID');
  };
  await run();await run();
  const line=editor.getLine(9),from=line.indexOf('first');
  editor.replaceRange('third',{line:9,ch:from},{line:9,ch:from+5},'+input');
  await run();
  const topBar=()=>document.querySelector('.console-top-bar');
  const initialHeaderHeight=topBar().getBoundingClientRect().height;
  const panel=()=>document.querySelector('[data-testid="history-panel"]');
  const title=()=>panel()?.querySelector('.ui-panel-title .tab-button');
  const idle=()=>testid('console-history-toggle').querySelector('.indicator-light')?.dataset.state!=='running';
  const list=()=>document.querySelector('[data-testid="history-entries"]');
  const entries=()=>[...(list()?.querySelectorAll('[data-testid^="history-entry-"]')??[])];
  const entryId=entry=>entry.dataset.testid.slice('history-entry-'.length);
  const viewer=()=>testDriver.editorAt('history-editor');
  const open=async()=>{
    if(testid('console-history-toggle').getAttribute('aria-pressed')!=='true') await click(testid('console-history-toggle'));
    await wait(()=>title());
    const height=topBar().getBoundingClientRect().height;
    assert(Math.abs(height-initialHeaderHeight)<0.5,
      `Opening History must not increase the shared service header height: ${initialHeaderHeight} -> ${height}`);
    assert(title().textContent==='History'
      && getComputedStyle(title()).backgroundColor
        ===getComputedStyle(topBar().querySelector('.console-tab-strip > .tab-button.active')).backgroundColor,
      'History must have the same visible active heading as the service tabs');
  };
  await open();
  await wait(()=>entries().length===3,
    ()=>`History must contain three executions: ${panel()?.textContent}`);
  assert(!viewer(),'History must not mount an empty editor before an execution is selected');
  assert(![...panel().querySelectorAll('button')].some(button=>/refresh/i.test(button.textContent)),
    'History must have no Refresh button');
  await click(testid('console-history-toggle'));
  await run();
  await open();
  await wait(()=>entries().length===4,
    'Opening History must reread the database: a closed panel hears of no executions');
  for(let count=5;count<=10;count++){
    await run();
    await wait(()=>entries().length===count && idle(),
      ()=>`An open History must list each new execution: ${entries().length} of ${count}`);
  }
  const geometry=()=>entries().map(entry=>{
    const box=entry.getBoundingClientRect();
    return [box.left,box.top,box.width,box.height].map(value=>Math.round(value*100)).join(':');
  }).join(' ');
  const detail=async index=>{
    const id=entryId(entries()[index]);
    const shape=geometry();
    await click(entries()[index]);
    await wait(()=>viewer()?.getValue().includes('#target') && idle());
    assert(entryId(entries()[index])===id,'Opening a snapshot must retain its execution identity');
    assert(entries()[index].classList.contains('is-primary')
      && list().querySelectorAll('.is-primary').length===1,
      'Exactly the displayed execution must be marked current');
    assert(shape===geometry(),
      `Selecting a record must not move or resize any row: ${shape} => ${geometry()}`);
    return viewer().getValue();
  };
  const sourceViewport=editor.getScrollInfo();
  const pageViewport={x:window.scrollX,y:window.scrollY};
  const oldestId=entryId(entries()[9]);
  const oldest=await detail(9);
  assert(editor.getScrollInfo().top===sourceViewport.top && editor.getScrollInfo().left===sourceViewport.left
    && window.scrollX===pageViewport.x && window.scrollY===pageViewport.y,
    'Opening a History entry must not scroll the source editor or page');
  await click(testid('console-templates-toggle'));
  await open();
  await wait(()=>entries().find(entry=>entryId(entry)===oldestId)?.classList.contains('is-primary')
    && viewer()?.getValue()===oldest && idle(),
    'Reopening History must keep the selected execution and its snapshot');
  await click(entries()[9]);
  await wait(()=>!viewer());
  assert(!list().querySelector('.is-primary'),'Collapsing the current execution must remove its current mark');
  assert(await detail(9)===oldest,'The oldest execution must remain visible and immutable when reopened');
  const latest=await detail(0);
  assert(latest.includes('third') && latest.includes('companion-fixture-only') && !latest.includes('{{env:'),
    `History must show full resolved sections/body rather than current variable references: ${latest}`);
  assert(!latest.includes('history-secret-token'),'History must not persist exposed Authorization secrets');
  const header=latest.match(/\n#responce [^\n]+ · OK\n/)?.[0];
  assert(header,'History must show the recorded completion time and Console response marker/outcome');
  const recordedResponse=latest.split(header)[1];
  assert(recordedResponse===JSON.stringify(JSON.parse(recordedResponse),null,2),
    'History must display JSON using the same indentation as the main check, without transport metadata');
  assert(!latest.includes('\n#effective-parameters\n') && !recordedResponse.includes('"durationMs"'),
    'History must not add effective parameters or response transport/debug wrappers');
  assert(viewer().getOption('readOnly')===true,'History snapshots must be read-only');
  await click(entries()[0]);
  await wait(()=>!viewer() && idle(),'Clicking the same record must collapse its snapshot');
  assert(await detail(0)===latest,'A collapsed history record must reopen its original immutable snapshot');
  const second=await detail(8),first=await detail(9);
  assert(first.includes('first') && !first.includes('"text":"third"') && first!==second,
    'Earlier requests and different server responses must remain immutable');
  editor.replaceRange('History renamed',{line:0,ch:7},{line:0,ch:editor.getLine(0).length},'+input');
  await wait(async()=>(await snapshot()).documents.rest.startsWith('#check History renamed'));
  assert((await snapshot()).checkIdentities.rest[0].id===identity,'Renaming a check must retain all its history');
  await click(testid('console-sessions-toggle'));
  await click(await wait(()=>document.querySelector(`[data-testid="session-copy-${current}"]`)));
  await wait(()=>Number(testid('console-session-name').dataset.sessionId)!==current);
  await click(testid('console-service-rest'));
  await open();
  await wait(()=>idle() && panel()?.textContent.includes('No executions recorded for this check.'),
    'Copying a Console session must create new check identities without inheriting execution history');
  assert(entries().length===0 && !viewer(),'An empty history must not show an editor');
  const copiedEditor=editorOf('console-editor');
  await activateSession(current);
  await wait(()=>!copiedEditor.getWrapperElement().isConnected);
  await click(testid('console-service-rest'));await open();
  await wait(()=>entries().length===10);
  assert((await snapshot()).workspaceId===workspace,'Switching sessions must restore the persisted workspace');
  await click(testid('console-templates-toggle'));
  assert(!visible(document.querySelector('[data-testid="history-panel"]')),'Another right panel must completely hide History');
  await open();
  assert(!visible(document.querySelector('[data-testid="templates-panel"]')),'History must exclusively own the right pane');
  await click(testid('console-service-terminal'));
  const terminalEditor=editorOf('console-editor');
  const terminalOutput='+----+-------+\n| ID | Title |\n+----+-------+\n| 1  | Ready |\n+----+-------+\n';
  setSource(terminalEditor,"#check Terminal history\n#target\nsh -c\n#payload\nprintf '+----+-------+\\n| ID | Title |\\n+----+-------+\\n| 1  | Ready |\\n+----+-------+\\n'; printf 'warning\\n' >&2");
  terminalEditor.setCursor(terminalEditor.lastLine(),terminalEditor.getLine(terminalEditor.lastLine()).length);
  await key('Shift+Enter');
  await wait(()=>terminalEditor.getValue().includes(terminalOutput+'warning\n')
    && !testid('console-main-status').classList.contains('is-running'),
    ()=>`Terminal execution did not render plain output: ${terminalEditor.getValue()}`);
  assert(testid('console-main-status').textContent.includes('WARNING · warning'),
    `A Terminal command that exits 0 after writing to stderr must be a WARNING showing that line: ${testid('console-main-status').textContent}`);
  await wait(()=>entries().length===1 && idle());
  const terminalSnapshot=await detail(0);
  assert(terminalSnapshot.endsWith(terminalOutput+'warning\n'),
    'Terminal History must match the main check output, joining stdout/stderr without added text');
  assert(!terminalSnapshot.includes('#terminal-output') && !terminalSnapshot.includes('"type":"stdout"')
    && !terminalSnapshot.includes('"durationMs"'),
    'Terminal History must hide stream-event and completion-metadata envelopes');
  await click(testid('console-service-rest'));
  await wait(()=>entries().length===10);
  const largeEditor=editorOf('console-editor');
  setSource(largeEditor,`#check Large History\n#target\n${fixtureUrl}/history-large-fixture`);
  largeEditor.closeHint();largeEditor.focus();largeEditor.setCursor(2,0);
  await key('Shift+Enter');
  await wait(()=>largeEditor.getValue().includes('response truncated')
    && !testid('console-main-status').classList.contains('is-running')
    && testid('console-main-status').textContent.includes('OK'),
    ()=>`Large History fixture did not complete: ${testid('console-main-status').textContent}; ${[...largeEditor.getValue()].slice(-200).join('')}`);
  await wait(()=>entries().length===1 && idle());
  const largeSnapshot=await detail(0);
  assert(!document.querySelector('[data-testid="history-more-text"]'),
    'History must load all snapshot chunks automatically without a Load more snapshot text button');
  assert(new TextEncoder().encode(largeSnapshot).length>3*128*1024,
    `The large History regression must span at least four native UTF-8 snapshot chunks: ${largeSnapshot.length} chars, ${largeSnapshot.slice(0, 300)}`);
  const largeResponse=JSON.parse(largeSnapshot.split(/\n#responce[^\n]*\n/)[1]);
  assert(largeResponse.tail==='history-auto-loaded-end' && largeResponse.rows.length===3000
    && largeResponse.rows.every((row,index)=>row.id===index && row.text==='chunk-é-🦀 '.repeat(10)),
    'Automatic snapshot loading must preserve every row, UTF-8 boundary and final response bytes');
  await click(testid('console-history-toggle'));
  return true;
})()
