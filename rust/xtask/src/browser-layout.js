(async()=>{
  const {assert,wait,click,testid,cm,setSource,key,type,input,openConsole,activateSession,sessionLibrary}=testDriver;
  await openConsole();
  const initialSession=Number(testid('console-session-name').dataset.sessionId);
  await click(testid('console-service-rest'));
  let editor=cm('console-editor');
  const selectionSource='#check Light\nVISIBLE '+' '.repeat(35)+'\n\n#check Dark\nVISIBLE '+' '.repeat(35);
  setSource(editor,selectionSource);
  await wait(()=>editor.getOption('servicesChecks')?.join(',')==='0,3');
  editor.setCursor(1,0);
  editor.scrollTo(0,0);
  const point=(line,ch)=>{
    const from=editor.charCoords({line,ch},'window'),to=editor.charCoords({line,ch:ch+1},'window');
    return {x:(from.left+to.left)/2,y:(from.top+from.bottom)/2};
  };
  const capture=async()=>{
    const screenshot=await input('Page.captureScreenshot',{format:'png'});
    const image=new Image();
    image.src=`data:image/png;base64,${screenshot.data}`;
    await image.decode();
    const canvas=document.createElement('canvas');
    canvas.width=image.width;canvas.height=image.height;
    const context=canvas.getContext('2d');
    context.drawImage(image,0,0);
    const scale=image.width/window.innerWidth;
    return {context,scale};
  };
  const glyphPixels=({context,scale})=>{
    const from=editor.charCoords({line:1,ch:1},'window'),to=editor.charCoords({line:1,ch:6},'window');
    return context.getImageData(Math.floor(from.left*scale),Math.floor((from.top+3)*scale),
      Math.max(1,Math.floor((to.left-from.left)*scale)),Math.max(1,Math.floor((from.bottom-from.top-6)*scale))).data;
  };
  const reference=glyphPixels(await capture());
  const start=editor.charCoords({line:1,ch:0},'window'),end=point(4,30);
  await input('Input.dispatchMouseEvent',{type:'mousePressed',x:start.left+1,y:(start.top+start.bottom)/2,button:'left',clickCount:1});
  await input('Input.dispatchMouseEvent',{type:'mouseMoved',x:end.x,y:end.y,button:'left',buttons:1});
  await input('Input.dispatchMouseEvent',{type:'mouseReleased',x:end.x,y:end.y,button:'left',clickCount:1});
  assert(editor.somethingSelected()&&editor.getCursor('from').line===1&&editor.getCursor('to').line===4,
    'A real mouse drag must select across both alternating check backgrounds');
  const paint=async(label,continuous=true)=>{
    const shot=await capture();
    const {context,scale}=shot;
    const pixel=(line,ch)=>{
      const {x,y}=point(line,ch);
      return [...context.getImageData(Math.floor(x*scale),Math.floor(y*scale),1,1).data].slice(0,3);
    };
    const at=(x,y)=>[...context.getImageData(Math.floor(x*scale),Math.floor(y*scale),1,1).data].slice(0,3);
    const close=(actual,expected)=>actual.every((channel,index)=>Math.abs(channel-expected[index])<=3);
    for(const line of [1,4]) assert(close(pixel(line,20),line===1?[46,57,69]:[39,50,60]),
      `${label}: selection must actually paint above check ${line===1?'light':'dark'} background: ${pixel(line,20)}`);
    assert(close(pixel(4,38),[37,39,42]),
      `${label}: unselected text must retain the dark check background`);
    for(const line of [1,3]){
      const end=editor.charCoords({line,ch:editor.getLine(line).length},'window');
      assert(close(at(end.left+20,(end.top+end.bottom)/2),line===1?[45,48,53]:[37,39,42]),
        `${label}: selection must not paint empty space after line ${line}`);
    }
    if(continuous){
      assert(close(pixel(2,0),[45,48,53]),`${label}: a selected newline must not fill an empty row`);
      const seam=editor.charCoords({line:4,ch:1},'window');
      assert(Math.max(...at(point(4,1).x,seam.top))<100,
        `${label}: adjacent selected rows must not have a bright internal seam`);
      const edge=editor.charCoords({line:1,ch:20},'window');
      const border=at(point(1,20).x,edge.top);
      assert(border[0]>75&&border[1]>110&&border[2]>150,
        `${label}: the external selection contour must remain bright: ${border}`);
      const muted=[...editor.getWrapperElement().querySelectorAll('pre.CodeMirror-line')]
        .find(line=>line.textContent.startsWith('#check Dark')).querySelector('.cm-comment');
      assert(muted,'The fixture must exercise a dim section header');
      const color=getComputedStyle(muted).color.match(/\d+/g).slice(0,3).map(Number);
      const luminance=rgb=>rgb.map(value=>{
        value/=255;return value<=0.04045?value/12.92:((value+0.055)/1.055)**2.4;
      }).reduce((sum,value,index)=>sum+value*[0.2126,0.7152,0.0722][index],0);
      assert((luminance([37,39,42])+0.05)/(luminance([39,50,60])+0.05)>=0.85
        &&luminance(color)>luminance([39,50,60]),
        `${label}: a dim header must retain at least 85% of its original contrast`);
    }
    assert(editor.getWrapperElement().querySelectorAll('.CodeMirror-selection-outline').length>0
      &&[...editor.getWrapperElement().querySelectorAll('.CodeMirror-selected')]
        .every(rect=>getComputedStyle(rect).boxShadow==='none'),
      `${label}: one external contour must replace individual rectangle borders`);
    const pixels=glyphPixels(shot);
    let glyphs=0;
    for(let i=0;i<pixels.length;i+=4){
      const before=[reference[i],reference[i+1],reference[i+2]],after=[pixels[i],pixels[i+1],pixels[i+2]];
      if(before.reduce((sum,value,index)=>sum+Math.abs(value-[45,48,53][index]),0)>100
        &&before.reduce((sum,value,index)=>sum+Math.abs(value-[51,127,189][index]),0)>50
        &&close(after,before)) glyphs++;
    }
    assert(glyphs>5,`${label}: selected glyph pixels must preserve their actual syntax colors above the fill: ${JSON.stringify({
      glyphs,line:[...editor.getWrapperElement().querySelectorAll('pre.CodeMirror-line')].find(line=>line.textContent.startsWith('VISIBLE'))?.outerHTML,
    })}`);
  };
  await paint('Focused drag');
  testid('console-service-rest').focus();
  await wait(()=>!editor.hasFocus());
  await paint('Unfocused selection');
  editor.focus();
  editor.setSelections([1,4].map(line=>({anchor:{line,ch:0},head:{line,ch:30}})));
  await paint('Multiple selections',false);
  assert(editor.getValue()===selectionSource,'Changing selection must never modify the source document');
  const foldedSource='#check Folded\n'+('payload\n'.repeat(20000))+'#check Tail\nend';
  setSource(editor,foldedSource);
  await wait(()=>editor.getOption('servicesChecks')?.length===2);
  const folded=editor.markText({line:0,ch:13},{line:20000,ch:7},{collapsed:true});
  // A single wall-clock sample is noisy on a loaded machine; painting every hidden line is slow on every attempt.
  const foldSelections=[];
  for(let attempt=0;attempt<3;attempt++){
    editor.setCursor({line:0,ch:0});
    const foldStart=performance.now();
    editor.setSelection({line:0,ch:0},{line:editor.lastLine(),ch:3});
    const ms=performance.now()-foldStart;
    const painted=editor.getWrapperElement().querySelectorAll('.CodeMirror-selected').length;
    assert(painted<20,`Selecting across 20,000 folded lines must only paint rendered visual lines: ${painted} painted rectangles`);
    foldSelections.push(ms);
    if(ms<200) break;
  }
  assert(Math.min(...foldSelections)<200,
    `Selecting across 20,000 folded lines must take under 200ms: ${foldSelections.map(ms=>ms.toFixed(1))}ms`);
  assert(editor.getAllMarks().length===1&&editor.getSelection()===foldedSource,
    'Text-only painting must preserve folds and copy every selected hidden character');
  folded.clear();
  const base='#check First\n#endpoint\nGET https://fixture.invalid/first';
  const checks=()=>editor.getOption('servicesChecks')||[];
  const reset=async text=>{
    setSource(editor,text);
    await wait(()=>checks().length===text.split('\n').filter(line=>/^#(?:check|request)(?:\s|$)/i.test(line)).length);
    editor.clearHistory();
  };
  await reset(base+'\n');
  await type('#check');
  assert(editor.getValue()===base+'\n#check','An unfinished marker must not reformat while its name is being typed');
  editor.clearHistory();
  await key('Space');
  assert(editor.getValue()===base+'\n\n#check ','Typing a new check must add a blank immediately before it');
  assert(editor.getCursor().line===4&&editor.getCursor().ch===7,'The cursor must follow the shifted check');
  editor.undo();
  assert(editor.getValue()===base+'\n#check','One undo must remove both the typed space and separator');
  editor.redo();
  assert(editor.getValue()===base+'\n\n#check ','Redo must restore the exact check separator');
  await key('Escape');
  await reset(base+'\n#');
  await key('Control+Space');
  await wait(()=>editor.state.completionActive?.data?.list.some(item=>item.text==='#check'));
  const completion=editor.state.completionActive;
  completion.pick(completion.data,completion.data.list.findIndex(item=>item.text==='#check'));
  assert(editor.getValue()===base+'\n\n#check ','Directive completion must add the missing separator');
  editor.undo();
  assert(editor.getValue()===base+'\n#','Completion and separator must undo together');
  const paste=text=>{
    const clipboardData=new DataTransfer();
    clipboardData.setData('text/plain',text);
    editor.getInputField().dispatchEvent(new ClipboardEvent('paste',{clipboardData,bubbles:true,cancelable:true}));
  };
  for(const [before,inserted,expected] of [
    ['', '#check First', '#check First'],
    [base+'\n\n','#check Second',base+'\n\n#check Second'],
    [base+'\n','#check Second\n#body\n{}\n#request Third',base+'\n\n#check Second\n#body\n{}\n\n#request Third'],
    [base+'\n','#checker',base+'\n#checker'],
  ]){
    await reset(before);
    paste(inserted);
    await wait(()=>editor.getValue()===expected,()=>`Unexpected check paste: ${JSON.stringify(editor.getValue())}`);
    editor.undo();
    assert(editor.getValue()===before,'A multi-check paste and all separators must undo as one change');
    editor.redo();
    assert(editor.getValue()===expected,'Redo must preserve all inserted separators');
  }
  const probe=document.createElement('div');
  document.body.append(probe);
  const large=CodeMirror(probe,{value:base+'\n'+('payload\n'.repeat(20000))+'#check Last',
    servicesChecks:[0,20003]});
  const original=large.getValue();
  const last=large.lastLine(), getLine=large.getLine.bind(large);
  let reads=0;
  large.getLine=line=>{reads++;return getLine(line);};
  large.getValue=()=>{throw new Error('Separator helper must not copy the full document');};
  assert(CodeMirror.servicesSeparateChecks(large,{from:{line:last,ch:0},text:['#check Last'],origin:'paste'})===1
    &&reads<=3,'Separating a check in a large document must read only the changed line and predecessor');
  large.getValue=()=>original;
  probe.remove();

  const open=async panel=>{
    const toggle=testid(`console-${panel}-toggle`);
    if(toggle.getAttribute('aria-pressed')!=='true') await click(toggle);
    await wait(()=>document.querySelector('[data-testid="console-pane-resizer"]')?.dataset.panel===panel);
    await wait(()=>Number(testid('console-pane-resizer').getAttribute('aria-valuemax'))<16384);
  };
  const width=()=>document.querySelector('.console-right-pane').getBoundingClientRect().width;
  const setWidth=async(value)=>{
    const handle=testid('console-pane-resizer'), rect=handle.getBoundingClientRect();
    const x=rect.left+rect.width/2,y=rect.top+16, end=x+width()-value;
    await input('Input.dispatchMouseEvent',{type:'mousePressed',x,y,button:'left',clickCount:1});
    await input('Input.dispatchMouseEvent',{type:'mouseMoved',x:end,y,button:'left',buttons:1});
    await input('Input.dispatchMouseEvent',{type:'mouseReleased',x:end,y,button:'left',clickCount:1});
    await wait(()=>Math.abs(width()-value)<1,()=>`Expected panel width ${value}, got ${width()}`);
  };
  const create=async(source,name)=>{
    await open('sessions');
    await wait(()=>testid('console-sessions').dataset.busy==='false');
    const field=testid('session-new-name');
    field.value=name;
    field.dispatchEvent(new InputEvent('input',{bubbles:true}));
    await click(testid(`session-copy-${source}`));
    await wait(()=>testid('console-session-name').textContent===name);
    editor=cm('console-editor');
    return Number(testid('console-session-name').dataset.sessionId);
  };
  const a=await create(0,'Layout A');
  const beforeSettings=editor.getValue();
  const beforeSettingsHeader=document.querySelector('.console-top-bar').getBoundingClientRect().height;
  await open('settings');
  const settings=testid('settings-panel'),settingsToggle=testid('console-settings-toggle');
  assert(testid('settings-version').textContent===fixtureVersion,
    'Application Settings must show the exact compiled release version in native and HTTP interfaces');
  if(globalThis.chrome?.runtime?.getManifest){
    assert(testid('settings-version').textContent===chrome.runtime.getManifest().version,
      'The Settings version must match the installed extension version');
  }
  await wait(()=>!testid('settings-update-status').textContent.includes('Checking'));
  assert(testid('settings-version').closest('.console-settings-version')?.textContent===`Current version${fixtureVersion}`,
    `Settings must label the current version ${fixtureVersion}: ${settings.textContent}`);
  const editable=[...settings.querySelectorAll('input,select,textarea,.CodeMirror')]
    .filter(field=>!field.closest('[data-testid="agent-settings"]'))
    .filter(field=>!(field.matches('[data-testid="settings-module-file"]')&&field.type==='file'&&field.hidden));
  assert(!editable.length,
    `Application Settings must not contain editable fields besides the Agent model picker and the hidden module file picker: ${editable.map(field=>field.outerHTML)}`);
  const updateCheck=document.querySelector('[data-testid="settings-update-check"]');
  assert(globalThis.chrome?.runtime?.getManifest ? updateCheck : !updateCheck,
    'Settings must expose authenticated update checks only in the Chrome extension');
  // Opening Settings loads the Copilot model catalog; its light runs until that finishes.
  await wait(()=>settingsToggle.querySelector('.console-state-light').dataset.state!=='running','Copilot model catalog load in Settings');
  assert(settingsToggle.classList.contains('active')
    && settingsToggle.querySelector('.console-state-light').dataset.state==='ready'
    && [...document.querySelectorAll('.console-toolbar [aria-pressed="true"]')].length===1,
    ()=>`Settings must exclusively activate its right-hand tool without claiming an available update: light=${settingsToggle.querySelector('.console-state-light')?.dataset.state}, pressed=${[...document.querySelectorAll('.console-toolbar [aria-pressed="true"]')].map(button=>button.dataset.testid)}`);
  assert(testid('settings-title').classList.contains('tab-button')
    && getComputedStyle(testid('settings-title')).backgroundColor
      ===getComputedStyle(document.querySelector('.console-tab-strip > .tab-button.active')).backgroundColor
    && document.querySelector('.console-top-bar').getBoundingClientRect().height===beforeSettingsHeader,
    'Settings must use the shared active heading without increasing the top bar height');
  assert(editor.getValue()===beforeSettings,'Opening application Settings must not alter the current check');
  await click(settingsToggle);
  // Once visited, Settings stays mounted (hidden) so its model catalog is not reloaded.
  const settingsHidden=()=>document.querySelector('[data-testid="settings-panel"]')?.hidden!==false;
  await wait(settingsHidden,'Settings panel hidden after toggling it off');
  assert(document.querySelector('.console-right-pane').hidden,
    'Clicking Settings again must close the right pane');
  await open('settings');
  await setWidth(360);
  await open('history');
  assert(settingsHidden(),
    'Another right tool must hide the application Settings panel');
  await setWidth(410);
  const tabs=document.querySelector('.console-tab-strip');
  const top=document.querySelector('.console-top-bar').getBoundingClientRect().height;
  const header=document.querySelector('.console-panel-heading:not([hidden])').getBoundingClientRect();
  assert(Math.abs(header.left-document.querySelector('.console-right-pane').getBoundingClientRect().left)<1,
    'The right header and panel must share one resizable grid boundary');
  await open('variables');
  await setWidth(520);
  await open('history');
  assert(Math.abs(width()-410)<1,'Each tool must remember its own width in the same session');
  const rail=testid('console-check-resizer');
  rail.focus();
  await key('ArrowRight');
  await wait(()=>testid('console-check-menu').getBoundingClientRect().width===36);
  let saved=await sessionLibrary();
  assert(saved.sessions.find(session=>session.id===a).snapshot.layout.check_menu_width===36
    &&saved.sessions.find(session=>session.id===a).snapshot.layout.panel_widths.history===410
    &&saved.sessions.find(session=>session.id===a).snapshot.layout.panel_widths.settings===360,
    'Both rail and right-panel sizes must be persisted in the active session');
  const b=await create(a,'Layout B');
  await open('settings');
  assert(Math.abs(width()-360)<1,'A copied session must inherit its independent Settings panel width');
  await open('history');
  assert(Math.abs(width()-410)<1,'Creating from a named session must copy its layout');
  await setWidth(460);
  await activateSession(a);
  await open('history');
  assert(Math.abs(width()-410)<1&&testid('console-check-menu').getBoundingClientRect().width===36,
    'Switching sessions must restore their independent left and right widths');
  for(const panel of ['history','templates','hints','variables','dictionary','agent','sessions','settings']){
    await open(panel);
    const handle=testid('console-pane-resizer');
    handle.focus();
    await key('Home');
    const minimum=Number(handle.getAttribute('aria-valuemin'));
    assert(Math.abs(width()-minimum)<1,`${panel}: panel must stop at its own minimum width`);
    await key('ArrowRight');
    assert(Math.abs(width()-minimum)<1,`${panel}: minimum must also constrain keyboard resizing`);
    assert(document.querySelector('.console-top-bar').getBoundingClientRect().height===top,
      'Narrow tabs must not wrap or increase the shared top bar height');
    handle.focus();
    await key('End');
    assert(tabs===document.querySelector('.console-tab-strip')||!tabs.isConnected,
      'Resizing must not recreate the service tab strip');
    const strip=document.querySelector('.console-tab-strip');
    assert(strip.scrollWidth<=strip.clientWidth+1&&getComputedStyle(strip).flexWrap==='nowrap',
      'Service tabs must shrink into the available width rather than wrap or overflow');
    assert([...strip.querySelectorAll('.tab-button')].some(tab=>tab.scrollWidth>tab.clientWidth)
      &&[...strip.querySelectorAll('.tab-button')].every(tab=>getComputedStyle(tab).textOverflow==='ellipsis'),
      'Narrow service tab names must be clipped with ellipses');
  }
  await open('history');
  await setWidth(410);
  const frame=document.createElement('iframe');
  frame.style.cssText='position:fixed;left:-10000px;top:0;width:1197px;height:960px';
  frame.src=location.href;
  document.body.append(frame);
  try{
    await wait(()=>frame.contentDocument?.querySelector('[data-testid="console-session-name"]'));
    const doc=frame.contentDocument;
    doc.querySelector('[data-testid="console-sessions-toggle"]').click();
    await wait(()=>doc.querySelector('[data-testid="console-sessions"]')?.dataset.busy==='false');
    doc.querySelector(`[data-testid="session-switch-${a}"]`).click();
    await wait(()=>doc.querySelector('[data-testid="console-session-name"]')?.dataset.sessionId===String(a));
    doc.querySelector('[data-testid="console-history-toggle"]').click();
    await wait(()=>Math.abs(frame.contentDocument.querySelector('.console-right-pane').getBoundingClientRect().width-410)<1
      &&frame.contentDocument.querySelector('[data-testid="console-check-menu"]').getBoundingClientRect().width===36,
      'A fresh application must restore the selected named session layout from IndexedDB');
  }finally{frame.remove();}
  await activateSession(0);
  await open('sessions');
  for(const id of [b,a]){
    await wait(()=>testid('console-sessions').dataset.busy==='false');
    await click(testid(`session-delete-${id}`));
    await wait(()=>!document.querySelector(`[data-testid="session-delete-${id}"]`));
  }
  await click(testid('console-sessions-toggle'));
  await activateSession(initialSession);
  return true;
})()
