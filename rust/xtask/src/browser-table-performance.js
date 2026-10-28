(async () => {
  const {assert, sleep} = testDriver;
  assert(typeof CodeMirror.servicesTableTokenizer === 'function',
    'Table token spans must use the native JS fast path');
  assert(typeof CodeMirror.servicesJsonFold === 'function',
    'Vertical JSON folding must use the native JS fast path');
  const payload = JSON.stringify({name:'Unicode 😀', escaped:'a"b\\c',
    values:Array.from({length:100},(_,i)=>({id:i,enabled:true}))});
  const width = Array.from(payload).length;
  const border = `+-------+${'-'.repeat(width+2)}+`;
  const header = `| id    | payload${' '.repeat(width-7)} |`;
  const prefix = ['#check Performance','#query','select fixture','#responce OK',
    border,header,border];
  const row = i => `| ${String(i).padEnd(5)} | ${payload} |`;
  const region = {request:0,header:3,end:10008,table:{
    columns:['id','payload'],types:{id:'number',payload:'json'},header_lines:1,
  }};
  const mode = CodeMirror.getMode({indentUnit:2},{
    name:'services-console-mysql',responses:[region],
  });
  let state = CodeMirror.startState(mode);
  const started = performance.now();
  for (const line of [...prefix,...Array.from({length:30},(_,i)=>row(i)),border]) {
    const stream = new CodeMirror.StringStream(line,4);
    while(!stream.eol()) {
      stream.start=stream.pos;
      mode.token(stream,state);
      assert(stream.pos>stream.start,'The batched table lexer must advance');
    }
  }
  const tokenizeMs = performance.now()-started;
  assert(tokenizeMs<200,`30 dense JSON rows must tokenize within 200ms, got ${tokenizeMs}ms`);
  const saved = CodeMirror.innerMode(mode,CodeMirror.copyState(mode,state)).state;
  assert(!('tableTokens' in saved) && !('tableLine' in saved),
    'Saved line states must not retain transient token arrays or full line strings');
  const source = [...prefix,...Array.from({length:10000},(_,i)=>row(i)),border].join('\n');
  const host=document.createElement('div');
  host.style.cssText='position:fixed;inset:0;z-index:2147483647;background:#282a36';
  document.body.appendChild(host);
  let editor;
  try {
    editor=CodeMirror(host,{value:source,
      mode:{name:'services-console-mysql',responses:[region]},
      servicesResponses:[region],servicesChecks:[0],theme:'dracula',selectionStyle:'text',lineNumbers:true,viewportMargin:10,
      foldGutter:{gutter:'console-fold-gutter'},
      gutters:['CodeMirror-linenumbers','console-fold-gutter'],workTime:8,workDelay:20});
    editor.setSize(1000,400);
    const getTokenAt = editor.getTokenAt;
    let foldTokenizations = 0;
    editor.getTokenAt = function(...args) {
      foldTokenizations++;
      return getTokenAt.apply(this,args);
    };
    const foldStart=performance.now();
    for(let line=7;line<37;line++)
      assert(!CodeMirror.fold.services(editor,{line,ch:0}),'Data rows must never fold');
    const foldMs=performance.now()-foldStart;
    assert(foldMs<30,`Owned output fold checks must not re-tokenize rows: ${foldMs}ms`);
    assert(foldTokenizations===0,'Horizontal table fold checks must never invoke tokenization');
    editor.getTokenAt = getTokenAt;
    editor.scrollTo(0,editor.heightAtLine(9000,'local'));
    await sleep(50);
    assert(editor.getViewport().from>8900,'A large document must scroll to distant rows');
    assert(host.querySelectorAll('.CodeMirror-code>div').length<100,
      '10,000 rows must render only a bounded viewport, not every row');
    assert(host.querySelectorAll('.console-check-band-even').length>0
      && host.querySelectorAll('.console-check-band-even').length<100,
      'Check backgrounds must decorate only the virtualized viewport, not allocate per result row');
    assert([...host.querySelectorAll('.cm-property')].some(s=>s.textContent==='"enabled"'),
      'Cold scrolling must preserve JSON colors without replaying the whole response');
    assert(editor.getValue()===source,'Performance optimization must not truncate or alter text');
    editor.setSelection({line:9000,ch:0},{line:9001,ch:0});
    assert(editor.getSelection()===`${row(8993)}\n`,
      'A distant row must remain exactly copyable, including Unicode and JSON escapes');
    const selectionEnd={line:editor.lastLine(),ch:editor.getLine(editor.lastLine()).length};
    editor.setOption('selectionStyle','line');
    let selectionStart=performance.now();
    editor.setSelection({line:0,ch:0},selectionEnd,{scroll:false});
    const baselineSelectionMs=performance.now()-selectionStart;
    editor.setCursor({line:9000,ch:0},{scroll:false});
    editor.setOption('selectionStyle','text');
    const input=editor.display.input, prepare=input.prepareSelection, show=input.showSelection;
    let selectionPaintMs=0;
    for(const [name,original] of [['prepareSelection',prepare],['showSelection',show]])
      input[name]=function(...args){
        const start=performance.now();
        try{return original.apply(this,args);}finally{selectionPaintMs+=performance.now()-start;}
      };
    let selectionMs;
    try{
      selectionStart=performance.now();
      editor.setSelection({line:0,ch:0},selectionEnd,{scroll:false});
      selectionMs=performance.now()-selectionStart;
    }finally{
      input.prepareSelection=prepare;input.showSelection=show;
    }
    assert(selectionPaintMs<200,
      `Painting a 10,000-row selection must remain viewport-bounded: ${selectionPaintMs}ms`);
    assert(selectionMs<=baselineSelectionMs*1.5+100,
      `Text-only selection must not regress native selection preparation: native=${baselineSelectionMs}ms, text=${selectionMs}ms`);
    assert(host.querySelectorAll('.CodeMirror-selected').length<100
      &&host.querySelectorAll('.CodeMirror-selection-outline').length===1,
      'A whole-table selection must allocate only visible-row rectangles and one external contour');
    assert(editor.getAllMarks().length===0&&editor.getValue()===source,
      'Selection painting must not allocate document markers or modify large result data');
    editor.setOption('foldGutter',false);
    const pretty = JSON.stringify({values:Array.from({length:10000},(_,id)=>({id}))},null,2);
    const verticalLines = ['#check Vertical performance','#query','select fixture\\G',
      '#responce OK','*************************** 1. row ***************************',
      ...(`payload: ${pretty.replace(/\n/g,'\n         ')}`).split('\n')];
    const verticalRegion = {request:0,header:3,end:verticalLines.length,table:{
      columns:['payload'],types:{payload:'json'},header_lines:1,
    }};
    editor.setValue(verticalLines.join('\n'));
    editor.setOption('servicesResponses',[verticalRegion]);
    editor.setOption('mode',{name:'services-console-mysql',responses:[verticalRegion]});
    foldTokenizations=0;
    editor.getTokenAt = function(...args) {
      foldTokenizations++;
      return getTokenAt.apply(this,args);
    };
    const coldLine = verticalLines.findIndex((line,i)=>i>20000 && line.trim()==='{');
    assert(coldLine>20000,'The vertical fixture must exercise a cold, distant JSON object');
    const jsonStart=performance.now();
    const rootFold=CodeMirror.fold.services(editor,{line:5,ch:0});
    assert(rootFold?.from.line===5 && rootFold.to.line===verticalLines.length-1,
      'A large vertical JSON root must fold to its own closing brace');
    const coldFold=CodeMirror.fold.services(editor,{line:coldLine,ch:0});
    assert(coldFold?.from.line===coldLine && coldFold.to.line===coldLine+2,
      'Cold nested JSON must fold without replaying preceding syntax state');
    for(let i=0;i<100;i++)
      assert(CodeMirror.fold.services(editor,{line:5,ch:0}).to.line===rootFold.to.line,
        'Repeated gutter checks must reuse the bounded fold cache');
    const verticalFoldMs=performance.now()-jsonStart;
    assert(verticalFoldMs<200,
      `10,000 nested JSON objects and cached fold checks must stay within 200ms: ${verticalFoldMs}ms`);
    assert(foldTokenizations===0,'Vertical JSON folding must never force syntax tokenization');
    editor.getTokenAt=getTokenAt;
    editor.replaceRange('  ',{line:coldFold.to.line,ch:0});
    const changedFold=CodeMirror.fold.services(editor,{line:coldLine,ch:0});
    assert(changedFold.to.ch===coldFold.to.ch+2,
      'Editing the document must invalidate cached JSON fold coordinates');
    editor.setOption('servicesResponses',[{...verticalRegion,end:coldLine+1}]);
    assert(!CodeMirror.fold.services(editor,{line:coldLine,ch:0}),
      'A JSON fold must never consume a closing bracket outside its response');
    editor.setOption('servicesResponses',[{...verticalRegion,table:{
      ...verticalRegion.table,types:{payload:'text'},
    }}]);
    assert(!CodeMirror.fold.services(editor,{line:coldLine,ch:0}),
      'Changing column metadata must disable previously cached JSON folds');
    globalThis.tablePerformanceReceipt={rows:10000,characters:source.length,
      tokenizeMs,foldMs,verticalFoldMs,selectionMs,selectionPaintMs,baselineSelectionMs};
  } finally {
    if(editor){editor.setOption('mode',null);editor.setValue('');}
    host.remove();
  }
  return true;
})()
