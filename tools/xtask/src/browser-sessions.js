(async () => {
  const {assert,wait,click,testid,editorOf,setSource,key,activateSession,visible}=testDriver;
  const current=()=>Number(testid('console-session-name').dataset.sessionId);
  const library=testDriver.sessionLibrary;
  const open=async tool=>{
    const toggle=testid(`console-${tool}-toggle`);
    if(toggle.getAttribute('aria-pressed')!=='true') await click(toggle);
    await wait(()=>toggle.getAttribute('aria-pressed')==='true');
    const editor={variables:'console-variables',dictionary:'console-dictionary',hints:'console-hints'}[tool];
    if(editor) await wait(()=>testDriver.editorAt(editor));
  };
  const sessionPanel=async()=>{
    await open('sessions');
    await testDriver.sessionsIdle();
  };
  const copy=(id,name)=>testDriver.createSession(id,name);
  const vars=async text=>{
    await open('variables');
    editorOf('console-variables').setValue(text);
    await wait(()=>testid('console-variables-status').textContent.includes(text==='missing delimiter'?'ERROR':'Variables save · OK'));
  };
  const hints=async text=>{
    await open('hints');
    editorOf('console-hints').setValue(text);
    await wait(()=>testid('console-hints-status').textContent.includes(text.startsWith('##')?'ERROR':'Hints save · OK'));
  };
  const runRest=async expected=>{
    await click(testid('console-service-rest'));
    const editor=editorOf('console-editor');
    editor.focus();
    editor.setCursor(0,0);
    await key('Shift+Enter');
    await wait(()=>testid('console-main-status').textContent.includes('OK')
      &&editor.getValue().includes(`session-value=${expected}`),
      `Request execution must resolve var from session ${expected}`);
  };
  await wait(()=>document.querySelector('[data-testid="console-session-name"]'));
  assert(current()===0,'Every new application must start in temporary session 0');
  assert((await library()).sessions[0].name==='Imported','Existing workspace must be preserved, not automatically activated');
  // The fixture var and hints are defaults of sessions without their own; this scenario
  // checks sessions start empty.
  localStorage.removeItem('module:variables:var');
  const legacy=await library();
  const legacyRaw=JSON.stringify(legacy);
  localStorage.setItem('servicesConsole.sessions.v1',legacyRaw);
  await testDriver.clearSessions();
  const migrationFrame=document.createElement('iframe');
  const migrationUrl=new URL(location.href);
  migrationUrl.searchParams.set('test-session-write-failure','1');
  migrationFrame.src=migrationUrl.href;
  document.body.append(migrationFrame);
  await wait(()=>migrationFrame.contentDocument?.body?.textContent.includes('Fixture session storage write failed'),
    ()=>`Failed IndexedDB migration must be reported: ${JSON.stringify({
      url:migrationFrame.contentWindow?.location.href,
      ready:migrationFrame.contentDocument?.readyState,
      injected:!!migrationFrame.contentWindow?.testDriver,
      failWrites:migrationFrame.contentWindow?.__testFailSessionWrites,
      body:migrationFrame.contentDocument?.body?.innerText.slice(0,2000),
      legacyPresent:localStorage.getItem('servicesConsole.sessions.v1')!==null,
    })}`);
  assert(localStorage.getItem('servicesConsole.sessions.v1')===legacyRaw,
    'A failed migration must preserve the complete original localStorage library');
  assert(await library()===null,'Failed migration must not commit an incomplete session library');
  migrationFrame.remove();
  const migratedFrame=document.createElement('iframe');
  migratedFrame.src=location.href;
  document.body.append(migratedFrame);
  await wait(()=>migratedFrame.contentDocument?.querySelector('[data-testid="console-session-name"]'));
  const migrated=await library();
  assert(JSON.stringify(migrated.sessions)===JSON.stringify(legacy.sessions),
    'Migration must preserve all original sessions and snapshots');
  assert(localStorage.getItem('servicesConsole.sessions.v1')===null,
    'The quota-limited legacy key must be removed only after IndexedDB commit');
  migratedFrame.remove();
  // The migrated library reaches this window: its console reloads (Export is enabled with it).
  await click(testid('console-transfer-toggle'));
  await wait(()=>document.querySelector('[data-testid="console-export"]:not(:disabled)'),'This window must reload the migrated library');
  await click(testid('console-transfer-toggle'));
  await activateSession(1);
  await activateSession(0);
  for(const service of ['mysql','rest','graphql','grpc','splunk','terminal']) {
    await click(testid(`console-service-${service}`));
    assert(editorOf('console-editor').getValue()==='',`${service}: fresh temporary checks must be empty`);
  }
  await click(testid('console-service-rest'));
  setSource(editorOf('console-editor'),'#check temp-only\n#target\nhttps://temporary.invalid');
  await open('variables');
  assert(editorOf('console-variables').getValue()==='','Temporary var must not inherit native legacy variables');
  await open('hints');
  assert(!editorOf('console-hints').getValue().includes('temp-only'),'Temporary hints must not hold the temporary request');
  assert(!JSON.stringify(await library()).includes('temp-only'),'Temporary contents must never be saved on disk');
  const a=await copy(0,'Session A');
  assert(editorOf('console-editor').getValue().includes('temp-only'),'Creating from 0 must keep the temporary working checks');
  await sessionPanel();
  assert(testid('session-used-0').textContent==='Immutable','The temporary session has no last-used date');
  assert(testid(`session-name-${a}`).tagName==='INPUT'&&testid('session-name-0').tagName!=='INPUT',
    'Only the current named session is renamed in place');
  const nameField=testid(`session-name-${a}`);
  {
    const textOf=element=>{const range=document.createRange();range.selectNodeContents(element);return range.getBoundingClientRect();};
    const style=getComputedStyle(nameField), field=nameField.getBoundingClientRect(), plain=textOf(testid('session-name-0'));
    const left=field.left+parseFloat(style.borderLeftWidth)+parseFloat(style.paddingLeft);
    const row=id=>testid(`session-${id}`).getBoundingClientRect().height;
    assert(Math.abs(left-plain.left)<1&&Math.abs(row(a)-row(0))<1,
      `The name being edited must stay where plain names are: text ${left} vs ${plain.left}, rows ${row(a)} vs ${row(0)}`);
  }
  nameField.focus();
  nameField.value='Session A renamed';
  await key('Escape');
  assert(document.activeElement!==nameField&&nameField.value==='Session A','Escape must put the name back and leave the field');
  nameField.focus();
  nameField.value='Session A renamed';
  await key('Enter');
  assert(document.activeElement!==nameField,'Enter must leave the name field');
  await wait(()=>testid('console-session-name').textContent==='Session A renamed');
  testDriver.fill(testid(`session-name-${a}`),'Session A');
  await wait(()=>testid('console-session-name').textContent==='Session A');
  for(const service of ['mysql','rest','graphql','grpc','splunk','terminal']) {
    await click(testid(`console-service-${service}`));
    setSource(editorOf('console-editor'),service==='rest'
      ?`#check session-a-rest\n#target\n${fixtureUrl}/session?session-value={{var:SESSION_MARKER}}\n`
      :`#check session-a-${service}\n`);
  }
  await vars('SESSION_MARKER: alpha');
  await hints('#target\nhttps://session-a.invalid\n');
  await runRest('alpha');
  const large=await copy(0,'Large result');
  await click(testid('console-service-mysql'));
  const largeEditor=editorOf('console-editor');
  // MySQL inline tables, unlike REST previews, are not capped at 300,000 characters.
  setSource(largeEditor,`#check large-persistence\n#target\nmysql://mysql.fixture.invalid\n#payload\nSELECT large_fixture;\n\n#responce OK\nsession-large-start\n${('x'.repeat(1024)+'\n').repeat(7168)}session-large-end`);
  const largeDocument=largeEditor.getValue();
  assert(largeDocument.length>6*1024*1024,'The regression must exceed the old Web Storage quota');
  let quotaExceeded=false;
  try { localStorage.setItem('__session_quota_probe',largeDocument); }
  catch(error) { if(error.name==='QuotaExceededError') quotaExceeded=true;else throw error; }
  finally { localStorage.removeItem('__session_quota_probe'); }
  assert(quotaExceeded,'The same uncompressed document must actually exceed localStorage quota');
  assert((await library()).sessions.find(s=>s.id===large).snapshot.console.documents.mysql===largeDocument,
    'IndexedDB must preserve the entire response without truncation');
  const largeFrame=document.createElement('iframe');
  largeFrame.src=location.href;
  largeFrame.style.cssText='position:fixed;inset:0;width:100vw;height:100vh;z-index:9999';
  document.body.append(largeFrame);
  try {
    await wait(()=>largeFrame.contentDocument?.querySelector('[data-testid="console-session-name"]'));
    await activateSession(large,largeFrame.contentDocument);
    assert(editorOf('console-editor',largeFrame.contentDocument).getValue()===largeDocument,
      'A fresh application instance must restore every byte of the large document');
    largeFrame.contentWindow.__testFailSessionWrites=true;
    setSource(editorOf('console-editor',largeFrame.contentDocument),
      largeDocument.replace('#check large-persistence','#check failed-write'));
    const frameDoc=largeFrame.contentDocument;
    // The App's autosave marks the Sessions button even while the panel is closed.
    if(testid('console-sessions-toggle',frameDoc).getAttribute('aria-pressed')==='true')
      await click(testid('console-sessions-toggle',frameDoc));
    await wait(()=>frameDoc.querySelector('[data-testid="console-sessions-toggle"] [data-state="error"]'),
      'Asynchronous save failures must mark the Sessions button');
    await click(testid('console-sessions-toggle',frameDoc));
    assert(![...frameDoc.querySelectorAll('.banner')].some(visible),'Save failures must not use the top banner');
    await wait(()=>testid('console-sessions-status',frameDoc).textContent.includes('Autosave · ERROR · Session save failed: Fixture session storage write failed. Changes are not saved'),
      ()=>`Asynchronous save failures must be visible in the Sessions status: ${testid('console-sessions-status',frameDoc).textContent}`);
    assert((await library()).sessions.find(s=>s.id===large).snapshot.console.documents.mysql===largeDocument,
      'Failed autosave must leave the previously committed large document intact');
    largeFrame.contentWindow.__testFailSessionWrites=false;
    await activateSession(0,largeFrame.contentDocument);
  } finally { largeFrame.remove(); }
  const largeCopy=await copy(large,'Large copy');
  assert(editorOf('console-editor').getValue()===largeDocument,'Copying must retain the full large response');
  assert((await library()).sessions.find(s=>s.id===largeCopy).snapshot.console.documents.mysql===largeDocument,
    'The copied large session must also persist');
  await sessionPanel();
  window.confirm=()=>false;
  await click(testid(`session-delete-${largeCopy}`));
  delete window.confirm;
  assert(current()===largeCopy&&document.querySelector(`[data-testid="session-${largeCopy}"]`),'Declining the question must keep the session');
  await testDriver.deleteSession(largeCopy);
  await wait(()=>current()===0);
  await sessionPanel();
  await testDriver.deleteSession(large);
  await wait(()=>!document.querySelector(`[data-testid="session-${large}"]`));
  await activateSession(a);
  await vars('missing delimiter');
  await hints('## invalid-session-hints\ninvalid draft');
  const b=await copy(a,'Session B');
  await open('variables');
  await wait(()=>editorOf('console-variables').getValue()==='missing delimiter',
    'Cloning must retain invalid var drafts as well as last saved values');
  await open('hints');
  assert(editorOf('console-hints').getValue().startsWith('## invalid-session-hints'),'Cloning must retain invalid hints drafts');
  await vars('SESSION_MARKER: beta');
  await hints('#target\nhttps://session-b.invalid\n');
  await runRest('beta');
  await open('dictionary');
  const dictionary=editorOf('console-dictionary').getValue();
  editorOf('console-dictionary').setValue(`${dictionary}\nSESSION_GLOBAL: shared`);
  await wait(()=>testid('console-dictionary-status').textContent.includes('Dictionary save · OK'));
  await open('agent');
  const composer=await wait(()=>document.querySelector('[data-testid="agent-composer"]'));
  testDriver.fill(composer,'session B agent draft');
  await activateSession(a);
  for(const service of ['mysql','graphql','grpc','splunk','terminal']) {
    await click(testid(`console-service-${service}`));
    assert(editorOf('console-editor').getValue()===`#check session-a-${service}\n`,`${service}: all checks must restore independently`);
  }
  await open('variables');
  await wait(()=>editorOf('console-variables').getValue()==='missing delimiter');
  await click(testid('console-service-rest'));
  await open('hints');
  assert(editorOf('console-hints').getValue().startsWith('## invalid-session-hints'),'Session A hints draft must not be replaced by B');
  await runRest('alpha');
  await open('dictionary');
  await wait(()=>editorOf('console-dictionary').getValue().includes('SESSION_GLOBAL: shared'),
    'Dictionary writes must be global across session switches');
  editorOf('console-dictionary').setValue(dictionary);
  await wait(()=>testid('console-dictionary-status').textContent.includes('Dictionary save · OK'));
  await open('agent');
  await wait(()=>document.querySelector('[data-testid="agent-composer"]'));
  assert(testid('agent-composer').value!=='session B agent draft','Agent chats belong to their Console session');
  await click(testid('agent-chat-list-toggle'));
  await wait(()=>[...document.querySelectorAll('[data-testid="agent-chat-list"] tbody tr')]
    .some(row=>row.querySelector('[data-testid="agent-chat-state"]').textContent.startsWith('Open')
      &&row.querySelector('[data-testid="agent-chat-session"]').textContent==='Session B'),
    'The chat list shows chats of other Console sessions');
  await click(testid('agent-chat-list-toggle'));
  const frame=document.createElement('iframe');
  frame.src=location.href;
  frame.style.cssText='position:fixed;inset:0;width:100vw;height:100vh;z-index:9999';
  document.body.append(frame);
  try {
    await wait(()=>frame.contentDocument?.querySelector('[data-testid="console-session-name"]'));
    const doc=frame.contentDocument;
    assert(testid('console-session-name',doc).dataset.sessionId==='0'
      &&editorOf('console-editor',doc).getValue()==='','New windows must not resume a named session');
    await activateSession(a,doc);
    assert(editorOf('console-editor',doc).getValue().includes('session-a-rest'),'Named checks must persist across application instances');
    await click(testid('console-hints-toggle',doc));
    await wait(()=>doc.querySelector('[data-testid="console-hints"]')&&editorOf('console-hints',doc));
    editorOf('console-hints',doc).setValue('#target\nhttps://other-window.invalid\n');
    await wait(()=>testid('console-hints-status',doc).textContent.includes('Hints save · OK'));
  } finally { frame.remove(); }
  // The Dictionary is shared by all windows through Local: another window's save shows when the panel is shown again.
  const reopen=async()=>{await testDriver.closePanel('dictionary');await open('dictionary');};
  const activeSet=doc=>doc.querySelector('[data-testid="dictionary-panel"] [role="tab"][aria-selected="true"]').textContent;
  const otherDictionary=async text=>{
    const set=activeSet(document);
    const frame=document.createElement('iframe');
    frame.src=location.href;
    frame.style.cssText='position:fixed;inset:0;width:100vw;height:100vh;z-index:9999';
    document.body.append(frame);
    try {
      const doc=await wait(()=>frame.contentDocument?.querySelector('[data-testid="console-dictionary-toggle"]')&&frame.contentDocument);
      await click(testid('console-dictionary-toggle',doc));
      await wait(()=>doc.querySelector('[data-testid="console-dictionary"]')&&editorOf('console-dictionary',doc));
      assert(activeSet(doc)===set,'Both windows must edit the same Dictionary set');
      editorOf('console-dictionary',doc).setValue(text);
      await wait(()=>testid('console-dictionary-status',doc).textContent.includes('Dictionary save · OK'));
    } finally { frame.remove(); }
  };
  await open('dictionary');
  await wait(()=>editorOf('console-dictionary').getValue()===dictionary);
  await otherDictionary(`${dictionary}\nOTHER_WINDOW: saved`);
  await reopen();
  await wait(()=>editorOf('console-dictionary').getValue().includes('OTHER_WINDOW: saved'),
    'A window must show the Dictionary another window saved');
  await otherDictionary(dictionary);
  await reopen();
  await wait(()=>editorOf('console-dictionary').getValue()===dictionary);
  // An invalid draft is not saved, so this window's draft deterministically starts before
  // the other window saves.
  editorOf('console-dictionary').setValue(`${dictionary}\nmissing delimiter`);
  await wait(()=>testid('console-dictionary-status').textContent.includes('ERROR'));
  await otherDictionary(`${dictionary}\nOTHER_WINDOW: saved`);
  editorOf('console-dictionary').setValue(`${dictionary}\nSTALE_WINDOW: draft`);
  await wait(()=>testid('console-dictionary-status').textContent.includes('changed in another window'),
    'A Dictionary draft started before another window saved must conflict instead of overwriting it');
  assert(editorOf('console-dictionary').getValue().includes('STALE_WINDOW: draft'),'A conflicting Dictionary draft must be preserved');
  await click(testid('console-dictionary-discard'));
  await wait(()=>editorOf('console-dictionary').getValue().includes('OTHER_WINDOW: saved')
    &&!document.querySelector('[data-testid="console-dictionary-discard"]'),
    'Discarding a conflicting Dictionary draft must load the other window’s save');
  await otherDictionary(dictionary);
  await reopen();
  await wait(()=>editorOf('console-dictionary').getValue()===dictionary,
    'A window must show the Dictionary another window restored');
  // A reply that changes nothing must still clear the guard: the recreated application's module
  // list request finishes without other state changes, which left data-busy stale on slow hosts.
  testDriver.nativeHold.match=message=>message.service==='modules'&&!message.op;
  await activateSession(0);
  await wait(()=>/^native in flight 1, background 0 \[modules [\d.]+s\]$/.test(testid('console-session-name').dataset.busyReason),
    ()=>`Sessions must stay busy only while the recreated application loads its module list: ${testid('console-session-name').dataset.busyReason}`);
  testDriver.nativeHold.release();
  await testDriver.sessionsIdle();
  await activateSession(a);
  await open('hints');
  await wait(()=>editorOf('console-hints').getValue().includes('other-window.invalid'),'Explicitly switching back must load the saved winner');
  await activateSession(b);
  await click(testid('console-service-terminal'));
  setSource(editorOf('console-editor'),"#check session-busy\n#target\nsh -c\n#payload\nprintf session-running; sleep 30");
  await key('Shift+Enter');
  await wait(()=>testid('console-main-status').classList.contains('is-running'));
  await open('sessions');
  await click(testid(`session-${a}`));
  await wait(()=>testid('console-sessions-status').textContent.includes('ERROR'));
  assert(current()===b,'Switching must be rejected while any service request is running');
  assert(testid('console-main-status').classList.contains('is-running'),'Rejected switching must not cancel work implicitly');
  await click(testid('console-stop'));
  await testDriver.sessionsIdle();
  await activateSession(a);
  await sessionPanel();
  await testDriver.deleteSession(b);
  await wait(()=>!document.querySelector(`[data-testid="session-${b}"]`));
  await testDriver.deleteSession(a);
  await wait(()=>current()===0);
  assert(editorOf('console-editor').getValue().includes('temp-only'),'Returning to 0 must preserve this window’s unsaved temporary work');
  await sessionPanel();
  assert(testid('session-delete-0').disabled,'Temporary session must not be deletable');
  await activateSession(1);
  assert((await library()).sessions.length===1,'Deleted sessions must remain deleted');
  return true;
})();
