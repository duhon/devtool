(async()=>{
  const {assert,wait,click,testid,editorOf,setSource,key,type,openConsole,visible}=testDriver;
  const bar=()=>document.querySelector('[data-testid="find-bar"]');
  const count=()=>testid('find-count').textContent;
  const query=async text=>{
    testid('find-query').select();
    await type(text);
  };
  const occurrences=(text,word)=>text.split(word).length-1;
  await openConsole();
  await click(testid('console-service-rest'));
  const editor=editorOf('console-editor');
  const wrapper=editor.getWrapperElement();
  setSource(editor,`#check Find acceptance\n#target\n${fixtureUrl}/history-fixture?value=needle&other=needle`);
  editor.focus();editor.setCursor(2,5);
  const before=editor.getValue();
  await key('Shift+Enter');
  await wait(()=>editor.getValue()!==before&&!testid('console-main-status').classList.contains('is-running')
    &&testid('console-main-status').textContent.includes('OK'),
    ()=>`Find fixture request did not complete: ${testid('console-main-status').textContent}`);
  const total=occurrences(editor.getValue(),'needle');
  assert(total>2,`The response must repeat the query: ${editor.getValue()}`);

  // Mod+F opens the bar in the focused editor and says which one it searches.
  editor.focus();editor.setCursor(0,0);
  await key('Mod+F');
  await wait(bar,'Mod+F did not open the find bar');
  assert(wrapper.contains(bar()),'The Console find bar must be drawn inside the Console editor');
  assert(wrapper.classList.contains('find-bar-target'),'The searched editor must be outlined');
  assert(/Find in Console · /.test(testid('find-scope').textContent),`Wrong scope: ${testid('find-scope').textContent}`);
  assert(document.activeElement===testid('find-query'),'Mod+F must focus the query');
  await query('needle');
  await wait(()=>count()===`1 of ${total}`,()=>`Wrong count: ${count()} for ${total}`);
  assert(editor.getSelection()==='needle'&&editor.getCursor('from').line===2,'Typing must select the first match');
  assert(wrapper.querySelectorAll('.editor-find-current').length===1,'The current match must be highlighted');
  assert(wrapper.querySelectorAll('.editor-find-match').length>=total,'Every visible match must be highlighted');
  await key('Enter');
  await wait(()=>count()===`2 of ${total}`,()=>`Enter did not move to the next match: ${count()}`);
  await key('Shift+Enter');
  await wait(()=>count()===`1 of ${total}`,()=>`Shift+Enter did not move back: ${count()}`);
  await key('Mod+G');
  await wait(()=>count()===`2 of ${total}`,()=>`Mod+G did not move to the next match: ${count()}`);

  await click(testid('find-regex'));
  await query('needle(');
  await wait(()=>count()==='Invalid regex','An invalid pattern must be reported');
  await query('NEEDLE');
  // Typing searches on from the match shown last.
  await wait(()=>count()===`2 of ${total}`,()=>`The search must ignore case by default: ${count()}`);
  await click(testid('find-case'));
  await wait(()=>count()==='No results','Match Case must find nothing for NEEDLE');
  await click(testid('find-case'));
  await click(testid('find-regex'));

  // Replace All edits the requests only and is a single undo step.
  await query('needle');
  await click(testid('find-toggle-replace'));
  await wait(()=>document.activeElement===document.querySelector('[data-testid="find-replacement"]'),
    'Opening Replace must focus the replacement');
  await type('pin');
  const original=editor.getValue();
  await key('Mod+Enter');
  await wait(()=>count().startsWith('Replaced 2 · '),()=>`Replace All did not report: ${count()}`);
  assert(count()===`Replaced 2 · ${total-2} in responses kept`,`Wrong Replace All note: ${count()}`);
  assert(editor.getLine(2).endsWith('value=pin&other=pin'),`The request was not replaced: ${editor.getLine(2)}`);
  assert(occurrences(editor.getValue(),'needle')===total-2,'Responses must keep their text');
  editor.focus();
  await key('Mod+Z');
  await wait(()=>editor.getValue()===original,'One undo must restore every replacement');

  await key('Escape');
  await wait(()=>!bar(),'Escape in the editor did not close the find bar');
  assert(!wrapper.querySelector('.editor-find-match')&&!wrapper.classList.contains('find-bar-target'),
    'Closing must clear the highlights and the outline');

  // A side panel searches its own editor.
  await click(testid('console-variables-toggle'));
  // The editor appears once the Variables have loaded.
  await wait(()=>visible(document.querySelector('[data-testid="console-variables-editor"]')),
    'Variables editor is not visible');
  const variables=editorOf('console-variables-editor');
  variables.focus();
  await key('Mod+F');
  await wait(()=>bar()&&variables.getWrapperElement().contains(bar()),'Mod+F must open the find bar in Variables');
  assert(/Find in Variables · /.test(testid('find-scope').textContent),`Wrong scope: ${testid('find-scope').textContent}`);
  editor.focus();
  await key('Mod+F');
  await wait(()=>bar()&&wrapper.contains(bar()),'Mod+F in the Console must move the bar back');
  assert(!variables.getWrapperElement().classList.contains('find-bar-target'),'Only one editor is searched');
  await key('Escape');
  await wait(()=>!bar(),'Escape did not close the find bar');

  // Highlights stay on the visible lines and Replace All stays fast on a large document.
  const lines=20000;
  setSource(editor,Array.from({length:lines},(_,index)=>`item ${index} needle`).join('\n'));
  editor.setCursor(0,0);editor.focus();
  await key('Mod+F');
  await wait(bar);
  await query('needle');
  await wait(()=>count()==='1 of 10000+',()=>`Large count: ${count()}`);
  assert(wrapper.querySelectorAll('.editor-find-match').length<200,'Only visible matches may be highlighted');
  assert(document.querySelector('[data-testid="find-replacement"]'),'Replace must stay expanded after closing');
  testid('find-replacement').select();
  await type('pin');
  const started=performance.now();
  await click(testid('find-replace-all'));
  await wait(()=>count()===`Replaced ${lines}`,()=>`Large Replace All: ${count()}`,30000);
  const elapsed=performance.now()-started;
  assert(editor.getLine(lines-1)===`item ${lines-1} pin`,'Large Replace All did not finish');
  assert(elapsed<3000,`Replace All of ${lines} matches took ${Math.round(elapsed)} ms`);
  editor.focus();
  const undoStarted=performance.now();
  await key('Mod+Z');
  await wait(()=>editor.getLine(lines-1)===`item ${lines-1} needle`,'Large Replace All was not undone in one step');
  const undone=performance.now()-undoStarted;
  assert(undone<3000,`Undo of ${lines} replacements took ${Math.round(undone)} ms`);
  await key('Escape');
  await wait(()=>!bar());
  return true;
})()
