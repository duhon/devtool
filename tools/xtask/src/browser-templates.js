(async () => {
  const {assert,wait,holds,visible,click,testid,editorOf,editorAt,setSource,key,type,openConsole,activateSession,openPanel}=testDriver;
  const status=()=>testid('console-templates-status');
  await openConsole();
  await click(testid('console-service-graphql'));
  const open=async()=>{
    await openPanel('templates');
    await wait(()=>editorAt('templates-editor')?.getOption('readOnly')===false,
      ()=>`Templates editor did not become editable: ${document.querySelector('[data-testid="templates-panel"]')?.textContent.slice(0,1000)}`);
    assert(!document.querySelector('[data-testid="templates-reload"]'),
      'Templates must not render a manual Reload button in the header');
  };
  const select=async section=>{
    await click(testid(`templates-section-${section}`));
    await wait(()=>testid(`templates-section-${section}`).getAttribute('aria-selected')==='true');
  };
  const save=async(service,section,text)=>{
    setSource(editorOf('templates-editor'),text);
    await wait(()=>status().textContent.includes('Templates saved · OK'),
      ()=>`Templates did not autosave: ${status().textContent}; ${testid('templates-panel').textContent}`);
  };
  const entries=()=>editorOf('console-editor').state.completionActive?.data?.list||[];
  const pick=async name=>{
    await wait(()=>entries().some(item=>item.displayText.startsWith(name+' — ')||item.displayText===name),
      ()=>`Missing template ${name}: ${entries().map(item=>item.displayText)}`);
    const index=entries().findIndex(item=>item.displayText.startsWith(name+' — ')||item.displayText===name);
    const selected=editorOf('console-editor').state.completionActive.widget.selectedHint;
    for(let i=0;i<Math.abs(index-selected);i++) await key(index>selected?'ArrowDown':'ArrowUp');
    await key('Enter');
  };
  const source=editorOf('console-editor');
  const body='{\n  "id": 123,\n  "title": "Example Book Title",\n  "token": "{{.var.TOKEN}}"\n}';
  const body2='{\n  "id": 1,\n  "title": "Example Book Title",\n}';
  const definitions=`@my_create_book bla bla comment\n${body}\n@my_create_book2 another comment\n${body2}`;
  const first=Number(testid('console-session-name').dataset.sessionId);
  await activateSession(first===0?1:0);
  await activateSession(first);
  await click(testid('console-service-graphql'));
  await open();
  assert(!status().textContent.includes('ERROR'),'Templates must open without an error');
  assert(!testid('templates-panel').querySelector('.agent-intro'),'Templates must not show an introductory heading above the editor');
  assert(status().textContent.includes('\\@ = @'),'The short escaping hint must live in the status strip');
  assert([...testid('templates-sections').querySelectorAll('[role="tab"]')].map(tab=>tab.textContent).join(',')==='check,target,metadata,variables,payload,options,Global',
    `GraphQL Templates must expose exactly its own section tabs: ${[...testid('templates-sections').querySelectorAll('[role="tab"]')].map(tab=>tab.textContent)}`);
  await select('variables');
  await save('graphql','variables',definitions);
  const tokens=(editor,line)=>editor.getLineTokens(line,true).map(token=>[token.string,token.type]);
  assert(editorOf('templates-editor').getOption('mode').name==='services-console-graphql',
    'GraphQL templates must use the same syntax mode as the main GraphQL Console');
  assert(editorOf('templates-editor').getLineTokens(0,true)[1]?.type?.includes('def'),
    'Template definition headers must be styled separately from their body');
  setSource(source,'#check Highlight\n#variables\n'+body);
  await wait(()=>(source.getOption('servicesResponses')||[]).length===0,
    'Replacing a complete check must clear old response ownership before checking its syntax');
  assert(JSON.stringify(tokens(editorOf('templates-editor'),2))===JSON.stringify(tokens(source,3)),
    `GraphQL variables templates must use the exact main Console JSON token styles: ${JSON.stringify({
      template:tokens(editorOf('templates-editor'),2),main:tokens(source,3),
      mode:source.getOption('mode'),regions:source.getOption('servicesResponses'),
      text:source.getValue(),
    })}`);
  const prefix=`#check Template body\n#target\n${fixtureUrl}/template\n#variables `;
  const call=prefix.trimEnd();
  const expand=async()=>{
    editorOf('console-editor').setCursor(editorOf('console-editor').lastLine(),editorOf('console-editor').getLine(editorOf('console-editor').lastLine()).length);
    await key('Enter');
  };
  setSource(source,'#check Eager\n#target\n{{$!nope()}}');
  assert(tokens(source,2).some(([,type])=>type?.includes('tpl-problem')),
    `A call of an unknown function must be underlined: ${JSON.stringify(tokens(source,2))}`);
  await expand();
  await wait(()=>testid('console-main-status').textContent.includes('Editor · ERROR · {{$!nope()}}: Unknown function $nope'),
    ()=>`An eager call that fails must report in the check status: ${testid('console-main-status').textContent}`);
  assert(!document.querySelector('[data-testid="app-notice"]'),'Editor errors must stay out of the top banner');
  setSource(source,'#check Arguments\n#target\n');
  await type('{{$nope(.');
  await wait(()=>entries().some(item=>item.text==='.var.'),
    ()=>`A . argument of a function call must complete the variable tree: ${JSON.stringify(entries().map(item=>item.text))}`);
  await key('Escape');
  setSource(source,call);
  await key('Space');
  await pick('my_create_book2');
  assert(source.getValue()===call+' @my_create_book2',
    'Choosing a template after #variables must only insert the call on the section line');
  await expand();
  assert(source.getValue()===call+' @my_create_book2\n'+body2,
    'Enter on the section line must replace the section body with the expansion');
  setSource(source,prefix);
  await type('@');
  await wait(()=>entries().filter(item=>item.literal).length===2,'Typing @ must automatically show both template names');
  assert(entries().every(item=>item.displayText.startsWith('my_create_book')),
    'The menu must display template signatures, not their JSON bodies');
  await pick('my_create_book');
  assert(source.getValue()===call+' @my_create_book','The call must replace the typed @');
  await expand();
  assert(source.getValue()===call+' @my_create_book\n'+body,'Enter must insert the literal body on a new line');
  source.execCommand('undo');
  assert(source.getValue()===call+' @my_create_book','Expansion must be a single undoable editor change');
  source.execCommand('redo');
  assert(source.getValue()===call+' @my_create_book\n'+body,'Redo must restore exact multiline content and variable placeholders');
  setSource(source,prefix.trimEnd()+'\n');
  await type('@');
  await holds(()=>!entries().some(item=>item.displayText?.startsWith('my_create_book')),'A standalone @ inside a section must not automatically suggest templates',100);
  await key('Control+Space');
  await holds(()=>!entries().some(item=>item.displayText?.startsWith('my_create_book')),'Manual completion inside section content must not suggest templates',100);
  await select('metadata');
  await save('graphql','metadata','@auth Auth headers\nX-Template: yes\nContent-Type: application/json');
  setSource(source,'#check Headers\n#metadata ');
  await type('@');
  await pick('auth');
  await expand();
  assert(source.getValue()==='#check Headers\n#metadata @auth\nX-Template: yes\nContent-Type: application/json',
    'A header template must expand under its own section marker');
  await select('check');
  const full=`#check Full book\n#target\n${fixtureUrl}/template-full\n#metadata\nAccept: application/json\n#variables\n${body}`;
  const implicit=`#target\n${fixtureUrl}/api/v1/Books/123\n#metadata\nContent-Type: application/json\nAccept: application/json\n#variables\n${body2}`;
  await save('graphql','check',`@full_book Complete check\n${full}\n@test1 mmm\n${implicit}\n@without_description\n${implicit}`);
  setSource(source,'#check @test1');
  await key('Control+Space');
  await pick('@test1');
  assert(source.getValue()==='#check mmm\n'+implicit,
    'A check template must generate #check from its description without inserting @name');
  source.execCommand('undo');
  assert(source.getValue()==='#check @test1','Generated check header and body must undo together');
  setSource(source,'#check @without_description');
  await key('Control+Space');
  await pick('@without_description');
  assert(source.getValue()==='#check without_description\n'+implicit,
    'An empty description must use the template name for the generated check title');
  setSource(source,'#check ');
  await type('@');
  await pick('@full_book');
  assert(source.getValue()===full,'A whole-check template must replace the reference and original #check marker');
  const previous='#check Previous\n#target\nhttps://fixture.invalid/previous';
  setSource(source,previous+'\n#check @full_book');
  await key('Control+Space');
  await pick('@full_book');
  assert(source.getValue()===previous+'\n\n'+full,
    'A check template must atomically add a missing separator before the new check');
  source.undo();
  assert(source.getValue()===previous+'\n#check @full_book',
    'The separator and complete check template must undo together');
  source.redo();
  assert(source.getValue()===previous+'\n\n'+full,
    'Redo must restore the exact template and separator');
  await click(testid('console-service-mysql'));
  await wait(()=>document.querySelector('[data-testid="templates-section-payload"]'));
  await select('payload');
  await save('mysql','payload','@my_create_book SQL query\nSELECT 123 AS id;\n@by_id(id: "1" | "2" = "1") Row by id\nSELECT {{id}} AS id;');
  setSource(editorOf('console-editor'),'#check SQL\n#payload ');
  await type('@');
  await pick('my_create_book');
  await expand();
  assert(editorOf('console-editor').getValue()==='#check SQL\n#payload @my_create_book\nSELECT 123 AS id;',
    'Same template name must resolve independently for each service/section');
  assert(editorOf('templates-editor').getOption('mode').name==='services-console-mysql',
    'MySQL templates must switch to the MySQL Console syntax mode');
  assert(JSON.stringify(tokens(editorOf('templates-editor'),1))===JSON.stringify(tokens(editorOf('console-editor'),2)),
    'MySQL templates must use the exact main Console SQL token styles');
  setSource(editorOf('console-editor'),'#check SQL\n#payload @by_id("2")');
  await expand();
  assert(editorOf('console-editor').getValue()==='#check SQL\n#payload @by_id("2")\nSELECT 2 AS id;',
    'Enter on a call with arguments must expand the parametrized body');
  await click(testid('console-service-splunk'));
  await wait(()=>document.querySelector('[data-testid="templates-section-options"]'));
  assert([...testid('templates-sections').querySelectorAll('[role="tab"]')].map(tab=>tab.textContent).join(',')==='check,target,metadata,options,payload,Global',
    'Splunk Templates must not duplicate options as empty from/to/app/count tabs');
  setSource(editorOf('console-editor'),'#check Splunk sections\n#');
  await key('Control+Space');
  await wait(()=>entries().some(item=>item.text==='#options'));
  assert(entries().map(item=>item.text).join(',')==='#target,#metadata,#options,#payload,#check',
    'Splunk section completion must offer only canonical sections');
  await key('Escape');
  for(const section of ['from','to','app','count']) {
    const splunk=editorOf('console-editor');
    setSource(splunk,`#check Removed ${section}\n#target\nhttps://splunk.fixture\n#payload\nindex=main\n#${section}\nvalue`);
    splunk.setCursor(6,5);splunk.focus();
    await key('Shift+Enter');
    await wait(()=>testid('console-main-status').textContent.includes(`Unsupported Splunk section #${section}; use #options.`),
      `Removed #${section} must show an explicit error instead of being sent as SPL`);
    assert(!testid('console-main-status').classList.contains('is-running'),
      'Rejected Splunk sections must not start a request');
  }
  await click(testid('console-service-graphql'));
  await select('variables');
  assert(editorOf('templates-editor').getValue()===definitions,'Switching services must restore the selected section library');
  const session=Number(testid('console-session-name').dataset.sessionId);
  await activateSession(0);
  await click(testid('console-service-graphql'));
  await open();await select('variables');
  assert(editorOf('templates-editor').getValue()===definitions,'Temporary session must share global templates');
  const globalText=definitions+'\n@from_temporary Global\n{"global":true}';
  await save('graphql','variables',globalText);
  await activateSession(session);
  await click(testid('console-service-graphql'));
  await open();await select('variables');
  assert(editorOf('templates-editor').getValue()===globalText,'Templates edited in temporary session must persist in the named session');
  const assertExclusivePanel=()=>{
    const panel=testid('console-tool-panel');
    assert([...panel.children].filter(visible).length===1,
      'Only the selected right tool may occupy space; hidden panels must not split the pane');
  };
  const invalid='@dup\nfirst\n@dup\nsecond';
  setSource(editorOf('templates-editor'),invalid);
  await wait(()=>status().textContent.includes('duplicate template'),
    'An invalid template must be saved with its error in the status');
  await click(testid('console-sessions-toggle'));
  await wait(()=>testid('console-sessions-toggle').getAttribute('aria-pressed')==='true');
  assert(!visible(document.querySelector('[data-testid="templates-panel"]')),
    'Opening Sessions must completely hide Templates, not leave a second flex panel');
  assertExclusivePanel();
  assert(testid('console-templates-toggle').querySelector('[data-state="error"]'),
    'An unseen error in hidden Templates must keep a red error indicator');
  await open();
  await wait(()=>status().textContent.includes('duplicate template'));
  assert(editorOf('templates-editor').getValue()===invalid,'Reopening must show the saved invalid templates');
  for(const tool of ['sessions','hints','variables','dictionary','agent']) {
    await click(testid(`console-${tool}-toggle`));
    await wait(()=>testid(`console-${tool}-toggle`).getAttribute('aria-pressed')==='true');
    assertExclusivePanel();
    assert(!visible(document.querySelector('[data-testid="templates-panel"]')),
      `${tool} must replace Templates even when its templates have errors`);
    await open();
    assertExclusivePanel();
    assert(editorOf('templates-editor').getValue()===invalid,
      'Right-panel switching must keep the invalid templates');
  }
  assert(!document.querySelector('[data-testid="templates-discard"]'),'Templates have no Discard draft button');
  await save('graphql','variables',globalText);
  assert(!JSON.stringify(await testDriver.sessionLibrary()).includes('bla bla comment'),
    'Template definitions/descriptions must not be copied into Console session snapshots');
  await click(testid('console-templates-toggle'));
  return true;
})()
