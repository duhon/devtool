(async () => {
  const { assert, wait, click, testid, cm, choose, setSource, key, visible, sleep } = testDriver;
  await choose('console');
  await click(testid('console-service-splunk'));
  const editor = cm('console-editor');
  const previous = editor.getValue();
  const typedOptions = 'Columns-Type: message.timestamp=datetime message.message=json score=number enabled=boolean\nColumns-Size: message.message=20';
  const assertTyped = target => {
    const region = (target.getOption('servicesResponses') || [])[0];
    assert(region?.table?.types['message.message'] === 'json'
      && region.table.types.score === 'number', 'Returned column metadata must reach the text editor');
    const tokens = [];
    for (let line = region.header + 1; line < region.end; line++) {
      tokens.push(...target.getLineTokens(line, true));
      assert(!CodeMirror.fold.services(target, {line, ch:0}),
        `Cell contents must never offer a brace/JSON fold: ${target.getLine(line)}`);
    }
    assert(tokens.some(t => t.type === 'number' && t.string.includes('123')),
      'Numeric overrides must color actual data cells');
    assert(tokens.some(t => t.type === 'atom' && t.string.trim() === 'true'),
      'Boolean overrides must color actual data cells');
    assert(tokens.some(t => t.type === 'string' && t.string.includes('2026-10-27')),
      'Datetime overrides must color actual data cells');
    assert(tokens.some(t => t.type === 'property' && t.string === '"n"')
      && tokens.some(t => t.type === 'number' && t.string === '42'),
    'Compact/wrapped JSON must have flat syntax tokens');
    assert(tokens.some(t => !t.type && t.string.includes('{"not":"typed"}')),
      'Untyped brace-shaped text must not be automatically parsed as JSON');
    const dataLines = [];
    for (let line = region.header + 1; line < region.end; line++) {
      if (target.getLine(line).startsWith('+') || target.getLine(line).startsWith('|'))
        dataLines.push(target.getLine(line));
    }
    const border = dataLines[0], cuts = [...border.matchAll(/\+/g)].map(m => m.index);
    const jsonColumn = region.table.columns.indexOf('message.message');
    assert(cuts[jsonColumn + 1] - cuts[jsonColumn] - 3 === 20,
      'Columns-Size must set a 20-character content width, not pixels');
    assert(dataLines.filter(line => line.startsWith('|')).length > 3,
      'JSON must physically wrap into text lines');
    assert(!target.getWrapperElement().querySelector('table'),
      'Console output must remain text, never an HTML table');
    assert(CodeMirror.fold.services(target, {line:region.header,ch:0}),
      'Folding the complete response must remain available');
    return region;
  };
  setSource(editor, `#check Streaming export\n#endpoint\n${fixtureUrl}\n#options\nFrom: -1h\nTo: now\nApp: search\nCount: 2\n${typedOptions}\n#search\nindex=fixture | sort - _time`);
  editor.setCursor(editor.lastLine(), editor.getLine(editor.lastLine()).length);
  await key('Shift+Enter');
  await wait(() => editor.getValue().includes('splunk-final-🦀-1')
    && editor.getValue().includes('splunk-final-🦀-2')
    && !testid('console-main-status').classList.contains('is-running'),
  () => `Splunk must return Count rows while the server keeps its stream open: ${testid('console-main-status').textContent}`);
  assert(testid('console-main-status').textContent.includes('OK'),
    'A Count-limited export must complete successfully, not as cancellation');
  assert(!editor.getValue().includes('splunk-preview'),
    'Preview snapshots must never appear as final table rows');
  assert(editor.getValue().includes('index=fixture | sort - _time'),
    'Export must preserve the original SPL');
  assertTyped(editor);
  if (!visible(document.querySelector('[data-testid="console-main-status-debug"]')))
    await click(testid('console-main-status'));
  const panel = testid('console-main-status-debug');
  for (const name of ['export', 'request']) {
    const line = [...panel.querySelectorAll('.json-line')].find(line =>
      visible(line) && line.querySelector(':scope > .cm-property')?.textContent.trim() === `"${name}":`);
    assert(line, `Splunk Debug must include the ${name} object`);
    const toggle = line.querySelector('.json-toggle');
    if (toggle.textContent === '▸') await click(toggle);
  }
  const debug = panel.textContent;
  assert(debug.includes('"stoppedAfterCount": true') && debug.includes('"streamComplete": false')
    && debug.includes('"resultCount": 2') && debug.includes('/search/v2/jobs/export'),
  `Debug must distinguish early Count termination from EOF and show returned-row count: ${debug}`);
  assert(debug.includes('"ignoredPreviewRecords": 4') && debug.includes('"legacyFallbacks": 0'),
    'The normal export must ignore preview and require no compatibility retry');
  await click(testid('console-main-status'));
  setSource(editor, `#check Preview and offset\n#endpoint\n${fixtureUrl}\n#options\nFrom: -1h\nTo: now\nApp: search\nCount: 2\nPreview: true\nOffset: 1\n${typedOptions}\n#search\nindex=fixture | sort - _time`);
  editor.setCursor(editor.lastLine(), editor.getLine(editor.lastLine()).length);
  await key('Shift+Enter');
  await wait(() => editor.getValue().includes('splunk-preview-🦀-2')
    && editor.getValue().includes('splunk-preview-🦀-3')
    && !testid('console-main-status').classList.contains('is-running'),
  () => `Preview with Offset must return one snapshot, not wait for final rows: ${testid('console-main-status').textContent}`);
  assert(testid('console-main-status').textContent.includes('WARNING')
    && editor.getValue().includes('Preview results are provisional'),
  'Preview must be visibly provisional in both status and the recorded response');
  assert(!editor.getValue().includes('splunk-preview-short')
    && !editor.getValue().includes('splunk-preview-🦀-1')
    && !editor.getValue().includes('splunk-final'),
  'Offset must restart per snapshot, skip its first row and never mix snapshots');
  const region = assertTyped(editor);
  editor.focus();
  editor.setSelection({line:region.header+1,ch:0},{line:region.end,ch:0});
  const expectedCopy = editor.getSelection(), input = editor.getInputField();
  const copy = new ClipboardEvent('copy',{
    clipboardData:new DataTransfer(),bubbles:true,cancelable:true,
  });
  input.dispatchEvent(copy);
  // The textarea input uses the browser's default copy; synthetic events do not run it.
  assert(input.tagName === 'TEXTAREA' && !copy.defaultPrevented
    && input.value.slice(input.selectionStart,input.selectionEnd) === expectedCopy,
  'The native plain-text copy source must preserve every border, space and physical wrap');
  editor.setCursor(region.request,0);
  editor.scrollIntoView({line:region.request,ch:0});
  await wait(() => !document.querySelector('[data-testid="console-history-toggle"]')?.disabled);
  await click(testid('console-history-toggle'));
  await wait(() => document.querySelector('[data-testid="history-entry"]'));
  await click(testid('history-entries').querySelector('[data-testid="history-entry"]'));
  await wait(() => document.querySelector('[data-testid="history-editor"]')
    && cm('history-editor').getValue().includes('splunk-preview-🦀-3'));
  const saved = cm('history-editor').getValue();
  assertTyped(cm('history-editor'));
  assert(saved.includes('Preview results are provisional') && saved.includes('· WARNING')
    && saved.includes('Preview: true') && saved.includes('Offset: 1'),
  'History must preserve preview status, option values and the selected portion');
  await click(testid('console-history-toggle'));
  await click(testid('console-service-rest'));
  await click(testid('console-service-splunk'));
  const restored = cm('console-editor');
  assert(restored.getValue().includes('splunk-preview-🦀-3'),
    'Typed response text must survive an editor remount');
  assertTyped(restored);
  setSource(restored, previous);
  await click(testid('console-service-mysql'));
  const mysql = cm('console-editor'), oldMysql = mysql.getValue();
  const rendered = '#check MySQL rendering fixture\n#endpoint\nmysql://fixture.invalid/db\n#query\nselect fixture\\G\n\n#responce OK\n*************************** 1. row ***************************\n       id: 42\n  enabled: 1\n  created: 2026-10-27T09:00:00Z\nfeed_data: {\n             "n": 12,\n             "ok": true,\n             "nested": {\n               "label": "😀 braces { [ and escaped \\" quote",\n               "items": [\n                 1,\n                 2\n               ]\n             }\n           }\n    plain: {"not":"typed"}\n           untyped {\n             "inner": [1, 2]\n           }';
  setSource(mysql, rendered);
  await sleep(50);
  const mysqlRegion = {request:0,header:6,end:mysql.lineCount(),table:{
    columns:['id','enabled','created','feed_data','plain'],
    types:{id:'number',enabled:'boolean',created:'datetime',feed_data:'json',plain:'text'},
  }};
  mysql.setOption('servicesResponses',[mysqlRegion]);
  mysql.setOption('mode',{name:'services-console-mysql',responses:[mysqlRegion]});
  const mysqlTokens = [];
  const lines = rendered.split('\n');
  const rootLine = lines.indexOf('feed_data: {');
  const nestedLine = lines.findIndex(line => line.includes('"nested": {'));
  const arrayLine = lines.findIndex(line => line.includes('"items": ['));
  const getMysqlTokenAt = mysql.getTokenAt;
  let jsonFoldTokenizations = 0;
  mysql.getTokenAt = function(...args) {
    jsonFoldTokenizations++;
    return getMysqlTokenAt.apply(this,args);
  };
  for (let line=mysqlRegion.header+1;line<mysqlRegion.end;line++) {
    mysqlTokens.push(...mysql.getLineTokens(line,true));
    const fold = CodeMirror.fold.services(mysql,{line,ch:0});
    assert(Boolean(fold) === [rootLine,nestedLine,arrayLine].includes(line),
      `Only typed vertical JSON containers must fold: ${mysql.getLine(line)}`);
  }
  mysql.getTokenAt = getMysqlTokenAt;
  assert(jsonFoldTokenizations === 0,
    'Vertical JSON fold checks must not force syntax tokenization');
  const rootFold = CodeMirror.fold.services(mysql,{line:rootLine,ch:0});
  assert(rootFold.from.line === rootLine && rootFold.from.ch === 'feed_data: {'.length
    && mysql.getRange(rootFold.to,{line:rootFold.to.line,ch:rootFold.to.ch+1}) === '}'
    && rootFold.to.line === lines.indexOf('           }'),
  'JSON folds must match their closing brace, ignoring escaped quotes and brackets in strings');
  const selectedBeforeFold = mysql.getValue();
  mysql.foldCode({line:rootLine,ch:0},{rangeFinder:CodeMirror.fold.services});
  assert(mysql.getAllMarks().some(mark => mark.__isFold),
    'The typed JSON value must actually collapse, not just offer a range');
  mysql.foldCode({line:rootLine,ch:0},{rangeFinder:CodeMirror.fold.services},'unfold');
  assert(mysql.getValue() === selectedBeforeFold,
    'Folding and unfolding JSON must preserve all copyable text');
  mysql.foldCode({line:rootLine,ch:0},{rangeFinder:CodeMirror.fold.services});
  assert(mysql.getAllMarks().some(mark => mark.__isFold),
    'Cached JSON ranges must remain reusable after unfolding');
  mysql.foldCode({line:rootLine,ch:0},{rangeFinder:CodeMirror.fold.services},'unfold');
  assert(mysqlTokens.some(t=>t.type==='number' && t.string.includes('42'))
    && mysqlTokens.some(t=>t.type==='atom' && t.string.includes('1'))
    && mysqlTokens.some(t=>t.type==='number' && t.string==='12')
    && mysqlTokens.some(t=>t.type==='property' && t.string==='"ok"'),
  'MySQL vertical schema and JSON values must have flat typed tokens');
  assert(mysqlTokens.some(t=>!t.type && t.string.includes('{"not":"typed"}')),
    'MySQL text containing JSON must stay plain without an override');
  mysql.scrollIntoView({line:12,ch:0},30);
  await sleep(50);
  const number=mysql.getWrapperElement().querySelector('.cm-number');
  const property=mysql.getWrapperElement().querySelector('.cm-property');
  assert(number && property && getComputedStyle(number).color !== getComputedStyle(property).color,
    'Typed tokens must have visibly distinct syntax colors, not just class names');
  mysql.setOption('servicesResponses',[]);
  mysql.setOption('mode',{name:'services-console-mysql',responses:[]});
  const braceLine = rendered.split('\n').indexOf('feed_data: {');
  assert(!CodeMirror.fold.services(mysql,{line:braceLine,ch:0}),
    'Pasted MySQL response JSON without ownership metadata must not fold either');
  setSource(mysql,oldMysql);
  await click(testid('console-service-rest'));
  return true;
})()
