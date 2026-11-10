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
  const drafts=()=>JSON.parse(localStorage.getItem('module:templates:drafts')||'{}');
  const save=async(service,section,text)=>{
    setSource(editorOf('templates-editor'),text);
    await wait(()=>!drafts()[`${service}/${section}`]
      &&status().textContent.includes('Templates saved · OK'),
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
  const body='{\n  "id": 123,\n  "title": "Example Book Title",\n  "token": "{{var:TOKEN}}"\n}';
  const body2='{\n  "id": 1,\n  "title": "Example Book Title",\n}';
  const definitions=`@my_create_book bla bla comment\n${body}\n@my_create_book2 another comment\n${body2}`;
  // A module removed with its data kept leaves its draft behind; it must neither block nor lose the others.
  const unloaded='@kept\nGET /kept';
  localStorage.setItem('module:templates:drafts',JSON.stringify({'removed-module/check':unloaded}));
  const first=Number(testid('console-session-name').dataset.sessionId);
  await activateSession(first===0?1:0);
  await activateSession(first);
  await click(testid('console-service-graphql'));
  await open();
  assert(!testid('templates-panel').textContent.includes('Invalid Templates draft')
    &&!status().textContent.includes('ERROR'),
    'A draft of an unloaded module must not break Templates');
  assert(!testid('templates-panel').querySelector('.agent-intro'),'Templates must not show an introductory heading above the editor');
  assert(status().textContent.includes('@@ = @'),'The short escaping hint must live in the status strip');
  assert([...testid('templates-sections').querySelectorAll('[role="tab"]')].map(tab=>tab.textContent).join(',')==='check,target,headers,variables,payload,options',
    `GraphQL Templates must expose exactly its own section tabs: ${[...testid('templates-sections').querySelectorAll('[role="tab"]')].map(tab=>tab.textContent)}`);
  await select('variables');
  await save('graphql','variables',definitions);
  assert(drafts()['removed-module/check']===unloaded,
    'Saving another draft must keep the draft of an unloaded module until it is installed again');
  const tokens=(editor,line)=>editor.getLineTokens(line,true).map(token=>[token.string,token.type]);
  assert(editorOf('templates-editor').getOption('mode').name==='services-console-graphql',
    'GraphQL templates must use the same syntax mode as the main GraphQL Console');
  assert(editorOf('templates-editor').getLineTokens(0,true).every(token=>token.type?.includes('comment')),
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
  setSource(source,prefix.trimEnd());
  await key('Space');
  await pick('@my_create_book2');
  assert(source.getValue()===prefix.trimEnd()+'\n'+body2,
    'Pressing Space after #variables must automatically offer templates and insert only their body');
  setSource(source,prefix);
  await type('@');
  await wait(()=>entries().filter(item=>item.literal).length===2,'Typing @ must automatically show both body template names');
  assert(entries().every(item=>item.displayText.startsWith('@my_create_book')),
    'The menu must display template names, not their JSON bodies');
  await pick('@my_create_book');
  assert(source.getValue()===prefix.trimEnd()+'\n'+body,'Inline #variables template must insert only literal content on a new line');
  source.execCommand('undo');
  assert(source.getValue()===prefix+'@','Template insertion must be a single undoable editor change');
  source.execCommand('redo');
  assert(source.getValue()===prefix.trimEnd()+'\n'+body,'Redo must restore exact multiline content and variable placeholders');
  setSource(source,prefix+'@my_create_book2');
  await key('Control+Space');
  await pick('@my_create_book2');
  assert(source.getValue()===prefix.trimEnd()+'\n'+body2,'Template insertion must preserve raw content, including trailing JSON commas');
  setSource(source,prefix.trimEnd()+'\n');
  await type('@');
  await holds(()=>!entries().some(item=>item.displayText?.startsWith('@')),'A standalone @ inside a section must not automatically suggest templates',100);
  await key('Control+Space');
  await holds(()=>!entries().some(item=>item.displayText?.startsWith('@')),'Manual completion inside section content must not suggest templates',100);
  await select('headers');
  await save('graphql','headers','@auth Auth headers\nX-Template: yes\nContent-Type: application/json');
  setSource(source,'#check Headers\n#headers ');
  await type('@');
  await pick('@auth');
  assert(source.getValue()==='#check Headers\n#headers\nX-Template: yes\nContent-Type: application/json',
    'Header templates must not duplicate the section marker');
  await select('check');
  const full=`#check Full book\n#target\n${fixtureUrl}/template-full\n#headers\nAccept: application/json\n#variables\n${body}`;
  const implicit=`#target\n${fixtureUrl}/api/v1/Books/123\n#headers\nContent-Type: application/json\nAccept: application/json\n#variables\n${body2}`;
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
  await wait(()=>document.querySelector('[data-testid="templates-section-query"]'));
  await select('payload');
  await save('mysql','payload','@my_create_book SQL query\nSELECT 123 AS id;');
  setSource(editorOf('console-editor'),'#check SQL\n#payload ');
  await type('@');
  await pick('@my_create_book');
  assert(editorOf('console-editor').getValue()==='#check SQL\n#payload\nSELECT 123 AS id;',
    'Same template name must resolve independently for each service/section');
  assert(editorOf('templates-editor').getOption('mode').name==='services-console-mysql',
    'MySQL templates must switch to the MySQL Console syntax mode');
  assert(JSON.stringify(tokens(editorOf('templates-editor'),1))===JSON.stringify(tokens(editorOf('console-editor'),2)),
    'MySQL templates must use the exact main Console SQL token styles');
  await click(testid('console-service-splunk'));
  await wait(()=>document.querySelector('[data-testid="templates-section-options"]'));
  assert([...testid('templates-sections').querySelectorAll('[role="tab"]')].map(tab=>tab.textContent).join(',')==='check,target,headers,options,payload',
    'Splunk Templates must not duplicate options as empty from/to/app/count tabs');
  setSource(editorOf('console-editor'),'#check Splunk sections\n#');
  await key('Control+Space');
  await wait(()=>entries().some(item=>item.text==='#options'));
  assert(entries().map(item=>item.text).join(',')==='#target,#headers,#options,#payload,#check',
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
  setSource(editorOf('templates-editor'),'@dup\nfirst\n@dup\nsecond');
  await wait(()=>drafts()['graphql/variables']?.includes('@dup')&&status().textContent.includes('duplicate template'),
    'An invalid template must stay a draft with its error in the status');
  await click(testid('console-sessions-toggle'));
  await wait(()=>testid('console-sessions-toggle').getAttribute('aria-pressed')==='true');
  assert(!visible(document.querySelector('[data-testid="templates-panel"]')),
    'Opening Sessions must completely hide Templates, not leave a second flex panel');
  assertExclusivePanel();
  assert(testid('console-templates-toggle').querySelector('[data-state="error"]'),
    'An unseen error in hidden Templates must keep a red error indicator');
  await open();
  await wait(()=>status().textContent.includes('duplicate template'));
  assert(editorOf('templates-editor').getValue()==='@dup\nfirst\n@dup\nsecond','Reopening must retain invalid global drafts');
  for(const tool of ['sessions','hints','variables','dictionary','agent']) {
    await click(testid(`console-${tool}-toggle`));
    await wait(()=>testid(`console-${tool}-toggle`).getAttribute('aria-pressed')==='true');
    assertExclusivePanel();
    assert(!visible(document.querySelector('[data-testid="templates-panel"]')),
      `${tool} must replace Templates even when its draft has validation errors`);
    await open();
    assertExclusivePanel();
    assert(editorOf('templates-editor').getValue()==='@dup\nfirst\n@dup\nsecond',
      'Right-panel switching must preserve the invalid template draft');
  }
  await click(testid('templates-discard'));
  await wait(()=>editorOf('templates-editor').getValue()===globalText && !drafts()['graphql/variables']);
  assert(!JSON.stringify(await testDriver.sessionLibrary()).includes('bla bla comment'),
    'Template definitions/descriptions must not be copied into Console session snapshots');
  await click(testid('console-templates-toggle'));
  return true;
})()
