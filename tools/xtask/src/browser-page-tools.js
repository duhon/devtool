(async () => {
  const {assert, wait, input} = testDriver;
  const evaluate = async expression => {
    const result = await input('Test.inspectedPage', {expression});
    assert(!result.exceptionDetails, `Page-tools fixture exception: ${JSON.stringify(result.exceptionDetails)}`);
    return result.result.value;
  };
  const metadata = await evaluate('({origin:location.origin,pageId:String(performance.timeOrigin)})');
  let maxReplyBytes = 0;
  const run = async (tool, args = {}) => {
    const parameters = {...metadata, accessId:'page-tools-acceptance', tool, ...args};
    const result = await evaluate(`(() => {try {return {value:(${pageAgentScript})(${JSON.stringify(parameters)})};}
      catch(error) {return {error:error.message};}})()`);
    if (result.value) {
      const size=new TextEncoder().encode(JSON.stringify(result.value)).length;
      maxReplyBytes=Math.max(maxReplyBytes,size);
      assert(size <= 6000,
        `${tool} exceeded the actual UTF-8 reply budget`);
      if (tool !== 'page_inspect')
        for (const secret of ['field-secret','password-secret','textarea-secret','editable-secret','hidden-secret','cookie-secret','storage-secret'])
          assert(!JSON.stringify(result.value).includes(secret), `${tool} disclosed ${secret}`);
    }
    return result;
  };
  const ok = async (tool, args) => {
    const result = await run(tool, args);
    assert(!result.error, `${tool}: ${result.error}`);
    return result.value;
  };
  await evaluate(`(() => {
    const fixture=document.createElement('section');fixture.id='page-tools-fixture';document.body.append(fixture);
    const large=document.createElement('section');large.id='page-tools-large';fixture.append(large);
    for(let i=0;i<6100;i++) {const p=document.createElement('p');p.textContent='part-'+i+' text';large.append(p);}
    for(let i=0;i<130;i++) {const b=document.createElement('button');b.textContent='control-'+i+' '+ 'Ж🦀'.repeat(90);large.append(b);}
    const tail=document.createElement('p');tail.textContent='beyond-old-node-limit unique-tail-marker';large.append(tail);
    const aria=document.createElement('button');aria.id='page-tools-aria';aria.textContent='short label';
    aria.setAttribute('aria-label','long aria '+'Ж🦀'.repeat(700)+' aria-tail');fixture.append(aria);
    const select=document.createElement('select');select.id='page-tools-options';
    for(let i=0;i<90;i++) {const o=document.createElement('option');o.textContent='Choice '+i;o.value='v'+i;select.append(o);}fixture.append(select);
    select.options[89].label='Long choice '+ '🦀'.repeat(900)+' option-tail';
    select.options[89].value=select.options[0].value;
    const check=document.createElement('input');check.type='checkbox';check.id='page-tools-check';fixture.append(check);
    window.pageToolsChecks=0;check.addEventListener('change',()=>window.pageToolsChecks++);
    const field=document.createElement('input');field.id='page-tools-field';field.value='field-secret';fixture.append(field);
    window.pageToolsKeys=0;field.addEventListener('keydown',event=>{if(event.key==='Enter')window.pageToolsKeys++;});
    const shadow=document.createElement('div');shadow.id='page-tools-shadow';fixture.append(shadow);
    const shadowRoot=shadow.attachShadow({mode:'open'});const sb=document.createElement('button');
    sb.textContent='shadow-unique-marker';sb.addEventListener('click',()=>{window.pageToolsShadowClicked=true;});shadowRoot.append(sb);
    const editor=document.createElement('div');editor.id='page-tools-editor';editor.contentEditable='true';fixture.append(editor);
    const editorText=document.createElement('span');editorText.textContent='editable-secret';
    editor.attachShadow({mode:'open'}).append(editorText);
    const frame=document.createElement('iframe');frame.id='page-tools-frame';
    frame.srcdoc='<button id="inside" onclick="window.clicked=true">frame-unique-marker</button>';fixture.append(frame);
    const cross=document.createElement('iframe');cross.id='page-tools-cross';
    cross.addEventListener('load',()=>{cross.dataset.ready='true';});
    cross.src=${JSON.stringify(fixtureUrl.replace('127.0.0.1','localhost') + '/page-agent-fixture')};fixture.append(cross);
    const scroll=document.createElement('div');scroll.id='page-tools-scroll';
    scroll.style.cssText='height:40px;overflow:auto';const content=document.createElement('div');
    content.style.height='800px';content.textContent='scroll-area';scroll.append(content);fixture.append(scroll);
    const waiting=document.createElement('div');waiting.id='page-tools-wait';waiting.textContent='pending';fixture.append(waiting);
    const phrase=document.createElement('p');fixture.append(phrase);
    for(const text of ['cross-node ','search-marker']) {const span=document.createElement('span');span.textContent=text;phrase.append(span);}
    return true;
  })()`);
  try {
    await wait(async () => await evaluate(`!!document.querySelector('#page-tools-frame').contentDocument?.querySelector('#inside')`));
    await wait(async () => await evaluate(`document.querySelector('#page-tools-cross').dataset.ready === 'true'`));
    const initial = await ok('page_snapshot');
    assert(initial.totalElements > 130 && initial.totalTextChars > 12000 && initial.inaccessibleFrames >= 1,
      'Snapshot still has a total text/control cap or concealed an inaccessible frame');
    let chunk = await ok('page_read', {selector:'#page-tools-large'}), text = '', controls = [], pageCount = 0;
    const snapshotId = chunk.snapshotId;
    for (let pages = 0;;pages++) {
      assert(pages < 300, 'Pagination did not make progress');
      pageCount++;
      text += chunk.text; controls.push(...chunk.elements);
      if (chunk.nextTextOffset === null && chunk.nextElementOffset === null) break;
      const oldText = chunk.textOffset, oldElement = chunk.elementOffset;
      chunk = await ok('page_read', {selector:'#page-tools-large', snapshotId,
        textOffset:chunk.nextTextOffset ?? chunk.totalTextChars,
        elementOffset:chunk.nextElementOffset ?? chunk.totalElements});
      assert(chunk.textOffset > oldText || chunk.elementOffset > oldElement, 'A continuation repeated a page');
    }
    const expected = await evaluate(`Array.from(document.querySelector('#page-tools-large').children).map(e=>e.textContent.trim()+'\\n').join('')`);
    assert(text === expected && controls.length === 130, 'Pagination lost, duplicated or corrupted text/controls/Unicode');
    await evaluate(`document.querySelector('#page-tools-large p').textContent='changed-live-text';true`);
    const frozen = await ok('page_read', {selector:'#page-tools-large', snapshotId});
    assert(frozen.text.startsWith('part-0 text'), 'Continuation did not retain the frozen snapshot');
    assert((await ok('page_read', {selector:'#page-tools-large'})).text.startsWith('changed-live-text'),
      'A fresh read did not see live DOM changes');
    const search = await ok('page_search', {query:'unique-tail-marker'});
    assert(search.items.some(item=>item.kind==='text'), 'Search did not reach beyond the old 5000-node cap');
    assert((await ok('page_search',{query:'cross-node search-marker'})).items.some(item=>item.kind==='text'),
      'Search could not match a phrase split across DOM text nodes');
    const controlSearch = await ok('page_search', {query:'control-129'});
    assert(controlSearch.items.some(item=>item.kind==='control'), 'Search did not reach beyond the old 80-control cap');
    let ariaChunk=await ok('page_read',{selector:'#page-tools-aria',source:'label'}), ariaText='';
    do {
      ariaText+=ariaChunk.text;
      if(ariaChunk.nextTextOffset===null)break;
      ariaChunk=await ok('page_read',{selector:'#page-tools-aria',source:'label',snapshotId:ariaChunk.snapshotId,
        textOffset:ariaChunk.nextTextOffset,elementOffset:ariaChunk.totalElements});
    } while(true);
    assert(ariaText === await evaluate(`document.querySelector('#page-tools-aria').getAttribute('aria-label')`),
      'An accessible label was permanently truncated');
    const many = await ok('page_search', {query:'part-',limit:3});
    const nextMatches = await ok('page_search', {query:'part-',limit:3,snapshotId:many.snapshotId,offset:many.nextOffset});
    assert(many.total === 6099 && many.items.length === 3 && nextMatches.items[0].textOffset > many.items.at(-1).textOffset,
      'Search did not paginate all matches from a stable snapshot');
    const shadow = (await ok('page_search', {query:'shadow-unique-marker'})).items.find(item=>item.kind==='control');
    assert(shadow?.selector.startsWith('@node-'), 'Shadow control did not receive a usable opaque selector');
    await ok('page_click', {selector:shadow.selector});
    assert(await evaluate('window.pageToolsShadowClicked') === true, 'Shadow control could not be activated');
    const frame = (await ok('page_search', {query:'frame-unique-marker'})).items.find(item=>item.kind==='control');
    await ok('page_click', {selector:frame.selector});
    assert(await evaluate(`document.querySelector('#page-tools-frame').contentWindow.clicked`) === true,
      'Same-origin frame control could not be activated');
    await evaluate(`document.querySelector('#page-tools-frame').remove();true`);
    assert((await run('page_click', {selector:frame.selector})).error, 'Detached frame handle remained actionable');
    let choices = [], offset = 0;
    do {
      const result = await ok('page_options', {selector:'#page-tools-options',offset});
      choices.push(...result.items);offset=result.nextOffset;
    } while (offset !== null);
    assert(choices.length === 90 && choices.at(-1).index === 89, 'Select choices were silently truncated');
    let optionChunk=await ok('page_read',{selector:choices.at(-1).selector}), optionText='';
    do {
      optionText+=optionChunk.text;
      if(optionChunk.nextTextOffset===null)break;
      optionChunk=await ok('page_read',{selector:choices.at(-1).selector,snapshotId:optionChunk.snapshotId,
        textOffset:optionChunk.nextTextOffset,elementOffset:optionChunk.totalElements});
    } while(true);
    assert(optionText === await evaluate(`document.querySelector('#page-tools-options').options[89].label`),
      'A long option label could not be read completely');
    assert((await run('page_read',{selector:choices.at(-1).selector,snapshotId:optionChunk.snapshotId,textOffset:13})).error,
      'A caller could split a Unicode code point with a manually calculated offset');
    assert((await run('page_read',{selector:'#page-tools-large',snapshotId:optionChunk.snapshotId})).error,
      'A continuation silently read a different requested scope');
    assert((await ok('page_select', {selector:'#page-tools-options',optionIndex:89})).matchesRequested,
      'Selection by option index was not verified');
    await ok('page_set_checked', {selector:'#page-tools-check',checked:true});
    await ok('page_set_checked', {selector:'#page-tools-check',checked:true});
    assert(await evaluate('window.pageToolsChecks') === 1, 'Idempotent checkbox setting toggled or fired twice');
    assert((await ok('page_fill', {selector:'#page-tools-field',value:'written-value'})).matchesRequested,
      'Fill did not verify the actual state');
    assert((await ok('page_focus', {selector:'#page-tools-field'})).focused, 'Focus was not observed');
    await ok('page_press_key', {selector:'#page-tools-field',key:'Enter'});
    assert(await evaluate('window.pageToolsKeys') === 1, 'Synthetic key event did not reach the site listener');
    assert((await run('page_press_key', {selector:'#password',key:'Enter'})).error, 'Key tool accessed a password field');
    assert((await run('page_read', {selector:'#editable'})).error, 'Read tool exposed contenteditable contents');
    assert((await run('page_wait', {selector:'#notes',state:'text',text:'textarea-secret'})).error,
      'Wait became a field-content oracle');
    await ok('page_scroll', {selector:'#page-tools-scroll',y:500});
    assert(await evaluate(`document.querySelector('#page-tools-scroll').scrollTop`) === 500,
      'Scrolling did not address the requested container');
    assert(!(await ok('page_wait', {selector:'#page-tools-wait',state:'text',text:'done'})).matched,
      'A wait claimed unobserved state');
    await evaluate(`document.querySelector('#page-tools-wait').textContent='done';true`);
    assert((await ok('page_wait', {selector:'#page-tools-wait',state:'text',text:'done'})).matched,
      'A wait failed to observe actual content');
    await evaluate(`(() => {
      document.querySelector('#page-tools-field').dispatchEvent(new PointerEvent('pointermove',{bubbles:true,clientX:21,clientY:34}));
      window.dispatchEvent(new ErrorEvent('error',{message:'page-tools-new-error'}));return true;
    })()`);
    const context = await ok('page_context');
    assert(context.pointer?.element.selector === '#page-tools-field' && context.pointer.x === 21
      && context.pointer.ageMs >= 0, 'Pointer context did not identify its age and target');
    await evaluate(`document.querySelector('#page-tools-editor').shadowRoot.querySelector('span')
      .dispatchEvent(new PointerEvent('pointermove',{bubbles:true,composed:true}));true`);
    const privateTarget=(await ok('page_context')).pointer.element.selector;
    assert((await run('page_read',{selector:privateTarget})).error,
      'Shadow-boundary traversal exposed editable contents');
    assert((await run('page_press_key',{selector:privateTarget,key:'Enter'})).error,
      'Shadow-boundary traversal bypassed the editable-input restriction');
    const command=(method,params)=>input('Test.inspectedCommand',{method,params});
    const tree=await command('DOM.getDocument',{depth:0});
    const selected=await command('DOM.querySelector',{nodeId:tree.root.nodeId,selector:'#page-tools-check'});
    await command('DOM.setInspectedNode',{nodeId:selected.nodeId});
    assert((await ok('page_context')).selected?.selector === '#page-tools-check',
      'The real DevTools selected-node context was not exposed');
    await command('DOM.disable',{});
    assert((await ok('page_errors')).items.some(item=>item.message==='page-tools-new-error'),
      'New page errors were not available');
    const current = await ok('page_snapshot');
    assert((await run('page_snapshot', {snapshotId:current.snapshotId,pageId:'stale'})).error,
      'A continuation accepted another document identity');
    assert((await run('page_snapshot', {textOffset:10})).error, 'Offsets without a snapshot silently skipped content');
    for (let index=0;index<4;index++) await ok('page_read',{selector:'#page-tools-wait'});
    assert((await run('page_snapshot', {snapshotId:current.snapshotId})).error,
      'Eviction did not explicitly reject a stale snapshot');
    let dom = await ok('page_inspect', {selector:'#page-tools-large'}), nodes = [], domPages = 0;
    for (;;) {
      nodes.push(...dom.items); domPages++;
      assert(domPages < 1000, 'DOM traversal did not make progress');
      if (dom.nextOffset === null) break;
      const previous = dom.offset;
      dom = await ok('page_inspect', {snapshotId:dom.snapshotId,offset:dom.nextOffset});
      assert(dom.offset > previous, 'DOM continuation repeated nodes');
    }
    assert(nodes.length === 6232 && nodes.some(node => node.text.includes('unique-tail-marker'))
      && nodes.filter(node => node.tag === 'button').length === 130,
      'DOM tree omitted nodes beyond the old page limits');
    await evaluate(`(() => {
      const form=document.createElement('section');form.id='page-tools-dom';document.body.append(form);
      const field=document.createElement('input');field.id='dom-value';field.name='team_m2_branch_ce';
      field.setAttribute('value','old-default');field.value='2.4-develop';field.setAttribute('data-token','token-visible');
      form.append(field);
      const hidden=document.createElement('input');hidden.id='dom-hidden';hidden.type='hidden';hidden.value='hidden-secret';form.append(hidden);
      const password=document.createElement('input');password.id='dom-password';password.type='password';
      password.value='password-secret';form.append(password);
      const select=document.createElement('select');select.id='dom-select';
      for(const value of ['one','two']){const option=document.createElement('option');option.value=value;option.textContent=value;select.append(option);}
      select.selectedIndex=1;form.append(select);
      const text=document.createElement('textarea');text.id='dom-textarea';text.value='textarea-secret';form.append(text);
      const check=document.createElement('input');check.id='dom-checked';check.type='checkbox';check.checked=true;form.append(check);
      const host=document.createElement('div');host.id='dom-shadow';form.append(host);
      const nested=document.createElement('input');nested.value='shadow-field-value';host.attachShadow({mode:'open'}).append(nested);
      const frame=document.createElement('iframe');frame.id='dom-frame';
      frame.srcdoc='<input value="frame-field-value">';form.append(frame);
      return true;
    })()`);
    try {
      await wait(async () => await evaluate(`!!document.querySelector('#dom-frame').contentDocument?.querySelector('input')`));
      const tree = await ok('page_inspect', {selector:'#page-tools-dom'});
      const items = tree.items;
      assert(items.find(node => node.selector === '#dom-value')?.value === '2.4-develop'
        && items.find(node => node.selector === '#dom-password')?.value === 'password-secret'
        && items.find(node => node.selector === '#dom-hidden')?.value === 'hidden-secret'
        && items.find(node => node.selector === '#dom-hidden')?.visible === false,
        'DOM inspection filtered live, password or hidden field values');
      assert(items.find(node => node.selector === '#dom-select')?.selectedIndex === 1
        && items.find(node => node.selector === '#dom-checked')?.checked === true
        && items.find(node => node.value === 'shadow-field-value')?.relation === 'shadow',
        'DOM inspection lost selected/checked state or shadow structure');
      const node = await ok('page_inspect', {selector:'#dom-value',mode:'node'});
      const details = JSON.parse(node.data);
      assert(details.attributes.value === 'old-default' && details.properties.value === '2.4-develop'
        && details.attributes['data-token'] === 'token-visible',
        'DOM details substituted stale HTML attributes or redacted attributes');
      await evaluate(`document.querySelector('#dom-value').value='changed';true`);
      assert((await ok('page_inspect', {snapshotId:tree.snapshotId})).items
        .find(node => node.selector === '#dom-value').value === '2.4-develop',
        'DOM continuation was not frozen');
      assert(JSON.parse((await ok('page_inspect',{selector:'#dom-value',mode:'node'})).data)
        .properties.value === 'changed', 'Fresh DOM inspection missed changed live values');
      await evaluate(`document.querySelector('#dom-value').value='Ж🦀'.repeat(16000)+' exact-value-tail';true`);
      const preview = await ok('page_inspect',{selector:'#dom-value'});
      assert(preview.items[0].valueTruncated === true, 'Long field preview concealed truncation');
      let chunk = await ok('page_inspect',{selector:'#dom-value',mode:'node'}), data = '', valuePages = 0;
      const longId = chunk.snapshotId;
      await evaluate(`document.querySelector('#dom-value').value='changed-again';true`);
      for (;;) {
        data += chunk.data; valuePages++;
        if (chunk.nextOffset === null) break;
        const previous = chunk.offset;
        chunk = await ok('page_inspect',{snapshotId:longId,offset:chunk.nextOffset});
        assert(chunk.offset > previous && valuePages < 200, 'Long DOM value continuation stalled');
      }
      assert(JSON.parse(data).properties.value === 'Ж🦀'.repeat(16000)+' exact-value-tail' && valuePages > 1,
        'Long live value was truncated, corrupted or changed during continuation');
      assert((await run('page_inspect',{snapshotId:longId,mode:'tree'})).error,
        'DOM continuation accepted a different mode');
      assert((await run('page_read',{snapshotId:longId,selector:'#dom-value'})).error,
        'Rendered page reader accepted a DOM snapshot');
      assert((await run('page_inspect',{selector:'#dom-value',offset:1})).error,
        'DOM offsets without a frozen snapshot silently skipped data');
      const frameTree = await ok('page_inspect',{selector:'#dom-frame'});
      assert(frameTree.items.some(node => node.relation === 'frame' && node.tag === 'html'),
        'DOM inspection omitted the accessible frame relation');
      globalThis.domToolsReceipt = {nodes:nodes.length,pages:domPages,valueChars:JSON.parse(data).properties.value.length,valuePages};
    } finally {
      await evaluate(`document.querySelector('#page-tools-dom')?.remove();true`);
    }
    globalThis.pageToolsReceipt={textChars:text.length,textUtf8Bytes:new TextEncoder().encode(text).length,
      controls:controls.length,pages:pageCount,maxReplyBytes};
    await ok('access_cleanup');
    assert(!await evaluate(`!!window.__servicesDevtoolsPageAccess`), 'Page caches/listeners survived access revocation');
  } finally {
    await run('access_cleanup');
    await evaluate(`document.querySelector('#page-tools-fixture')?.remove();true`);
  }
  return true;
})()
