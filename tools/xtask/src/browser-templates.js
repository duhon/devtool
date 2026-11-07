(async () => {
  const {assert,wait,visible,click,testid,editorOf,setSource,key,openConsole}=testDriver;
  await openConsole();
  await click(testid('console-service-artemis'));
  const open=async()=>{
    if(testid('console-templates-toggle').getAttribute('aria-pressed')!=='true')
      await click(testid('console-templates-toggle'));
    await wait(()=>document.querySelector('[data-testid="templates-editor"]')
      &&editorOf('templates-editor').getOption('readOnly')===false,
      ()=>`Templates editor did not become editable: ${JSON.stringify({
        pressed:testid('console-templates-toggle').getAttribute('aria-pressed'),
        inert:document.querySelector('#service-views')?.inert,
        status:document.querySelector('[data-testid="templates-status"]')?.textContent,
        panel:document.querySelector('[data-testid="templates-panel"]')?.textContent.slice(0,1000),
        sessions:document.querySelector('[data-testid="console-sessions-toggle"] [data-state]')?.dataset.state,
        editor:!!document.querySelector('[data-testid="templates-editor"]'),
      })}`);
    assert(!document.querySelector('[data-testid="templates-reload"]'),
      'Templates must not render a manual Reload button in the header');
  };
  const drafts=()=>JSON.parse(localStorage.getItem('servicesConsole.templateDrafts')||'{}');
  const entries=()=>editorOf('console-editor').state.completionActive?.data?.list||[];
  // A module removed with its data kept leaves its draft behind; it must neither block nor lose the others.
  const unloaded={text:'@kept\nGET /kept',base:''};
  localStorage.setItem('servicesConsole.templateDrafts',JSON.stringify({'removed-module/check':unloaded}));
  await open();
  assert(!testid('templates-panel').textContent.includes('Invalid Templates draft')
    &&!testid('templates-status').classList.contains('is-error'),
    'A draft of an unloaded module must not break Templates');
  assert(!testid('templates-panel').querySelector('.agent-intro'),'Templates must not show an introductory heading above the editor');
  assert(testid('templates-status').textContent.includes('@@ = @'),'The short escaping hint must live in the status strip');
  // Service modules get template sections with the Templates panel module (step 5).
  assert(testid('templates-sections').querySelectorAll('[role="tab"]').length===0,
    `Kernel services have no template sections yet: ${[...testid('templates-sections').querySelectorAll('[role="tab"]')].map(tab=>tab.textContent)}`);
  assert(JSON.stringify(drafts()['removed-module/check'])===JSON.stringify(unloaded),
    'Templates must keep the draft of an unloaded module until it is installed again');
  await click(testid('console-service-splunk'));
  setSource(editorOf('console-editor'),'#check Splunk sections\n#');
  await key('Control+Space');
  await wait(()=>entries().some(item=>item.text==='#options'),
    ()=>`Splunk section completion: ${entries().map(item=>item.text)}`);
  assert(entries().map(item=>item.text).join(',')==='#endpoint,#headers,#options,#search,#check',
    `Splunk section completion must offer only canonical sections: ${entries().map(item=>item.text)}`);
  await key('Escape');
  for(const section of ['from','to','app','count']) {
    const splunk=editorOf('console-editor');
    setSource(splunk,`#check Removed ${section}\n#endpoint\nhttps://splunk.fixture\n#search\nindex=main\n#${section}\nvalue`);
    splunk.setCursor(6,5);splunk.focus();
    await key('Shift+Enter');
    await wait(()=>testid('console-main-status').textContent.includes(`Unsupported Splunk section #${section}; use #options.`),
      ()=>`Removed #${section} must show an explicit error instead of being sent as SPL: ${testid('console-main-status').textContent}`);
    assert(!testid('console-main-status').classList.contains('is-running'),
      'Rejected Splunk sections must not start a request');
  }
  await click(testid('console-service-artemis'));
  await open();
  const assertExclusivePanel=()=>{
    const panel=testid('console-tool-panel');
    assert([...panel.children].filter(visible).length===1,
      'Only the selected right tool may occupy space; hidden panels must not split the pane');
  };
  const hiddenTemplates=document.querySelector('[data-testid="templates-panel"]');
  for(const tool of ['sessions','hints','variables','dictionary','agent']) {
    await click(testid(`console-${tool}-toggle`));
    await wait(()=>testid(`console-${tool}-toggle`).getAttribute('aria-pressed')==='true');
    assertExclusivePanel();
    assert(hiddenTemplates.hidden && getComputedStyle(hiddenTemplates).display==='none'
      && hiddenTemplates.getBoundingClientRect().height===0,
      `${tool} must completely hide Templates, not leave a second flex panel`);
    await open();
    assertExclusivePanel();
  }
  await click(testid('console-templates-toggle'));
  return true;
})()
