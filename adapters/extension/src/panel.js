import init from './wasm/services_ui.js';

const isCheck = text => /^\s*#(?:check|request)(?:\s|$)/i.test(text);
CodeMirror.servicesSeparateChecks = (cm, change, pastedRegions = []) => {
  const checks = cm.getOption('servicesChecks');
  if (!checks || !['+input', 'paste', 'complete'].includes(change.origin)) return 0;
  const previous = cm.state.servicesPreviousCheck;
  const added = [];
  const protectedRegions = pastedRegions.length > 1
    ? pastedRegions.slice().sort((a, b) => a.header - b.header) : pastedRegions;
  let protectedIndex = 0;
  let seen;
  for (let i = 0; i < change.text.length; i++) {
    const number = change.from.line + i;
    const text = cm.getLine(number);
    if (!text || !isCheck(text)) continue;
    while (protectedIndex < protectedRegions.length && protectedRegions[protectedIndex].end <= i)
      protectedIndex++;
    if (protectedIndex < protectedRegions.length && protectedRegions[protectedIndex].header <= i) continue;
    if (i === 0 && previous?.line === number && previous.check && previous.output
      && change.origin !== 'paste') continue;
    if (seen === undefined) seen = checks.some(line => line < change.from.line);
    const marker = /^\s*#(?:check|request)/i.exec(text)[0];
    const editingTitle = i === 0 && change.origin === '+input'
      && change.text.length === 1 && change.from.ch > marker.length;
    const incomplete = change.origin === '+input' && change.text.length === 1
      && text.trim().toLowerCase() === marker.trim().toLowerCase()
      && !/\s/.test(change.text[0]);
    if (seen && !editingTitle && !incomplete && number > 0
      && cm.getLine(number - 1).trim() !== '') added.push(number);
    seen = true;
  }
  if (!added.length) return 0;
  cm.operation(() => {
    for (let i = added.length - 1; i >= 0; i--)
      cm.replaceRange('\n', {line: added[i], ch: 0}, {line: added[i], ch: 0}, change.origin);
  });
  return added.length;
};

CodeMirror.defineOption('servicesChecks', null, (cm, checks) => {
  if (!cm.state.servicesCheckBands && checks !== null) {
    cm.on('beforeChange', (editor, change) => {
      const check = isCheck(editor.getLine(change.from.line));
      cm.state.servicesPreviousCheck = {line: change.from.line, check,
        output: check && (editor.getOption('servicesResponses') || [])
          .some(region => change.from.line >= region.header && change.from.line < region.end)};
    });
    const render = (editor, line, element) => {
      const bounds = editor.getOption('servicesChecks');
      if (!bounds) return;
      const number = editor.getLineNumber(line);
      let from = 0, to = bounds.length;
      while (from < to) {
        const mid = (from + to) >>> 1;
        if (bounds[mid] <= number) from = mid + 1;
        else to = mid;
      }
      if (from) element.classList.add(from % 2 ? 'console-check-band-even' : 'console-check-band-odd');
    };
    cm.state.servicesCheckBands = render;
    cm.on('renderLine', render);
  }
  if (cm.state.servicesCheckBands) cm.refresh();
});

CodeMirror.servicesTableTokenizer = tokenize => function (stream, state) {
  // Rust produces a line's spans once; subsequent tokens stay on the JS side.
  if (stream.pos > 0 && state.tableLine === stream.string) {
    const index = state.tableTokenIndex;
    const token = state.tableTokens?.[index];
    if (token && token[0] > stream.pos) {
      stream.pos = token[0];
      state.tableTokenIndex = index + 1;
      return token[1];
    }
  }
  return tokenize(stream, state);
};

const jsonFoldCaches = new WeakMap();
CodeMirror.servicesJsonFold = (cm, line) => {
  const doc = cm.getDoc();
  const region = (cm.getOption('servicesResponses') || [])
    .find(region => line > region.header && line < region.end);
  if (!region?.table) return;
  const generation = doc.changeGeneration();
  let cache = jsonFoldCaches.get(doc);
  if (!cache || cache.generation !== generation || cache.region !== region) {
    const columns = region.table.columns;
    const widths = columns.map(name => Array.from(name).length);
    const width = widths.reduce((max, width) => Math.max(max, width), 0);
    cache = {generation, region, scopes: [], folds: new Map(),
      fields: columns.map((name, i) => ({
        prefix: ' '.repeat(width - widths[i]) + name + ': ',
        kind: region.table.types[name],
      }))};
    jsonFoldCaches.set(doc, cache);
  }
  const cached = cache.folds.get(line);
  if (cached) return {from: {...cached.from}, to: {...cached.to}};
  let fieldLine, field;
  for (let at = line; at > region.header; at--) {
    const scope = cache.scopes.find(scope => at >= scope.from && at <= scope.to);
    if (scope) {
      fieldLine = scope.from;
      field = scope.field;
      scope.to = Math.max(scope.to, line);
      break;
    }
    const text = doc.getLine(at);
    if (text.startsWith('*****') || text.startsWith('|') || text.startsWith('+')) return;
    field = cache.fields.find(field => text.startsWith(field.prefix));
    if (field) {
      fieldLine = at;
      cache.scopes.push({from: at, to: line, field});
      if (cache.scopes.length > 32) cache.scopes.shift();
      break;
    }
  }
  if (!field || field.kind !== 'json') return;
  const value = doc.getLine(fieldLine).slice(field.prefix.length).trimStart();
  if (!value.startsWith('{') && !value.startsWith('[')) return;
  const stack = [];
  let from, quoted = false, escaped = false;
  for (let at = line; at < Math.min(region.end, doc.lineCount()); at++) {
    const text = doc.getLine(at);
    for (let ch = at === fieldLine ? field.prefix.length : 0; ch < text.length; ch++) {
      const c = text[ch];
      if (quoted) {
        if (escaped) escaped = false;
        else if (c === '\\') escaped = true;
        else if (c === '"') quoted = false;
      } else if (c === '"') {
        quoted = true;
      } else if (c === '{' || c === '[') {
        if (!from) from = {line: at, ch: ch + 1};
        stack.push(c === '{' ? '}' : ']');
      } else if (c === '}' || c === ']') {
        if (stack.pop() !== c) return;
        if (!stack.length && from) {
          const fold = {from, to: {line: at, ch}};
          cache.folds.set(line, {from: {...from}, to: {line: at, ch}});
          if (cache.folds.size > 128) cache.folds.delete(cache.folds.keys().next().value);
          return fold;
        }
      }
    }
    if (!from) return;
  }
};

try {
  await init();
} catch (error) {
  document.getElementById('app').textContent = `Could not start Services DevTools: ${error.message}`;
  console.error(error);
}
