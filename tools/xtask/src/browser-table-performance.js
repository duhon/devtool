(async () => {
  // Budgets measured on the reference machine; testDriver.budget scales them to this one.
  const TOKENIZE_MS=200, FOLD_MS=3, VERTICAL_FOLD_MS=30, SCALE_SLACK_MS=5;
  // How much more a whole-table selection may cost than one screen of it.
  const SCALE=3;
  const {assert, wait, click, testid, editorOf, setSource, budget} = testDriver;
  await wait(() => document.querySelector('.console-editor-host .editor'), 'Console editor shown');
  await click(testid('console-service-mysql'));
  const mysqlConsole = editorOf('console-editor'), previous = mysqlConsole.getValue();
  setSource(mysqlConsole, '#check Module ready\n#payload\nselect 1');
  await wait(() => mysqlConsole.getLineTokens(2, true).some(token => token.type === 'keyword'),
    'The MySQL module must highlight before the standalone editor uses it');
  setSource(mysqlConsole, previous);
  // TEMPORARY: a standalone editor until this scenario is rewritten after the editor rewrite.
  assert(typeof window.__servicesMountEditor === 'function', 'The standalone editor hook must exist');
  const payload = JSON.stringify({name:'Unicode 😀', escaped:'a"b\\c',
    values:Array.from({length:100},(_,i)=>({id:i,enabled:true}))});
  const width = Array.from(payload).length;
  const border = `+-------+${'-'.repeat(width+2)}+`;
  const header = `| id    | payload${' '.repeat(width-7)} |`;
  const prefix = ['#check Performance','#options','Columns-Type: id=number payload=json',
    '#payload','select fixture','#responce OK',border,header,border];
  const row = i => `| ${String(i).padEnd(5)} | ${payload} |`;
  const region = {request:0,header:5,end:prefix.length+10001};
  const source = [...prefix,...Array.from({length:10000},(_,i)=>row(i)),border].join('\n');
  const host=document.createElement('div');
  host.style.cssText='position:fixed;inset:0;z-index:2147483647;background:#282a36';
  document.body.appendChild(host);
  let editor;
  try {
    editor=window.__servicesMountEditor(host,{value:source,service:'mysql',responses:[region],checks:[0]});
    // The view renders after this task, so these lines are still cold.
    const started = performance.now();
    for (let line=0;line<prefix.length+30;line++)
      assert(editor.getLineTokens(line,true).length>0,'The table lexer must produce tokens');
    const tokenizeMs = performance.now()-started;
    assert(tokenizeMs<budget(TOKENIZE_MS),`30 dense JSON rows must tokenize within ${budget(TOKENIZE_MS)}ms, got ${tokenizeMs}ms`);
    await new Promise(resolve=>requestAnimationFrame(resolve));
    editor.setSize(1000,400);
    const foldStart=performance.now();
    for(let line=prefix.length;line<prefix.length+30;line++)
      assert(!editor.foldRange({line,ch:0}),'Data rows must never fold');
    const foldMs=performance.now()-foldStart;
    assert(foldMs<budget(FOLD_MS),`Owned output fold checks must not re-tokenize rows: ${foldMs}ms, budget ${budget(FOLD_MS)}ms`);
    editor.scrollTo(0,editor.heightAtLine(9000,'local'));
    await wait(()=>editor.getViewport().from>8900,'A large document must scroll to distant rows');
    await wait(()=>[...host.querySelectorAll('pre.editor-line')].some(line=>line.textContent.startsWith('| 899')),
      'Distant rows must render');
    assert(host.querySelectorAll('.editor-code>div').length<100,
      '10,000 rows must render only a bounded viewport, not every row');
    assert(host.querySelectorAll('.editor-band-even').length>0
      && host.querySelectorAll('.editor-band-even').length<100,
      'Check backgrounds must decorate only the virtualized viewport, not allocate per result row');
    assert([...host.querySelectorAll('.tok-property')].some(s=>s.textContent==='"enabled"'),
      'Cold scrolling must preserve JSON colors without replaying the whole response');
    assert(editor.getValue()===source,'Performance optimization must not truncate or alter text');
    editor.setSelection({line:9000,ch:0},{line:9001,ch:0});
    assert(editor.getSelection()===`${row(9000-prefix.length)}\n`,
      'A distant row must remain exactly copyable, including Unicode and JSON escapes');
    // Selecting the whole table must cost about as much as selecting one screen of it.
    const select = async (from,to) => {
      editor.setCursor({line:9000,ch:0},{scroll:false});
      await new Promise(resolve=>setTimeout(resolve,0));
      const started=performance.now();
      editor.setSelection(from,to,{scroll:false});
      const selectMs=performance.now()-started;
      // The view paints after this task: until the next one is painting time.
      await new Promise(resolve=>setTimeout(resolve,0));
      return {selectMs,paintMs:performance.now()-started-selectMs};
    };
    const median = async (from,to) => {
      const runs=[];
      for(let i=0;i<3;i++) runs.push(await select(from,to));
      const middle=key=>runs.map(run=>run[key]).sort((a,b)=>a-b)[1];
      return {selectMs:middle('selectMs'),paintMs:middle('paintMs')};
    };
    const screen=await median({line:9000,ch:0},{line:9020,ch:0});
    const whole=await median({line:0,ch:0},{line:editor.lastLine(),ch:editor.getLine(editor.lastLine()).length});
    const {selectMs,paintMs:selectionPaintMs}=whole;
    assert(selectionPaintMs<screen.paintMs*SCALE+budget(SCALE_SLACK_MS),
      `Painting a 10,000-row selection must cost about one screen: ${selectionPaintMs}ms vs ${screen.paintMs}ms`);
    assert(selectMs<screen.selectMs*SCALE+budget(SCALE_SLACK_MS),
      `Selecting 10,000 rows must cost about one screen: ${selectMs}ms vs ${screen.selectMs}ms`);
    assert(host.querySelectorAll('.editor-selected').length>0
      &&host.querySelectorAll('.editor-selected').length<100
      &&host.querySelectorAll('.editor-selection-outline').length===1,
      'A whole-table selection must allocate only visible-row rectangles and one external contour');
    assert(editor.getAllMarks().length===0&&editor.getValue()===source,
      'Selection painting must not allocate document markers or modify large result data');
    const pretty = JSON.stringify({values:Array.from({length:10000},(_,id)=>({id}))},null,2);
    const verticalLines = ['#check Vertical performance','#options','Columns-Type: payload=json',
      '#payload','select fixture\\G',
      '#responce OK','*************************** 1. row ***************************',
      ...(`payload: ${pretty.replace(/\n/g,'\n         ')}`).split('\n')];
    const root = 7;
    const verticalRegion = {request:0,header:5,end:verticalLines.length};
    editor.setValue(verticalLines.join('\n'));
    editor.setOption('servicesResponses',[verticalRegion]);
    const coldLine = verticalLines.findIndex((line,i)=>i>20000 && line.trim()==='{');
    assert(coldLine>20000,'The vertical fixture must exercise a cold, distant JSON object');
    const jsonStart=performance.now();
    const rootFold=editor.foldRange({line:root,ch:0});
    assert(rootFold?.from.line===root && rootFold.to.line===verticalLines.length-1,
      'A large vertical JSON root must fold to its own closing brace');
    const coldFold=editor.foldRange({line:coldLine,ch:0});
    assert(coldFold?.from.line===coldLine && coldFold.to.line===coldLine+2,
      'Cold nested JSON must fold without replaying preceding syntax state');
    for(let i=0;i<100;i++)
      assert(editor.foldRange({line:root,ch:0}).to.line===rootFold.to.line,
        'Repeated gutter checks must reuse the bounded fold cache');
    const verticalFoldMs=performance.now()-jsonStart;
    assert(verticalFoldMs<budget(VERTICAL_FOLD_MS),
      `10,000 nested JSON objects and cached fold checks must stay within ${budget(VERTICAL_FOLD_MS)}ms: ${verticalFoldMs}ms`);
    editor.replaceRange('  ',{line:coldFold.to.line,ch:0});
    const changedFold=editor.foldRange({line:coldLine,ch:0});
    assert(changedFold.to.ch===coldFold.to.ch+2,
      'Editing the document must invalidate cached JSON fold coordinates');
    editor.replaceRange('+---+',{line:coldLine+1,ch:0},{line:coldLine+1,ch:editor.getLine(coldLine+1).length});
    assert(!editor.foldRange({line:coldLine,ch:0}),
      'A JSON fold must never consume a closing bracket outside its record');
    globalThis.tablePerformanceReceipt={rows:10000,characters:source.length,
      tokenizeMs,foldMs,verticalFoldMs,selectMs,selectionPaintMs,screen,speedSampleMs:globalThis.testSpeedSampleMs};
  } finally {
    editor?.destroy();
    host.remove();
  }
  return true;
})()
