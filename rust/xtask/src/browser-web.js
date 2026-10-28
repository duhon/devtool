(async () => {
  const { assert, sleep, wait, visible, find, testid, cm, key, click, button, choose,
    setSource, commitRestUrl, candidates, pick, pickThenEnter, type } = testDriver;
  const objectFieldMarks = editor => editor.getAllMarks()
    .filter(mark => mark.className === 'cm-graphql-object-field');
  const expectObjectColors = async (editor, expected, scalars = []) => {
    const names = () => objectFieldMarks(editor).map(mark => {
      const range = mark.find();
      return editor.getRange(range.from, range.to);
    }).sort();
    await wait(() => JSON.stringify(names()) === JSON.stringify(expected.slice().sort()),
      () => `Object field colors: expected ${JSON.stringify(expected)}, got ${JSON.stringify(names())}`);
    const leaves = [...editor.getWrapperElement().querySelectorAll('.CodeMirror-line span')]
      .filter(span => !span.childElementCount);
    for (const [fields, color] of [[expected, 'rgb(79, 143, 247)'], [scalars, 'rgb(102, 217, 239)']])
      for (const name of fields)
        assert(leaves.some(span => span.textContent === name && getComputedStyle(span).color === color),
          `${name}: expected actual glyph color ${color}`);
  };
  const fragmentColorSource = 'fragment PriceTest on Price { amount }\nquery { products {\n  ... on SimpleProductView {\n    price { ... PriceTest }\n  }\n} }';
  const checkJsonVariableSnippets = async (editor, prefix = '') => {
    const suffix = prefix ? '\n#query\nquery { products { sku } }\n#check preserved\n#query\nquery { viewer { name } }' : '';
    const previousHeight = editor.getWrapperElement().style.height;
    // Keep this nested-JSON fixture visible so popup checks do not race the widget's scroll-to-close behavior.
    editor.setSize(null, Math.max(editor.getScrollInfo().clientHeight, editor.defaultTextHeight() * 16));
    const same = expected => JSON.stringify(candidates()) === JSON.stringify(expected);
    const valueChoices = () => {
      const indent = editor.getLine(editor.getCursor().line).match(/^[ \t]*/)[0];
      return ['""', `{\n${indent}  \n${indent}}`, `[\n${indent}  \n${indent}]`];
    };
    const expectValues = () => wait(() => same(valueChoices()),
      () => `Variables must offer exactly string, object and array: ${JSON.stringify(candidates())}`);
    const property = async name => {
      await wait(() => same(['""']), () => `An object must automatically offer a quoted property name: ${JSON.stringify({
        name, source: editor.getValue(), cursor: editor.getCursor(), candidates: candidates(),
        focused: editor.hasFocus(), connected: editor.getWrapperElement().isConnected,
        service: editor.__servicesCompletion?.service, field: editor.__servicesCompletion?.field,
        enabled: editor.__servicesHintsEnabled, automatic: editor.__servicesHintsAutomatic,
        local: editor.state.completionActive?.data?.list?.map(item => item.text),
      })}`);
      await pick(editor, '""');
      const cursor = editor.getCursor();
      assert(editor.getLine(cursor.line).slice(cursor.ch - 1, cursor.ch + 1) === '""',
        'The property template must place the caret between its quotes');
      assert(candidates().length === 0, 'Typing a property name must not show value or schema hints');
      await type(name);
      await key('ArrowRight');
      await type(':');
      await expectValues();
      await key('Space');
      await expectValues();
    };
    setSource(editor, prefix + suffix);
    editor.setCursor(editor.posFromIndex(prefix.length));
    await key('Control+Space');
    await wait(() => same(['{\n  \n}']));
    await pick(editor, '{\n  \n}');
    await property('skus');
    assert(document.querySelector('.CodeMirror-hints')?.textContent.includes('[] — JSON array'),
      'The multiline array snippet must have a compact [] menu label');
    await pick(editor, valueChoices()[2]);
    await expectValues();
    await pick(editor, '""');
    await type('sku-first');
    await key('ArrowRight');
    await type(', ');
    await expectValues();
    await pick(editor, valueChoices()[1]);
    await property('nested');
    await pick(editor, valueChoices()[2]);
    await expectValues();
    await pick(editor, '""');
    await type('sku-nested');
    await key('ArrowRight');
    const arrayEnd = editor.getValue().indexOf('\n  ]', prefix.length) + '\n  ]'.length;
    editor.setCursor(editor.posFromIndex(arrayEnd));
    await type(',');
    await key('Escape');
    await key('Enter');
    await property('filter');
    await pick(editor, valueChoices()[1]);
    await property('country');
    await pick(editor, '""');
    await type('PL');
    await key('ArrowRight');
    const source = editor.getValue();
    const variables = JSON.parse(source.slice(prefix.length, source.length - suffix.length));
    assert(JSON.stringify(variables) === JSON.stringify({ skus: ['sku-first', { nested: ['sku-nested'] }], filter: { country: 'PL' } }),
      'Keyboard snippets must produce valid nested JSON and retain existing closing delimiters');
    assert(source.startsWith(prefix) && source.endsWith(suffix),
      'Variables snippets must not change adjacent Console sections or checks');
    setSource(editor, `${prefix}{"имя😀": "fixed"}${suffix}`);
    editor.setCursor(editor.posFromIndex(prefix.length + '{"имя😀": '.length));
    await key('Control+Space');
    await wait(() => same([]), 'Variables templates must not insert over an existing value');
    assert(editor.getValue() === `${prefix}{"имя😀": "fixed"}${suffix}`,
      'Right-context protection must preserve Unicode property names and existing values');
    editor.setSize(null, previousHeight);
  };
  const checkAliasAndManualHeaderHints = async (editor, prefix = '') => {
    setSource(editor, `${prefix}fragment CountrySummary `);
    await type('o');
    await type('n');
    await key('Space');
    await wait(() => candidates().includes('Country') && candidates().includes('Language'),
      'Manually typing on and pressing Space must automatically offer known composite types');
    setSource(editor, `${prefix}fragment CountrySummary on Country {\n  displayName`);
    await type(':');
    await key('Space');
    await wait(() => candidates().includes('name') && candidates().includes('capital'),
      'An alias colon followed by Space must automatically offer the current type fields');
    assert(!candidates().some(candidate => candidate.startsWith('...')),
      'A fragment spread cannot replace the field name after an alias colon');
    await pick(editor, 'name');
    assert(editor.getValue().endsWith('displayName: name'), 'Alias completion must preserve its name and colon');
    await key('Space');
    await wait(() => candidates().includes('@include()') && candidates().includes('@skip()'));
    assert(candidates().every(candidate => candidate.startsWith('@')),
      'A completed scalar alias must offer directives, not sibling fields, on Space');
  };
  const checkAdvertisedDirectives = async (editor, prefix = '') => {
    for (const body of [
      'query { products(skus: [])',
      'query { products { ... on SimpleProductView',
      'fragment Fields on Product { sku }\nquery { products { ...Fields',
    ]) {
      setSource(editor, prefix + body);
      await key('Space');
      await wait(() => candidates().includes('@include()') && candidates().includes('@skip()'),
        `Directives must be advertised at a valid attachment site: ${body}`);
    }
    for (const body of ['query { products {', 'fragment Fields on Product']) {
      setSource(editor, prefix + body);
      await key('Space');
      await key('Control+Space');
      assert(!candidates().some(candidate => candidate.startsWith('@')),
        `Conditional directives are invalid here: ${body}`);
    }
    setSource(editor, `${prefix}query Test($show: Boolean!) { products { sku`);
    await key('Space');
    await wait(() => candidates().includes('@include()') && candidates().includes('@skip()'));
    assert(candidates().every(candidate => candidate.startsWith('@')),
      'Scalar Space must automatically offer only directives');
    await pick(editor, '@include()');
    assert(editor.getValue().endsWith('sku @include()')
      && editor.getCursor().ch === editor.getLine(editor.getCursor().line).length - 1,
    'A directive choice must insert parentheses and place the cursor inside them');
    await pick(editor, 'if: ');
    await wait(() => candidates().includes('$show') && candidates().includes('true') && candidates().includes('false'),
      'A directive argument must advertise compatible variables and Boolean literals directly');
    assert(!candidates().includes('$'), 'Standalone $ must never be suggested');
    await type('$');
    await wait(() => candidates().includes('$show') && !candidates().includes('true'),
      'Manually typing the variable prefix must automatically open declared variable hints');
    await pick(editor, '$show');
    await key('ArrowRight');
    await key('Space');
    await wait(() => candidates().length === 1 && candidates()[0] === '@skip()',
      'A used non-repeatable directive must be excluded from automatic menus');
    await pick(editor, '@skip()');
    await pick(editor, 'if: ');
    await pick(editor, 'false');
    await key('ArrowRight');
    await key('Space');
    await sleep(80);
    assert(candidates().length === 0, 'Used include/skip must not be suggested again');
    await key('Control+Space');
    await wait(() => candidates().includes('images') && candidates().includes('... on '));
    const menu = find('.CodeMirror-hints').textContent;
    assert(menu.includes('Type condition') && !menu.includes('Inline fragment'),
      'The ... on hint must describe a type condition, not a named fragment');
  };
  const checkUndeclaredVariableHints = async (editor, prefix, name) => {
    const source = `${prefix}fragment U on Country { currency @skip() }`;
    setSource(editor, source);
    editor.setCursor(editor.posFromIndex(source.indexOf('@skip()') + '@skip('.length));
    await pick(editor, 'if: ');
    await wait(() => candidates().includes('true') && candidates().includes('false'),
      'An undeclared Boolean variable must still allow literal values');
    assert(!candidates().includes('$'), 'Standalone $ must not appear without declarations either');
    await type('$');
    await wait(() => candidates().length === 1 && candidates()[0] === `$${name}`,
      'Typing $ without declarations must automatically suggest this request Variables JSON names');
    await pick(editor, `$${name}`);
    assert(editor.getValue() === source.replace('@skip()', `@skip(if: $${name})`),
      'Choosing a variable must replace the prefix once and preserve the existing closing parenthesis');
  };
  const checkUsefulTypeConditions = async (editor, prefix = '') => {
    for (const [body, field, parent] of [
      ['query {\n  ', 'country', 'Query'],
      ['query { country(code: "PL") {\n  ', 'code', 'Country'],
      ['query { products { images {\n  ', 'url', 'Image'],
      ['query { products { ... on SimpleProductView { price {\n  ', 'amount', 'Price'],
    ]) {
      setSource(editor, prefix + body);
      await key('Control+Space');
      await wait(() => candidates().includes(field));
      assert(!candidates().includes('... on '),
        `${parent} must not advertise a type condition when only the same type is compatible`);
      await type('... ');
      await key('Control+Space');
      await wait(() => candidates().includes('@include()'));
      assert(!candidates().some(candidate => candidate.trim() === 'on'),
        'Manually typed ellipsis must not offer an unusable on continuation either');
      await type('on ');
      await wait(() => candidates().length === 1 && candidates()[0] === parent,
        'Explicitly typed type conditions must retain valid self-type completion');
    }
    setSource(editor, `${prefix}fragment Shared on Country { code }\nquery { country(code: "PL") {\n  `);
    await key('Control+Space');
    await wait(() => candidates().includes('...Shared') && candidates().includes('name'));
    assert(!candidates().includes('... on '),
      'Useful named fragments must stay available without a redundant type-condition hint');
    await type('... ');
    await wait(() => candidates().includes('Shared'));
    assert(!candidates().some(candidate => candidate.trim() === 'on'));
    await pick(editor, 'Shared');
    assert(editor.getValue().endsWith('... Shared'), 'Named fragment insertion must remain intact');
  };
  const checkSingleOperationHints = async (editor, prefix = '') => {
    for (const operation of [
      'query Q { viewer { name } }',
      'mutation M { updateUser(input: {}) { name } }',
      'subscription S { userChanged { name } }',
      '{ viewer { name } }',
    ]) {
      setSource(editor, prefix + operation);
      await key('Enter');
      await key('Control+Space');
      assert(prefix ? candidates().includes('#check') && candidates().every(text => text.startsWith('#'))
        : candidates().length === 0,
      'A completed operation must only offer Console separators, never another operation or anonymous selection set');
    }
    const leading = 'fragment A on Country { code }\nfragment B on Country { name }\n';
    setSource(editor, prefix + leading);
    await key('Control+Space');
    await wait(() => candidates().includes('fragment') && candidates().includes('query') && candidates().includes('mutation'),
      'Multiple leading fragments must retain the first-operation and additional-fragment choices');
    setSource(editor, `${prefix}${leading}\nquery Q { country(code: "PL") { ...A } }`);
    editor.setCursor(editor.posFromIndex(prefix.length + leading.length));
    await key('Control+Space');
    await wait(() => candidates().length === 1 && candidates()[0] === 'fragment',
      'An operation already below the cursor must leave only additional leading fragments available');
    if (prefix) {
      setSource(editor, `${prefix}query Q { viewer { name } }\n#check fresh\n#query\n`);
      await key('Control+Space');
      await wait(() => candidates().includes('query') && candidates().includes('mutation'),
        'A new Console request must have its own first-operation choices');
    }
  };
  const checkConsoleSeparatorBlocks = async editor => {
    setSource(editor,
      '#check first\n#endpoint\nhttps://hints.fixture.invalid/graphql\n#variables\n{"hideCurrency":false}\n#query\nquery First { viewer { name } }');
    await key('Enter');
    await wait(() => candidates().includes('#check') && candidates().includes('#headers'),
      'A blank line after a completed query must automatically offer a new request and unused sections');
    assert(!candidates().includes('#endpoint') && !candidates().includes('#variables') && !candidates().includes('#query'),
      'Completed request sections must not be duplicated in the same block');
    await type('#');
    await wait(() => candidates().includes('#check'),
      'Typing # after a completed request must retain the new-request choice');
    await pick(editor, '#check');
    await type('second');
    await key('Enter');
    await wait(() => ['#endpoint', '#variables', '#query'].every(marker => candidates().includes(marker)),
      'A second request must get all its own section hints despite the first request using them');
    await pick(editor, '#endpoint');
    await type('https://hints.fixture.invalid/graphql');
    await key('Enter');
    await wait(() => candidates().includes('#variables') && candidates().includes('#query'));
    assert(!candidates().includes('#endpoint'), 'Only the current request endpoint should be considered used');
    await pick(editor, '#variables');
    await pick(editor, '{\n  \n}');
    await type('"hideCurrency": false');
    editor.setCursor(editor.lastLine(), editor.getLine(editor.lastLine()).length);
    await key('Enter');
    await type('#');
    await wait(() => candidates().includes('#query') && candidates().includes('#check'));
    assert(!candidates().includes('#variables') && !candidates().includes('#endpoint'),
      'Each section must be tracked within the second block only');
    await pick(editor, '#query');
    await type('query Second { viewer { name } }');
    await key('Enter');
    await wait(() => candidates().includes('#check'));
    await pick(editor, '#check');
    await type('third');
    await key('Enter');
    await wait(() => candidates().includes('#endpoint') && candidates().includes('#variables') && candidates().includes('#query'),
      'A third request must reset section availability independently');
    await type('#variable');
    await wait(() => candidates().includes('#variables'));
    await pick(editor, '#variables');
    assert(editor.getValue().endsWith('#check third\n#variables\n'),
      'The #variable prefix must complete to the canonical #variables marker and open its body');
    const source = editor.getValue();
    for (const [needle, prefix, candidate] of [
      ['#endpoint', '#end', '#endpoint'],
      ['#variables', '#vari', '#variables'],
      ['#query', '#qu', '#query'],
      ['#check second', '#che', '#check'],
    ]) {
      editor.setCursor(editor.posFromIndex(source.indexOf(needle) + prefix.length));
      await key('Control+Space');
      await wait(() => candidates().includes(candidate),
        `An existing section must remain editable with later request blocks present: ${needle}`);
      await pick(editor, candidate);
      assert(editor.getValue() === source,
        `Editing ${needle} must not duplicate its suffix, title whitespace or following newline`);
    }
  };
  const checkMultilineOperationFolding = async (editor, prefix = '') => {
    const document = testDriver.graphqlExplore.query
      .replace('query ExploreCountries(', 'query ExploreCountries (')
      .replace('$filter: CountryFilterInput = { continent: { eq: "EU" } }',
        '$filter: CountryFilterInput = {\n    continent: {\n      eq: "EU"\n    }\n  }')
      .replace('  $primaryCode:', '  # 🦀 { } ignored\n  $primaryCode:');
    const source = prefix + document.trimEnd();
    setSource(editor, source);
    const history = JSON.stringify(editor.historySize());
    const operation = editor.posFromIndex(source.indexOf('query ExploreCountries')).line;
    const body = editor.posFromIndex(source.indexOf('\n) {', source.indexOf('query ExploreCountries')) + 1).line;
    const folds = () => editor.getAllMarks().filter(mark => mark.__isFold);
    const toggle = async line => {
      editor.setCursor({ line, ch: 0 });
      editor.scrollIntoView({ line, ch: 0 }, 60);
      if (prefix) {
        await wait(() => editor.lineInfo(line).gutterMarkers?.['console-fold-gutter'],
          `A GraphQL fold gutter marker must exist on line ${line}`);
        await click(editor.lineInfo(line).gutterMarkers['console-fold-gutter']);
      } else {
        editor.foldCode({ line, ch: 0 });
      }
    };
    await toggle(operation);
    await wait(() => folds().length === 1, 'The operation header must fold after leading fragments');
    const range = folds()[0].find();
    assert(range.from.line === operation
      && editor.getRange({ line: operation, ch: 0 }, range.from) === 'query ExploreCountries',
    'A multiline operation fold must start on its header, not on the input default object or selection body');
    assert(editor.getRange(range.from, range.to) === source.slice(source.indexOf('query ExploreCountries') + 'query ExploreCountries'.length),
      'The operation fold must include all parameters and the entire body, but not preceding fragments');
    await toggle(operation);
    await wait(() => folds().length === 0, 'The operation header gutter must unfold the same range');
    await toggle(body);
    await wait(() => folds().length === 1, 'The selection body must remain independently foldable');
    const bodyRange = folds()[0].find();
    const hidden = editor.getRange(bodyRange.from, bodyRange.to);
    assert(bodyRange.from.line === body && hidden.includes('...CountryDetails') && hidden.includes('...LanguageDetails')
      && !hidden.includes('$filter: CountryFilterInput'),
    'The body-line fold must hide the full selection without hiding the variable declarations');
    await toggle(body);
    await wait(() => folds().length === 0);
    assert(editor.getValue() === source && JSON.stringify(editor.historySize()) === history,
      'Operation and body folding must preserve query text and undo history');
    const aliased = `${prefix}query Outer {\n  query: country(code: "PL") {\n    code\n  }\n}`;
    setSource(editor, aliased);
    const alias = editor.posFromIndex(aliased.indexOf('  query:')).line;
    await toggle(alias);
    await wait(() => folds().length === 1, 'An alias named query must retain ordinary field folding');
    assert(editor.getRange(folds()[0].find().from, folds()[0].find().to).trim() === 'code',
      'A field named query must not be mistaken for an operation header');
    await toggle(alias);
    await wait(() => folds().length === 0);
  };
  const expectFragmentColors = async editor => {
    for (const [style, expected, color] of [
      ['graphql-fragment', ['PriceTest', 'PriceTest'], 'rgb(241, 250, 140)'],
      ['graphql-type-condition', ['Price', 'SimpleProductView'], 'rgb(62, 156, 112)'],
    ]) {
      const spans = () => [...editor.getWrapperElement().querySelectorAll(`.CodeMirror-line .cm-${style}`)];
      await wait(() => JSON.stringify(spans().map(span => span.textContent).sort()) === JSON.stringify(expected.slice().sort()),
        `Expected ${style} token names: ${JSON.stringify(expected)}`);
      for (const span of spans())
        assert(getComputedStyle(span).color === color,
          `${span.textContent}: expected actual glyph color ${color}, got ${getComputedStyle(span).color}`);
    }
  };
  const checkExploreCountries = async (editor, prefix = '') => {
    const source = prefix + testDriver.graphqlExplore.query;
    const margin = editor.getOption('viewportMargin');
    editor.setOption('viewportMargin', Infinity);
    setSource(editor, source);
    await expectObjectColors(editor,
      ['continent', 'languages', 'country', 'country', 'countries', 'languages'],
      ['code', 'capital', 'currency', 'native']);
    for (const [style, count, color] of [
      ['graphql-fragment', 9, 'rgb(241, 250, 140)'],
      ['graphql-type-condition', 4, 'rgb(62, 156, 112)'],
    ]) {
      const spans = () => [...editor.getWrapperElement().querySelectorAll(`.cm-${style}`)];
      await wait(() => spans().length === count, `ExploreCountries ${style} count: ${count}`);
      assert(spans().every(span => getComputedStyle(span).color === color),
        `ExploreCountries ${style} glyph colors`);
    }
    for (const [needle, typed, candidate] of [
      ['  continent {', '  cont', 'continent'],
      ['    languages {', '    lang', 'languages'],
      ['      ...LanguageDetails', '      ...Lang', 'LanguageDetails'],
      ['    ...CountryDetails', '    ...Cou', 'CountryDetails'],
      ['@skip(if:', '@sk', 'skip'],
      ['@include(if:', '@inc', 'include'],
    ]) {
      const at = source.indexOf(needle, prefix.length);
      assert(at >= 0, `ExploreCountries cursor target: ${needle}`);
      editor.setCursor(editor.posFromIndex(at + typed.length));
      await pick(editor, candidate);
      assert(editor.getValue() === source, `ExploreCountries completion must preserve existing suffixes: ${needle}`);
    }
    const variable = source.indexOf('$hideCurrency)', prefix.length);
    editor.replaceRange('', editor.posFromIndex(variable), editor.posFromIndex(variable + '$hideCurrency'.length));
    editor.setCursor(editor.posFromIndex(variable));
    await key('Control+Space');
    await wait(() => candidates().includes('$hideCurrency') && candidates().includes('$withLanguages'),
      'Directive values in a leading fragment must use Boolean declarations from the later operation');
    assert(!candidates().includes('$primaryCode') && !candidates().includes('$filter'),
      'Directive Boolean arguments must exclude incompatible variable types');
    assert(!candidates().includes('$'), 'Leading fragments must not suggest a redundant standalone $');
    await pick(editor, '$hideCurrency');
    assert(editor.getValue() === source, 'Directive value completion must preserve the existing closing parenthesis');
    const directive = source.indexOf('skip(if: $hideCurrency)', prefix.length);
    editor.replaceRange('', editor.posFromIndex(directive),
      editor.posFromIndex(directive + 'skip(if: $hideCurrency)'.length));
    editor.setCursor(editor.posFromIndex(directive));
    await pick(editor, 'skip()');
    await pick(editor, 'if: ');
    await pick(editor, '$hideCurrency');
    assert(editor.getValue().includes('currency @skip(if: $hideCurrency)'),
      'A directive must be writable through name, parentheses, argument and typed variable hints');
    setSource(editor, source);
    editor.setOption('viewportMargin', margin);
  };
  const bracketMarks = editor => editor.getAllMarks()
    .filter(mark => /^CodeMirror-(?:non)?matchingbracket$/.test(mark.className || ''))
    .map(mark => {
      const range = mark.find();
      assert(editor.indexFromPos(range.to) - editor.indexFromPos(range.from) === 1,
        'Bracket highlights must cover exactly one character');
      return { at: editor.indexFromPos(range.from), className: mark.className };
    }).sort((a, b) => a.at - b.at);
  const expectBracketMarks = async (editor, indices, matching = true) => {
    const expected = indices.slice().sort((a, b) => a - b);
    const className = matching ? 'CodeMirror-matchingbracket' : 'CodeMirror-nonmatchingbracket';
    await wait(() => {
      const marks = bracketMarks(editor);
      return JSON.stringify(marks.map(mark => mark.at)) === JSON.stringify(expected)
        && marks.every(mark => mark.className === className);
    }, () => `Bracket highlights: expected ${JSON.stringify(expected)}, got ${JSON.stringify(bracketMarks(editor))}`);
    for (const span of editor.getWrapperElement().querySelectorAll(`.${className}`))
      assert(getComputedStyle(span).color === 'rgb(255, 85, 85)', 'Both bracket glyphs must be red');
  };
  const checkBracketHighlighting = async (editor, prefix = '') => {
    const source = `${prefix}query ($ids: [String] ) {\n  viewer {\n    name\n  }\n}`;
    setSource(editor, source);
    const history = JSON.stringify(editor.historySize());
    const paren = source.indexOf('(', prefix.length), square = source.indexOf('[', prefix.length);
    const outer = source.indexOf('{', prefix.length), inner = source.indexOf('{', outer + 1);
    for (const pair of [[paren, source.indexOf(')', paren)], [square, source.indexOf(']', square)],
      [outer, source.lastIndexOf('}')], [inner, source.indexOf('}', inner)]]) {
      for (const at of [pair[0], pair[0] + 1, pair[1], pair[1] + 1]) {
        editor.setCursor(editor.posFromIndex(at));
        await expectBracketMarks(editor, pair);
      }
      for (const at of pair) {
        editor.setSelection(editor.posFromIndex(at), editor.posFromIndex(at + 1));
        await expectBracketMarks(editor, pair);
        editor.setSelection(editor.posFromIndex(at + 1), editor.posFromIndex(at));
        await expectBracketMarks(editor, pair);
      }
    }
    editor.setSelections([paren, outer].map(at => ({
      anchor: editor.posFromIndex(at), head: editor.posFromIndex(at),
    })));
    await expectBracketMarks(editor, [paren, source.indexOf(')', paren), outer, source.lastIndexOf('}')]);
    editor.setSelection(editor.posFromIndex(outer), editor.posFromIndex(outer + 2));
    await expectBracketMarks(editor, []);
    editor.setCursor(editor.posFromIndex(source.indexOf('name', prefix.length) + 2));
    await expectBracketMarks(editor, []);
    editor.setCursor(editor.posFromIndex(outer));
    await expectBracketMarks(editor, [outer, source.lastIndexOf('}')]);
    find('[data-testid="service-select"], [data-testid="console-old"]').focus();
    await expectBracketMarks(editor, []);
    editor.focus();
    await expectBracketMarks(editor, [outer, source.lastIndexOf('}')]);
    assert(editor.getValue() === source && JSON.stringify(editor.historySize()) === history,
      'Bracket highlighting must not change text or undo history');
    assert(candidates().length === 0, 'Bracket navigation must not reopen completion menus');
    const ignored = `${prefix}query { viewer(text: "🦀 { ignored }") { name } # [ comment ]\n}`;
    setSource(editor, ignored);
    for (const at of [ignored.indexOf('{ ignored'), ignored.indexOf('ignored }') + 'ignored '.length,
      ignored.indexOf('[ comment')]) {
      assert(at >= prefix.length, 'Ignored bracket fixture must point inside the request body');
      for (const ch of [at, at + 1]) {
        editor.setCursor(editor.posFromIndex(ch));
        await expectBracketMarks(editor, []);
      }
      editor.setSelection(editor.posFromIndex(at), editor.posFromIndex(at + 1));
      await expectBracketMarks(editor, []);
    }
    const opening = ignored.indexOf('{', prefix.length), closing = ignored.lastIndexOf('}');
    editor.setCursor(editor.posFromIndex(opening));
    await expectBracketMarks(editor, [opening, closing]);
    editor.replaceRange(' ', editor.posFromIndex(closing), editor.posFromIndex(closing + 1));
    await expectBracketMarks(editor, [opening], false);
    editor.undo();
    assert(editor.getValue() === ignored, 'Undo must restore the edited bracket');
    editor.setCursor(editor.posFromIndex(opening));
    await expectBracketMarks(editor, [opening, closing]);
    setSource(editor, `${prefix}query { viewer ]`);
    editor.setCursor(editor.posFromIndex(prefix.length + 'query '.length));
    await expectBracketMarks(editor, [prefix.length + 'query '.length], false);
  };
  await choose('rest');
  cm('field-url-editor').setValue(`${fixtureUrl}/web-browser`);
  await click(testid('run-request'));
  await wait(() => testid('response-output').textContent.includes('rust-ui-fixture'));
  await choose('terminal');
  cm('field-endpoint-editor').setValue('/bin/sh -c');
  cm('field-command-editor').setValue('printf rust-web-terminal');
  await click(testid('run-request'));
  await wait(() => testid('response-output').textContent.includes('rust-web-terminal'));
  const fetchReal = window.fetch.bind(window);
  const schema = {
    rest: {basePath: '/api', paths: ['/api/pets'], methodsByPath: {'/api/pets': ['GET']}},
    graphql: { queryType: 'Query', mutationType: 'Mutation', subscriptionType: 'Subscription',
      directives: ['include', 'skip'].map(name => ({ name, isRepeatable: false,
        locations: ['FIELD', 'FRAGMENT_SPREAD', 'INLINE_FRAGMENT'],
        args: [{ name: 'if', type: { kind: 'NON_NULL', ofType: { kind: 'SCALAR', name: 'Boolean' } } }],
      })), types: {
      Query: { fields: [{ name: 'viewer', type: 'User' },
        { name: 'user', type: 'User', args: [
          { name: 'id', type: { kind: 'NON_NULL', ofType: { kind: 'SCALAR', name: 'ID' } } },
          { name: 'filter', type: { kind: 'INPUT_OBJECT', name: 'Filter' } },
          { name: 'state', type: { kind: 'ENUM', name: 'State' } },
        ] },
        { name: 'products', type: 'Product', args: [
          { name: 'skus', type: { kind: 'LIST', ofType: { kind: 'SCALAR', name: 'String' } } },
        ] },
        { name: 'country', type: 'Country', args: [
          { name: 'code', type: { kind: 'NON_NULL', ofType: { kind: 'SCALAR', name: 'ID' } } },
        ] },
        { name: 'countries', type: 'Country', args: [
          { name: 'filter', type: { kind: 'INPUT_OBJECT', name: 'CountryFilterInput' } },
        ] }] },
      Country: { kind: 'OBJECT', fields: [
        { name: 'code', type: 'ID' }, { name: 'name', type: 'String' },
        { name: 'capital', type: 'String' }, { name: 'currency', type: 'String' },
        { name: 'continent', type: 'Continent' }, { name: 'languages', type: 'Language' },
      ] },
      Continent: { kind: 'OBJECT', fields: [{ name: 'code', type: 'ID' }, { name: 'name', type: 'String' }] },
      Language: { kind: 'OBJECT', fields: [{ name: 'code', type: 'ID' }, { name: 'name', type: 'String' }, { name: 'native', type: 'String' }] },
      CountryFilterInput: { kind: 'INPUT_OBJECT', inputFields: [
        { name: 'code', type: 'StringQueryOperatorInput' }, { name: 'continent', type: 'StringQueryOperatorInput' },
      ] },
      StringQueryOperatorInput: { kind: 'INPUT_OBJECT', inputFields: [
        { name: 'eq', type: 'String' }, { name: 'in', type: { kind: 'LIST', ofType: { kind: 'SCALAR', name: 'String' } } },
      ] },
      User: { fields: [{ name: 'address', type: 'Address' }, { name: 'name', type: 'String' }] },
      Address: { fields: [{ name: 'city', type: 'String' }] },
      Mutation: { fields: [{ name: 'updateUser', type: 'User', args: [
        { name: 'input', type: { kind: 'INPUT_OBJECT', name: 'Filter' } },
      ] }] },
      Subscription: { fields: [{ name: 'userChanged', type: 'User', args: [
        { name: 'id', type: { kind: 'SCALAR', name: 'ID' } },
      ] }] },
      Filter: { kind: 'INPUT_OBJECT', inputFields: [
        { name: 'state', type: { kind: 'ENUM', name: 'State' } },
        { name: 'nested', type: { kind: 'INPUT_OBJECT', name: 'Nested' } },
        { name: 'tags', type: { kind: 'LIST', ofType: { kind: 'NON_NULL', ofType: { kind: 'SCALAR', name: 'String' } } } },
      ] },
      Nested: { kind: 'INPUT_OBJECT', inputFields: [
        { name: 'active', type: { kind: 'NON_NULL', ofType: { kind: 'SCALAR', name: 'Boolean' } } },
      ] },
      State: { kind: 'ENUM', enumValues: ['OPEN', 'CLOSED'] },
      ID: { kind: 'SCALAR' },
      String: { kind: 'SCALAR' },
      Boolean: { kind: 'SCALAR' },
      Int: { kind: 'SCALAR' },
      Product: { kind: 'INTERFACE', possibleTypes: ['SimpleProductView', 'ComplexProductView'],
        fields: [{ name: 'sku', type: 'String' }, { name: 'images', type: 'Image', args: [
        { name: 'limit', type: { kind: 'SCALAR', name: 'Int' } },
      ] }] },
      SimpleProductView: { kind: 'OBJECT', fields: [{ name: 'sku', type: 'String' }, { name: 'price', type: 'Price' }] },
      ComplexProductView: { kind: 'OBJECT', fields: [{ name: 'sku', type: 'String' }, { name: 'options', type: 'String' }] },
      Price: { kind: 'OBJECT', fields: [{ name: 'amount', type: 'Int' }] },
      Image: { fields: [{ name: 'url', type: 'String' }] },
    } },
    grpc: { services: ['demo.Greeter'], methods: ['Hello'] },
    mysql: { tables: [
      { name: 'users', columns: [{ name: 'email' }, { name: 'id' }, { name: 'status' },
        ...Array.from({ length: 150 }, (_, i) => ({ name: `field_${i}` }))] },
      { name: 'orders', columns: [{ name: 'user_id' }, { name: 'total' }] },
    ] },
  };
  let discovered = 0;
  let discoveryGate = null;
  let requestGate = null;
  const grpcStageRequests = [];
  const graphqlRecoveryRequests = [];
  const grpcRepeatedFields = [
    { name: 'settings', kind: 'object', fields: [{ name: 'locale', kind: 'string' }] },
    { name: 'products_by_store_view', kind: 'array', items: { kind: 'object', fields: [
      { name: 'store_view_id', kind: 'object', fields: [
        { name: 'environment_id', kind: 'string' },
        { name: 'website_code', kind: 'string' },
        { name: 'store_code', kind: 'string' },
        { name: 'store_view_code', kind: 'string' },
      ] },
      { name: 'skus', kind: 'array', items: { kind: 'string' } },
      { name: 'store_view_code', kind: 'string' },
      { name: 'products', kind: 'array', items: { kind: 'object', fields: [
        { name: 'sku', kind: 'string' },
        { name: 'tags', kind: 'array', items: { kind: 'string' } },
      ] } },
    ] } },
    { name: 'flags', kind: 'array', items: { kind: 'boolean' } },
    { name: 'ids', kind: 'array', items: { kind: 'number' } },
  ];
  let countryRequest = null;
  const exploreRequests = [];
  window.fetch = async (input, options) => {
    const request = new Request(input, options);
    const url = new URL(request.url);
    const message = url.pathname === '/api/console' ? await request.clone().json() : null;
    const servicePath = message ? ({
      rest: '/api/services/rest/request',
      mysql: '/api/services/mysql/query',
      graphql: '/api/services/graphql/request',
    }[message.service] || url.pathname) : url.pathname;
    if (url.pathname === '/api/console/discover') {
      discovered++;
      const message = await request.json();
      if (message.service === 'grpc' && message.params.endpoint.includes('grpc-stage'))
        grpcStageRequests.push(message.params.endpoint);
      if (message.service === 'graphql' && message.params.url.includes('graphql-recovery'))
        graphqlRecoveryRequests.push(message.params);
      const gate = discoveryGate;
      if (gate && message.service === gate.service
        && Object.values(message.params).includes(gate.endpoint)) {
        gate.captured = message;
        await gate.blocked;
        gate.returned = true;
        if (gate.failure)
          return Response.json({ error: gate.failure }, { status: gate.httpError ? 503 : 200 });
      }
      if (message.service === 'grpc' && message.params.endpoint.includes('grpc-stage')) {
        const path = new URL(message.params.endpoint).pathname.slice(1);
        if (!path.includes('/'))
          return Response.json({ services: ['stage.Greeter', 'stage.Other'] });
        const [service, method] = path.split('/');
        const result = { methods: service === 'stage.Greeter' ? ['Hello', 'Goodbye'] : ['Ping'] };
        if (method)
          result.fields = [
            { name: service === 'stage.Greeter' ? 'greeting' : 'probe', kind: 'string' },
            ...grpcRepeatedFields,
          ];
        return Response.json(result);
      }
      if (message.service === 'graphql' && message.params.url.includes('graphql-recovery')) {
        const token = message.params.headers?.['X-Token'];
        if (!['recovery-token', 'recovery-latest'].includes(token))
          return Response.json({ error: 'GraphQL discovery requires X-Token' });
        return Response.json({ queryType: 'CatalogQuery', types: {
          CatalogQuery: { kind: 'OBJECT', fields: [
            { name: token === 'recovery-latest' ? 'latestCatalog' : 'initialCatalog', type: 'Product' },
          ] },
          Product: { kind: 'OBJECT', fields: [{ name: 'sku', type: 'String' }] },
          String: { kind: 'SCALAR' },
        } });
      }
      if (message.service === 'rest' && message.params.url.includes('rest-missing.fixture.invalid'))
        return Response.json({});
      if (message.service === 'rest' && message.params.url.includes('rest-warning.fixture.invalid'))
        return Response.json({warning: 'No OpenAPI or Swagger discovery is available for this API'});
      if (message.service === 'rest' && message.params.url.includes('rest-prefix.fixture.invalid')) {
        const url = new URL(message.params.url);
        const site = url.pathname.split('/')[1];
        if (!['site1', 'site2'].includes(site))
          return Response.json({warning: 'No OpenAPI or Swagger schema at this prefix'});
        const path = `/${site}/rest/v1/product`;
        return Response.json({baseUrl: `${url.origin}/${site}`, basePath: `/${site}`,
          paths: [path], methodsByPath: {[path]: ['POST']},
          bodySchemas: {[`POST ${path}`]: [{name: site, kind: 'string', required: true}]}});
      }
      if (message.service === 'rest' && message.params.url.includes('rest-root.fixture.invalid')) {
        const origin = new URL(message.params.url).origin;
        return Response.json({baseUrl: origin, basePath: '',
          paths: ['/site1/rest/v1/product', '/site2/rest/v1/product'],
          methodsByPath: {'/site1/rest/v1/product': ['POST'], '/site2/rest/v1/product': ['POST']}});
      }
      if (message.service === 'rest' && message.params.url.startsWith('https://petstore3.swagger.io/'))
        return Response.json({ basePath: '/api/v3', paths: ['/api/v3/pet/findByStatus'],
          methodsByPath: {'/api/v3/pet/findByStatus': ['GET']} });
      return Response.json(schema[message.service] || {});
    }
    if (servicePath === '/api/services/rest/request') {
      const params = message ? message.params : await request.json();
      const gate = requestGate;
      if (gate && params.url === gate.endpoint) {
        gate.captured = params;
        await gate.blocked;
        gate.returned = true;
      }
      if (new URL(params.url).pathname === '/failure')
        return Response.json({ error: 'UI error fixture' }, { status: 400 });
      if (new URL(params.url).pathname === '/server-failure')
        return Response.json({ status: 503, body: '{"error":"Server unavailable"}', durationMs: 1 });
      return Response.json({ status: 200, headers: { 'x-ui-parity': 'yes' },
        body: JSON.stringify({ marker: 'ui-parity-result', nested: { inner: { leaf: 'retained-node' } } }),
        durationMs: 1, debug: { phase: 'ui-test' } });
    }
    if (servicePath === '/api/services/mysql/query')
      return Response.json({ columns: ['id', 'payload', 'typed'],
        columnTypes: {id:'number',payload:'text',typed:'json'},
        rows: [{ id: 1, payload: null, typed:null },
          { id: 2, payload: '{"nested":{"flag":true}}', typed:'{"nested":{"flag":true}}' }],
        durationMs: 1, debug: { phase: 'ui-test' } });
    if (servicePath === '/api/services/graphql/request') {
      const params = message ? message.params : await request.clone().json();
      if (params.url === 'https://countries.explore.fixture.invalid/graphql') {
        const variables = typeof params.variables === 'string' ? JSON.parse(params.variables) : params.variables;
        assert(params.query.trim() === testDriver.graphqlExplore.query,
          'ExploreCountries must submit the exact fragment-first reference with directives and defaults unchanged');
        assert(variables.primaryCode === 'PL' && variables.filter.code.in.join(',') === 'PL,DE,FR'
          && variables.withLanguages === true && variables.hideCurrency === false
          && !('secondaryCode' in variables), 'ExploreCountries must preserve Variables and operation defaults');
        exploreRequests.push(params);
        const countries = [
          { code: 'DE', displayName: 'Germany', capital: 'Berlin', currency: 'EUR', language: ['de', 'German', 'Deutsch'] },
          { code: 'FR', displayName: 'France', capital: 'Paris', currency: 'EUR', language: ['fr', 'French', 'Français'] },
          { code: 'PL', displayName: 'Poland', capital: 'Warsaw', currency: 'PLN', language: ['pl', 'Polish', 'Polski'] },
        ].map(({ language: [code, name, native], ...country }) => ({
          __typename: 'Country', ...country, continent: { code: 'EU', name: 'Europe' },
          languages: [{ code, name, native }],
        }));
        return Response.json({ status: 200, headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ data: { primary: countries[2], secondary: countries[0],
            matchingCountries: countries.map(({ __typename, code, displayName, languages }) =>
              ({ __typename, code, displayName, languages })) } }), durationMs: 1 });
      }
      if (params.url === 'https://countries.fixture.invalid/graphql') {
        countryRequest = params;
        const variables = typeof params.variables === 'string' ? JSON.parse(params.variables) : params.variables;
        assert(variables.first === 'PL' && variables.second === 'DE', 'Countries must receive the Variables panel values');
        assert(params.query.startsWith('fragment CountryFields on Country')
          && params.query.indexOf('fragment CountryFields') < params.query.indexOf('query CompareCountries'),
        'Fragment definitions must precede the query in the submitted document');
        assert((params.query.match(/\.\.\.CountryFields/g) || []).length === 2,
          'Both country aliases must reuse the same fragment without expanding it inline');
        return Response.json({ status: 200, headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ data: {
            firstCountry: { code: 'PL', name: 'Poland', capital: 'Warsaw', currency: 'PLN' },
            secondCountry: { code: 'DE', name: 'Germany', capital: 'Berlin', currency: 'EUR' },
          } }), durationMs: 1, debug: { phase: 'ui-test' } });
      }
    }
    return fetchReal(request);
  };
  const gateFor = (service, endpoint, failure = null, httpError = false) => {
    let release;
    return { service, endpoint, failure, httpError,
      blocked: new Promise(resolve => { release = resolve; }), release: () => release() };
  };
  const checkIncompleteDiscoveryAddresses = async () => {
    const alerts = () => [...document.querySelectorAll('[role="alert"]')].map(el => el.textContent);
    for (const [service, scheme] of [
      ['graphql', 'https:'], ['rest', 'https:'], ['grpc', 'grpcs:'], ['mysql', 'mysql:'],
    ]) {
      await click(testid(`console-service-${service}`));
      const editor = cm('console-editor');
      setSource(editor, `#check incomplete\n#endpoint\n${service === 'rest' ? 'GET ' : ''}`);
      await new Promise(resolve => requestAnimationFrame(resolve));
      const count = discovered;
      const previousAlerts = JSON.stringify(alerts());
      const previousStatus = find('.console-status').textContent;
      await type(scheme);
      await sleep(550);
      await type('//');
      await sleep(550);
      assert(find('.console-status').textContent === previousStatus,
        `${service}: incomplete typing must not start discovery or replace the status: ${JSON.stringify({before: previousStatus, after: find('.console-status').textContent})}`);
      await key('Escape');
      await key('Enter');
      if (service === 'rest') {
        await sleep(50);
        assert(find('.console-status').textContent === previousStatus,
          'Enter in a REST URL must not trigger discovery or validation');
        setSource(editor, `${editor.getValue()}\n#body\n`);
        await key('Control+Space');
      }
      await wait(() => find('.console-status').classList.contains('is-warning'),
        `${service}: committing an invalid address must show a local warning, not stale green success`);
      assert(discovered === count
        && JSON.stringify(alerts()) === previousAlerts,
      `${service}: typing an incomplete scheme must send no discovery and show no errors`);
      await type('{{:unfinished');
      await sleep(550);
      assert(discovered === count && JSON.stringify(alerts()) === previousAlerts,
        `${service}: an incomplete endpoint variable must not trigger discovery`);
      await key('Escape');
    }
    for (const [service, field, scheme] of [
      ['graphql', 'url', 'https://'], ['rest', 'url', 'https://'],
      ['grpc', 'endpointText', 'grpcs://'], ['mysql', 'dsn', 'mysql://'],
    ]) {
      await choose(service);
      const editor = cm(`field-${field}-editor`);
      setSource(editor, '');
      const count = discovered;
      const previousAlerts = JSON.stringify(alerts());
      await type(scheme);
      await sleep(550);
      assert(discovered === count && JSON.stringify(alerts()) === previousAlerts,
        `${service} form: an incomplete endpoint must not send discovery or display errors`);
      await key('Escape');
    }
    await choose('console');
    await click(testid('console-service-graphql'));
    const editor = cm('console-editor');
    setSource(editor, '#check commit-url\n#endpoint\n');
    const endpoint = 'https://events.fixture.invalid/graphql';
    const count = discovered;
    const gate = discoveryGate = gateFor('graphql', endpoint);
    await type(endpoint);
    await sleep(650);
    assert(discovered === count, 'Even a syntactically valid GraphQL URL must wait for a completion event, not a typing pause');
    await key('Enter');
    await wait(() => gate.captured, 'Enter after a complete endpoint must commit automatic discovery');
    gate.release();
    await wait(() => find('.console-status').textContent.includes('Discovery · OK'));
    discoveryGate = null;
    const leaveEndpoint = `${endpoint}?event=leave`;
    setSource(editor, `#check leave-connection\n#endpoint\n${leaveEndpoint}\n#headers\nX-Token: committed-token\n#query\n{\n  `);
    editor.setCursor(2, editor.getLine(2).length);
    const leave = discoveryGate = gateFor('graphql', leaveEndpoint);
    editor.setCursor(editor.lastLine(), editor.getLine(editor.lastLine()).length);
    await wait(() => leave.captured, 'Leaving the connection section must commit discovery');
    assert(leave.captured.params.headers['X-Token'] === 'committed-token',
      'A connection commit must include the finished authentication headers');
    leave.release();
    await wait(() => find('.console-status').textContent.includes('Discovery · OK'));
    const paste = text => {
      const clipboard = new DataTransfer();
      clipboard.setData('text/plain', text);
      editor.getInputField().dispatchEvent(new ClipboardEvent('paste', {
        clipboardData: clipboard, bubbles: true, cancelable: true,
      }));
    };
    const pastedEndpoint = `${endpoint}?event=paste`;
    editor.setSelection({line: 2, ch: 0}, {line: 2, ch: editor.getLine(2).length});
    const pasted = discoveryGate = gateFor('graphql', pastedEndpoint);
    paste(pastedEndpoint);
    await wait(() => pasted.captured, 'Pasting a complete URL must commit discovery without Enter');
    assert(editor.getLine(2) === pastedEndpoint, 'The paste fixture must insert through the real CodeMirror input');
    pasted.release();
    await wait(() => find('.console-status').textContent.includes('Discovery · OK'));
    discoveryGate = null;
    const beforeBody = discovered;
    setSource(editor, `#check body-paste\n#endpoint\n${endpoint}?event=uncommitted\n#query\n{\n  `);
    paste('viewer { name }');
    await sleep(600);
    assert(discovered === beforeBody,
      'Body paste and programmatic document replacement must not commit an unconfirmed new URL');
    await key('Escape');
    editor.setCursor(2, editor.getLine(2).length);
    editor.getInputField().blur();
    setSource(editor, `#check superseding-draft\n#endpoint\n${endpoint}?event=new-draft`);
    await sleep(600);
    assert(discovered === beforeBody,
      'A deferred blur from the old document must not discover a superseding uncommitted URL');
  };
  const checkGraphqlDiscoveryRecovery = async () => {
    await click(testid('console-service-graphql'));
    const editor = cm('console-editor');
    const status = () => find('.console-status').textContent;
    const source = (endpoint, token = '') =>
      `#check graphql-recovery\n#endpoint\n${endpoint}\n#headers\n${token ? `X-Token: ${token}` : ''}\n#query\n{\n  `;
    const commitHeaders = async () => {
      const line = editor.getValue().split('\n').findIndex(line => line.startsWith('X-Token:'));
      editor.setCursor(line, editor.getLine(line).length);
      await key('Enter');
      editor.setCursor(editor.lastLine(), editor.getLine(editor.lastLine()).length);
    };
    const endpoint = 'https://graphql-recovery.fixture.invalid/protected';
    setSource(editor, source(endpoint));
    const denied = discoveryGate = gateFor('graphql', endpoint);
    await key('Alt+Space');
    await wait(() => denied.captured);
    denied.release();
    await wait(() => status().includes('Discovery · ERROR') && status().includes('requires X-Token'));
    await key('Control+Space');
    assert(!candidates().includes('products') && !candidates().includes('initialCatalog'),
      'Failed discovery must not offer fields from a previously discovered GraphQL endpoint');
    const failedCount = discovered;
    const failedStatus = status();
    await pick(editor, '__typename');
    await type('\n  ');
    await sleep(600);
    assert(discovered === failedCount && status() === failedStatus,
      'Typing and choosing hints after a failed discovery must not repeatedly retry the unchanged connection');
    const retry = discoveryGate = gateFor('graphql', endpoint);
    await key('Alt+Space');
    await wait(() => retry.captured, 'Manual Discovery must explicitly retry a previously failed connection');
    retry.release();
    await wait(() => discovered === failedCount + 1 && status().includes('Discovery · ERROR'));
    await key('Escape');
    assert(![...document.querySelectorAll('[role="alert"]')].some(visible),
      'Manual GraphQL discovery errors must also stay in status/debug, not a top banner');
    setSource(editor, source(endpoint, 'recovery-token'));
    const authenticated = discoveryGate = gateFor('graphql', endpoint);
    await commitHeaders();
    await wait(() => authenticated.captured);
    assert(authenticated.captured.params.headers['X-Token'] === 'recovery-token',
      'Automatic GraphQL discovery must use Console request headers');
    authenticated.release();
    await wait(() => status().includes('Discovery · OK') && candidates().includes('initialCatalog'),
      'Fixing headers must retry discovery and automatically replace __typename-only fallback with schema fields');
    const pendingEndpoint = 'https://graphql-recovery.fixture.invalid/pending';
    setSource(editor, source(pendingEndpoint, 'recovery-token'));
    const first = discoveryGate = gateFor('graphql', pendingEndpoint);
    await commitHeaders();
    await wait(() => first.captured);
    setSource(editor, source(pendingEndpoint, 'recovery-latest'));
    const latest = discoveryGate = gateFor('graphql', pendingEndpoint);
    await commitHeaders();
    await sleep(500);
    first.release();
    await wait(() => latest.captured, 'Changed GraphQL headers must queue discovery while the previous request is pending');
    assert(latest.captured.params.headers['X-Token'] === 'recovery-latest',
      'Queued introspection must use the latest header values, not the previous request');
    await key('Control+Space');
    assert(!candidates().includes('initialCatalog'), 'A superseded GraphQL reply must not expose stale fields');
    latest.release();
    await wait(() => candidates().includes('latestCatalog') && !candidates().includes('initialCatalog'),
      'The latest queued introspection result must refresh field suggestions without further typing');
    assert(graphqlRecoveryRequests.filter(params => params.url === pendingEndpoint).length === 2,
      'A header change during introspection must send exactly the active and latest requests');
    const cachedCount = discovered;
    const cachedStatus = status();
    await pick(editor, 'latestCatalog');
    await key('Space');
    const indent = editor.getLine(editor.getCursor().line).match(/^[ \t]*/)[0];
    await pick(editor, `{\n${indent}  \n${indent}}`);
    await wait(() => candidates().includes('sku'), 'Nested fields must be available from the already loaded complete schema');
    await pick(editor, 'sku');
    await sleep(600);
    assert(discovered === cachedCount && status() === cachedStatus,
      'GraphQL typing and field completion must use the loaded schema without any new discovery request');
    const selectedEndpoint = 'https://graphql.fixture.invalid';
    const partial = 'https://gr';
    setSource(editor, source(partial, 'recovery-token'));
    editor.setCursor(2, partial.length);
    const selected = discoveryGate = gateFor('graphql', selectedEndpoint);
    await pick(editor, selectedEndpoint);
    await wait(() => selected.captured,
      'Choosing a complete GraphQL endpoint must start discovery without another typed character');
    assert(selected.captured.params.headers['X-Token'] === 'recovery-token',
      'Endpoint-completion discovery must retain headers from the selected check');
    selected.release();
    await wait(() => selected.returned && status().includes('Discovery · OK'));
    editor.setCursor(editor.lastLine(), editor.getLine(editor.lastLine()).length);
    await key('Control+Space');
    await wait(() => candidates().includes('viewer') && candidates().includes('products'),
      'An endpoint selected entirely through completion must produce real GraphQL field suggestions');
    await key('Escape');
    discoveryGate = null;
  };
  const checkGrpcBodyBootstrap = async (editor, prefix = '') => {
    await sleep(500);
    const endpoint = `grpc://grpc-stage-body-${prefix ? 'console' : 'form'}.fixture.invalid/stage.Greeter/Hello`;
    const bodyPrefix = prefix ? `${prefix}${endpoint}\n#message\n` : '';
    if (!prefix)
      setSource(cm('field-endpointText-editor'), endpoint);
    setSource(editor, bodyPrefix);
    const gate = discoveryGate = gateFor('grpc', endpoint);
    await wait(() => gate.captured,
      'Focusing a gRPC body with an already complete endpoint must bootstrap its missing schema');
    await key('Control+Space');
    await pick(editor, '{\n  \n}');
    gate.release();
    await wait(() => candidates().includes('"greeting": ""') && candidates().includes('"products_by_store_view": []'),
      'The gRPC body must automatically show reflected fields after selecting its object, without further typing');
    assert(editor.getValue() === `${bodyPrefix}{\n  \n}`,
      'Schema loading must preserve the body template and its nested-field cursor');
    await pick(editor, '"greeting": ""');
    await type('fixture');
    await sleep(600);
    assert(grpcStageRequests.filter(value => value === endpoint).length === 1,
      'Loaded gRPC body fields and subsequent editing must reuse the schema without another discovery request');
    await key('Escape');
    discoveryGate = null;
  };
  const checkGrpcArrayItems = async (editor, prefix) => {
    const previousHeight = editor.getWrapperElement().style.height;
    editor.setSize(null, Math.max(editor.getScrollInfo().clientHeight, editor.defaultTextHeight() * 24));
    const object = () => {
      const indent = editor.getLine(editor.getCursor().line).match(/^[ \t]*/)[0];
      return `{\n${indent}  \n${indent}}`;
    };
    const expect = text => wait(() => candidates().includes(text),
      () => `Expected automatic gRPC array completion ${JSON.stringify(text)}: ${JSON.stringify({
        source: editor.getValue(), cursor: editor.getCursor(), candidates: candidates(),
      })}`);
    const compact = (text, label) => {
      const item = editor.state.completionActive?.data?.list.find(item => item.text === text);
      assert(item?.displayText === label && !item.displayText.includes('\n'),
        `gRPC JSON snippets must have compact single-line labels: ${JSON.stringify(item)}`);
      assert([...document.querySelectorAll('.CodeMirror-hint')].some(el => el.textContent === label),
        `The actual gRPC menu must render ${label}`);
    };
    try {
      setSource(editor, prefix);
      await key('Control+Space');
      await expect('{\n  \n}');
      compact('{\n  \n}', '{} — JSON object');
      await pick(editor, '{\n  \n}');
      await expect('"settings": {\n    \n  }');
      compact('"settings": {\n    \n  }', '"settings": {} — object');
      await pick(editor, '"settings": {\n    \n  }');
      await expect('"locale": ""');
      assert(JSON.stringify(JSON.parse(editor.getValue().slice(prefix.length))) === '{"settings":{}}',
        'Compact gRPC menu labels must preserve multiline insertion and the nested-field cursor');
      setSource(editor, `${prefix}{\n  prod\n}`);
      editor.setCursor(editor.posFromIndex(prefix.length + '{\n  prod'.length));
      await pick(editor, '"products_by_store_view": []');
      await expect(object());
      compact(object(), '{} — JSON object');
      await pick(editor, object());
      await expect('"store_view_code": ""');
      assert(!candidates().includes('"greeting": ""') && !candidates().includes('"products_by_store_view": []'),
        'An array element must offer its own message fields, not ancestor fields');
      await pick(editor, '"store_view_code": ""');
      await type('default');
      await key('ArrowRight');
      await type(',');
      await key('Escape');
      await key('Enter');
      await expect('"products": []');
      assert(!candidates().includes('"store_view_code": ""'), 'Already used fields must be excluded in this array element');
      await pick(editor, '"products": []');
      await expect(object());
      await pick(editor, object());
      await expect('"sku": ""');
      await pick(editor, '"sku": ""');
      await type('sku-1');
      await key('ArrowRight');
      await type(', ');
      await expect('"tags": []');
      await pick(editor, '"tags": []');
      await expect('""');
      await pick(editor, '""');
      await type('sku-tag');
      await key('ArrowRight');
      await type(', ');
      await expect('""');
      await pick(editor, '""');
      await type('sku-tag-2');
      await key('ArrowRight');
      const closing = editor.getValue().indexOf('\n  }]', prefix.length);
      assert(closing !== -1, 'Nested array snippets must preserve the outer object and array delimiters');
      editor.setCursor(editor.posFromIndex(closing + '\n  }'.length));
      await type(', ');
      await expect(object());
      await pick(editor, object());
      await expect('"store_view_code": ""');
      await pick(editor, '"store_view_code": ""');
      await type('second');
      await key('ArrowRight');
      assert(JSON.stringify(JSON.parse(editor.getValue().slice(prefix.length))) === JSON.stringify({
        products_by_store_view: [
          { store_view_code: 'default', products: [{ sku: 'sku-1', tags: ['sku-tag', 'sku-tag-2'] }] },
          { store_view_code: 'second' },
        ],
      }), 'Keyboard completion must produce valid JSON with nested repeated fields and independent array elements');
      const completedObject = [
        '{', '  "products_by_store_view": [{', '    "store_view_id": {',
        '      "environment_id": "{{:env_id}}",',
        '      "website_code": "{{:website}}",',
        '      "store_code": "{{:store_code}}",',
        '      "store_view_code": "{{:store_view}}"',
        '    }',
      ].join('\n');
      const objectTail = '\n  }]\n}';
      setSource(editor, prefix + completedObject + objectTail);
      editor.setCursor(editor.posFromIndex(prefix.length + completedObject.length));
      await key('Enter');
      const entered = editor.getValue();
      assert(entered.startsWith(prefix + completedObject) && entered.endsWith(objectTail)
        && /^\n[ \t]*$/.test(entered.slice(prefix.length + completedObject.length, -objectTail.length)),
      'Enter after a closed nested object must only insert an indented newline, not a comma');
      await expect('"skus": []');
      assert(!candidates().some(text => text.startsWith('"store_view_id":')),
        'Enter after a nested object must return to unused parent fields');
      await pick(editor, '"skus": []');
      assert(editor.getValue().startsWith(prefix + completedObject + ',\n'),
        'Choosing the next parent field must insert the missing comma immediately after the previous object');
      const commaBody = JSON.parse(editor.getValue().slice(prefix.length));
      assert(JSON.stringify(commaBody) === JSON.stringify({ products_by_store_view: [{
        store_view_id: {
          environment_id: '{{:env_id}}', website_code: '{{:website}}',
          store_code: '{{:store_code}}', store_view_code: '{{:store_view}}',
        },
        skus: [],
      }] }), 'Automatic comma insertion must preserve every existing placeholder and closing delimiter');
      await expect('""');
      setSource(editor, prefix + '{}');
      await key('Enter');
      await sleep(50);
      assert(candidates().length === 0 && editor.getValue() === prefix + '{}\n',
        'A completed root object must not offer parent fields or add a trailing comma');
      for (const [field, expected] of [
        ['products_by_store_view', ['{\n      \n    }']],
        ['flags', ['true', 'false']],
        ['ids', ['0']],
      ]) {
        const before = `{\n  "${field}": [\n    `;
        setSource(editor, `${prefix}${before}\n  ]\n}`);
        editor.setCursor(editor.posFromIndex(prefix.length + before.length));
        await key('Control+Space');
        await wait(() => JSON.stringify(candidates()) === JSON.stringify(expected),
          `A multiline ${field} array must offer exactly the protobuf element type`);
        await key('Escape');
      }
    } finally {
      editor.setSize(null, previousHeight);
    }
  };
  const checkGrpcStagedDiscovery = async (editor, prefix = '') => {
    const name = prefix ? 'console' : 'form';
    const authority = `grpc://grpc-stage-${name}.fixture.invalid:50051`;
    const immediate = async (action, gate) => {
      const original = window.setTimeout;
      let debounced = false;
      window.setTimeout = function(callback, delay, ...args) {
        if (delay === 450) debounced = true;
        return original.call(window, callback, delay, ...args);
      };
      try {
        await action();
        await wait(() => gate.captured, () => `gRPC stage did not start without retyping /: ${JSON.stringify(grpcStageRequests)}`);
      } finally {
        window.setTimeout = original;
      }
      assert(!debounced, 'Explicit gRPC path separators and selected completions must bypass the typing debounce');
    };
    setSource(editor, prefix + authority);
    const services = discoveryGate = gateFor('grpc', `${authority}/`);
    await immediate(() => type('/'), services);
    assert(!candidates().includes('demo.Greeter/') && !candidates().includes('Hello'),
      'Pending endpoint discovery must not expose another target schema');
    services.release();
    await wait(() => candidates().includes('stage.Greeter/') && candidates().includes('stage.Other/'),
      'Typing the first / must automatically show the returned services');
    const methods = discoveryGate = gateFor('grpc', `${authority}/stage.Greeter/`);
    await immediate(() => pick(editor, 'stage.Greeter/'), methods);
    assert(editor.getValue() === `${prefix}${authority}/stage.Greeter/`,
      'Choosing a service must insert exactly one trailing slash');
    assert(!candidates().includes('Hello'), 'A services-only reply must not masquerade as a methods schema');
    methods.release();
    await wait(() => candidates().includes('Hello') && candidates().includes('Goodbye'),
      'Choosing a service must automatically request and show its methods');
    const fields = discoveryGate = gateFor('grpc', `${authority}/stage.Greeter/Hello`);
    await immediate(() => pick(editor, 'Hello'), fields);
    fields.release();
    await wait(() => fields.returned);
    await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    assert(candidates().length === 0, 'A selected complete method must not reopen its method menu');
    discoveryGate = null;
    const message = prefix ? editor : cm('field-requestMessage-editor');
    setSource(message, `${prefix ? `${prefix}${authority}/stage.Greeter/Hello\n#message\n` : ''}{\n  gr`);
    await pick(message, '"greeting": ""');
    await checkGrpcArrayItems(message, prefix ? `${prefix}${authority}/stage.Greeter/Hello\n#message\n` : '');
    const target = `grpc://grpc-stage-${name}-queued.fixture.invalid:50051`;
    setSource(editor, prefix + target);
    const first = discoveryGate = gateFor('grpc', `${target}/`);
    await immediate(() => type('/'), first);
    await type('stage.Other/');
    const queued = discoveryGate = gateFor('grpc', `${target}/stage.Other/`);
    first.release();
    await wait(() => queued.captured,
      'An endpoint changed during reflection must queue discovery for the latest service');
    assert(!candidates().includes('Hello'), 'A different service must not inherit the previous methods');
    queued.release();
    await wait(() => candidates().length === 1 && candidates()[0] === 'Ping',
      'The queued service must show its own methods without another slash edit');
    const clickedTarget = `grpc://grpc-stage-${name}-clicked.fixture.invalid:50051`;
    setSource(editor, prefix + clickedTarget);
    const clicked = discoveryGate = gateFor('grpc', `${clickedTarget}/`);
    await immediate(() => type('/'), clicked);
    await click(editor.getWrapperElement(), 12, 8);
    clicked.release();
    await wait(() => clicked.returned);
    await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    assert(candidates().length === 0, 'A delayed gRPC reply must respect an explicit mouse dismissal');
    editor.setCursor(editor.lastLine(), editor.getLine(editor.lastLine()).length);
    await key('Control+Space');
    await wait(() => candidates().includes('stage.Greeter/'), 'Dismissed reflection data must remain available manually');
    await key('Escape');
    discoveryGate = null;
    await sleep(500);
    for (const endpoint of [
      `${authority}/`, `${authority}/stage.Greeter/`, `${authority}/stage.Greeter/Hello`,
      `${target}/`, `${target}/stage.Other/`, `${clickedTarget}/`
    ])
      assert(grpcStageRequests.filter(value => value === endpoint).length === 1,
        `gRPC stage must not repeat after the old debounce interval: ${endpoint}`);
  };
  const checkRestPathInsertion = async () => {
    await click(testid('console-service-rest'));
    const editor = cm('console-editor');
    const path = '/api/v3/pet/findByStatus';
    for (const spacing of [' ', '  ', ' \u00a0', '\u00a0', '\t']) {
      const prefix = `GET${spacing}https://petstore3.swagger.io`;
      setSource(editor, `#check test1\n#endpoint\n${prefix}/api/v3/`);
      await commitRestUrl(editor);
      await testDriver.suggest(editor, path);
      const loaded = discovered;
      await pick(editor, path);
      assert(editor.getLine(2) === `${prefix}${path}`,
        `REST path insertion must preserve the entire host and method separator ${JSON.stringify(spacing)}`);
      await sleep(60);
      assert(discovered === loaded, 'Selecting a path from the loaded REST API must reuse its full schema');
    }
    requestGate = gateFor('rest', `https://petstore3.swagger.io${path}`);
    await key('Shift+Enter');
    await wait(() => requestGate.captured, 'The completed REST URL must reach request parsing without host truncation');
    assert(requestGate.captured.method === 'GET', 'REST completion must preserve the HTTP method');
    requestGate.release();
    await wait(() => editor.getValue().includes('ui-parity-result'));
    const header = editor.getValue().split('\n').find(line => line.startsWith('#responce '));
    assert(header.endsWith(' · OK') && !header.includes('1ms'),
      'Successful response comments must also contain only time and status');
    requestGate = null;
  };
  const checkRestDiscoveryOutcomes = async () => {
    await click(testid('console-service-rest'));
    const editor = cm('console-editor');
    const status = () => find('.console-status');
    const source = endpoint => `#check discovery-result\n#endpoint\nPOST ${endpoint}\n#body\n`;
    const endpoint = 'https://rest-outcomes.fixture.invalid/api/';
    setSource(editor, source(endpoint));
    const initial = discovered;
    await key('Control+Space');
    await wait(() => discovered === initial + 1 && status().textContent.includes('Discovery · OK'));
    let count = discovered;
    setSource(editor, source('hhttp://fake.com'));
    await key('Control+Space');
    await wait(() => status().classList.contains('is-warning'));
    assert(status().textContent.includes('Invalid') && !status().classList.contains('is-ok') && discovered === count,
      'An invalid URL must replace stale green discovery with a local warning without issuing network requests');
    for (const host of ['rest-missing', 'rest-warning']) {
      const missing = `https://${host}.fixture.invalid/api/`;
      setSource(editor, source(missing));
      const before = discovered;
      await key('Control+Space');
      await wait(() => discovered === before + 1 && status().classList.contains('is-warning')
        && status().textContent.includes('No OpenAPI or Swagger'));
      count = discovered;
      setSource(editor, `${source(missing)}\n#body\n`);
      await type('{}');
      await sleep(100);
      assert(discovered === count && !status().classList.contains('is-ok'),
        'Neither an empty legacy discovery result nor a no-schema warning may be treated as a loaded schema or retried by body input');
      for (let index = 0; index < 12; index++) await key('Control+Space');
      await sleep(100);
      assert(discovered === count, 'Body hint requests must not repeatedly retry a failed URL');
    }
    const pendingEndpoint = 'https://rest-pending.fixture.invalid/api/';
    setSource(editor, source(pendingEndpoint));
    const pending = discoveryGate = gateFor('rest', pendingEndpoint);
    await key('Control+Space');
    await wait(() => pending.captured);
    setSource(editor, source('hhttp://fake.com'));
    await key('Control+Space');
    await wait(() => status().classList.contains('is-warning'));
    const warning = status().textContent;
    pending.release();
    await wait(() => pending.returned);
    await sleep(100);
    assert(status().textContent === warning && !status().classList.contains('is-ok'),
      'A reply for the previous valid URL must not overwrite the new invalid-URL warning with stale success');
    discoveryGate = null;
    const headerEndpoint = 'https://rest-headers.fixture.invalid/api/';
    const authenticated = token => `#check headers\n#endpoint\nPOST ${headerEndpoint}\n#headers\nX-Token: ${token}\n#body\n{}`;
    setSource(editor, authenticated('first-token'));
    const first = discoveryGate = gateFor('rest', headerEndpoint);
    await key('Control+Space');
    await wait(() => first.captured);
    setSource(editor, authenticated('latest-token'));
    const headerCount = discovered;
    await key('Control+Space');
    await sleep(50);
    first.release();
    await wait(() => first.returned);
    editor.replaceRange('GET', {line: 2, ch: 0}, {line: 2, ch: 4});
    editor.setCursor(2, editor.getLine(2).length);
    await testDriver.suggest(editor, '/api/pets');
    await key('Control+Space');
    await sleep(100);
    assert(discovered === headerCount && first.captured.params.headers['X-Token'] === 'first-token',
      'Headers must be sent with discovery but must not partition the cache or queue another request');
    discoveryGate = null;
    await key('Escape');
  };
  const checkRestDiscoveryRecovery = async () => {
    await click(testid('console-service-rest'));
    const editor = cm('console-editor');
    const status = () => find('.console-status');
    const paste = text => {
      const clipboard = new DataTransfer();
      clipboard.setData('text/plain', text);
      editor.getInputField().dispatchEvent(new ClipboardEvent('paste', {
        clipboardData: clipboard, bubbles: true, cancelable: true,
      }));
    };
    for (const httpError of [false, true]) {
      const badOrigin = `https://rest-recovery-bad-${httpError}.fixture.invalid`;
      const goodOrigin = `https://rest-recovery-good-${httpError}.fixture.invalid`;
      setSource(editor, '#check recovery\n#endpoint\nGET ');
      const failed = discoveryGate = gateFor('rest', `${badOrigin}/api/`, 'Cannot reach mistyped host', httpError);
      const beforePaste = discovered;
      paste(failed.endpoint);
      await sleep(100);
      assert(discovered === beforePaste, 'Pasting a REST URL must not start discovery');
      await commitRestUrl(editor);
      await wait(() => failed.captured);
      failed.release();
      await wait(() => status().classList.contains('is-error') && status().textContent.includes(failed.failure));
      assert(![...document.querySelectorAll('[role="alert"]')].some(visible),
        'An automatic discovery error must stay in status/debug without requiring Dismiss');
      const failedCount = discovered;
      await sleep(700);
      assert(discovered === failedCount, 'A failed URL must not be retried by a timer');
      const failedSource = editor.getValue();
      editor.closeHint();
      editor.replaceRange('\n#body\n', editor.posFromIndex(failedSource.length));
      editor.setCursor(editor.lastLine(), 0);
      const failedStatus = status().textContent;
      for (let index = 0; index < 12; index++) {
        await key('Control+Space');
        await sleep(25);
      }
      await sleep(650);
      assert(discovered === failedCount && status().textContent === failedStatus,
        'Repeated Ctrl-Space in an empty body must not retry a failed discovery or restart its status');
      await key('Escape');
      setSource(editor, failedSource);
      editor.setSelection({line: 2, ch: 4}, {line: 2, ch: 4 + badOrigin.length});
      const recovered = discoveryGate = gateFor('rest', `${goodOrigin}/api/`);
      paste(goodOrigin);
      await sleep(100);
      assert(discovered === failedCount, 'Correcting a REST host by paste must wait for a URL slash or body hints');
      assert(editor.getLine(2) === `GET ${goodOrigin}/api/`
        && editor.getCursor().ch === 4 + goodOrigin.length,
      'Correcting the host must preserve the path and leave the caret before its existing slash');
      await commitRestUrl(editor);
      await wait(() => recovered.captured, 'A URL slash must discover the corrected REST host');
      recovered.release();
      await wait(() => status().textContent.includes('Discovery · OK') && candidates().includes('/api/pets'),
        'A successful corrected-host discovery must automatically reopen API paths before the existing slash');
      assert(discovered === failedCount + 1 && ![...document.querySelectorAll('[role="alert"]')].some(visible),
        'The corrected connection must be discovered once without a stale error notification');
      await key('Enter');
      assert(editor.getLine(2) === `GET ${goodOrigin}/api/pets`,
        'Automatic recovery completion must replace only the path, not the corrected host');
      const loaded = discovered;
      await type('/');
      await type('/');
      await sleep(700);
      assert(discovered === loaded, 'Repeated path slashes must reuse the full loaded API schema without network');
      setSource(editor, `${editor.getValue()}\n#body\n`);
      await type('/');
      await sleep(700);
      assert(discovered === loaded, 'A body slash must not trigger discovery');
      discoveryGate = null;
    }
    setSource(editor, '#check slash-commit\n#endpoint\nGET ');
    const pending = discoveryGate = gateFor('rest', 'https://rest-slash.fixture.invalid/');
    const before = discovered;
    await type('https://');
    await sleep(600);
    await type('rest-slash.fixture.invalid');
    await sleep(600);
    assert(discovered === before, 'Typing a scheme or host without a path slash must not discover REST');
    await type('/');
    await wait(() => pending.captured, 'Entering / after the REST authority must commit discovery');
    pending.release();
    await wait(() => status().textContent.includes('Discovery · OK'));
    await type('api/');
    await sleep(700);
    assert(discovered === before + 1, 'Entering / inside the discovered API must not refetch its schema');
    await key('Escape');
    discoveryGate = null;
  };
  const checkRestDiscoveryPolicy = async () => {
    await click(testid('console-service-rest'));
    const editor = cm('console-editor');
    const origin = 'https://rest-prefix.fixture.invalid';
    setSource(editor, `#check prefixes\n#endpoint\nPOST ${origin}`);
    const initial = discovered;
    await type('/');
    await wait(() => find('.console-status').classList.contains('is-warning'));
    assert(discovered === initial + 1, 'The authority slash must discover the root prefix once');
    await type('site1/');
    await wait(() => candidates().includes('/site1/rest/v1/product'));
    assert(discovered === initial + 2, 'A missing root schema must not block discovery at the next URL prefix');
    const source = site => `#check ${site}\n#endpoint\nPOST ${origin}/${site}/rest/v1/product\n#headers\nX-Token: value\n#body\n`;
    setSource(editor, source('site1'));
    await key('Control+Space');
    await wait(() => candidates().some(candidate => candidate.includes('"site1"')));
    assert(discovered === initial + 2, 'Body hints must reuse the schema found at the site1 prefix');
    setSource(editor, '');
    const clipboard = new DataTransfer();
    clipboard.setData('text/plain', source('site2'));
    editor.getInputField().dispatchEvent(new ClipboardEvent('paste', {
      clipboardData: clipboard, bubbles: true, cancelable: true,
    }));
    await sleep(150);
    assert(discovered === initial + 2, 'Pasting a full URL, headers and body must not discover');
    editor.setCursor(2, editor.getLine(2).length);
    await key('Enter');
    editor.setCursor(editor.lastLine(), 0);
    await click(find('.console-status'));
    editor.focus();
    await sleep(100);
    assert(discovered === initial + 2, 'Enter, cursor departure, blur and focus must not discover REST');
    const pending = discoveryGate = gateFor('rest', `${origin}/site2/rest/v1/product`);
    await key('Control+Space');
    await wait(() => pending.captured);
    for (let index = 0; index < 12; index++) await key('Control+Space');
    assert(discovered === initial + 3, 'Repeated body hint requests must share an in-flight discovery');
    pending.release();
    await wait(() => candidates().some(candidate => candidate.includes('"site2"')));
    discoveryGate = null;
    for (const site of ['site1', 'site2', 'site1']) {
      setSource(editor, source(site).replace('X-Token: value', 'X-Token: changed'));
      await key('Control+Space');
      await wait(() => candidates().some(candidate => candidate.includes(`"${site}"`)));
      assert(!candidates().some(candidate => candidate.includes(`"${site === 'site1' ? 'site2' : 'site1'}"`)),
        'Each mounted URL must receive only its own cached body schema');
    }
    assert(discovered === initial + 3, 'Switching mounted APIs and changing headers must not evict or refetch schemas');
    setSource(editor, '#check root-cache\n#endpoint\nPOST https://rest-root.fixture.invalid');
    await type('/');
    await wait(() => candidates().includes('/site1/rest/v1/product'));
    const rootLoaded = discovered;
    await type('site2/rest/v1/');
    await sleep(100);
    assert(discovered === rootLoaded, 'A root schema with full site paths must cover their later slash prefixes');
    await key('Escape');
  };
  const checkDiscoveryStatus = async () => {
    const status = () => find('.console-status');
    const operationBlock = kind => status().querySelector('.console-status-text').textContent.startsWith(kind) ? status() : null;
    const statusText = () => {
      const labels = [...status().querySelectorAll('.console-status-operation-text')];
      return labels.length ? labels.map(label => label.textContent).join(' | ')
        : status().querySelector('.console-status-text').textContent;
    };
    const liveMilliseconds = (kind = '') => {
      const text = (operationBlock(kind) || status()).querySelector('.console-status-elapsed')?.textContent || '';
      assert(/^\d+ ms$/.test(text), `Live elapsed time must be integer milliseconds: ${text}`);
      return Number.parseInt(text, 10);
    };
    const checkLiveStatus = async kind => {
      assert(status().classList.contains('is-running') && statusText().includes(kind),
        `Running status must identify its owner: ${kind}`);
      const block = operationBlock(kind) || status();
      const spinner = block.querySelector('.console-status-spinner');
      const elapsed = block.querySelector('.console-status-elapsed');
      assert(visible(spinner) && visible(elapsed) && block.firstElementChild === spinner,
        'The spinner and live milliseconds must be visible at the left of each operation block');
      const text = block.querySelector('.console-status-operation-text') || block.querySelector('.console-status-text');
      assert(spinner.getBoundingClientRect().right <= elapsed.getBoundingClientRect().left
        && elapsed.getBoundingClientRect().right <= text.getBoundingClientRect().left,
      'The running line must lay out spinner, milliseconds and request identity from left to right');
      const reducedMotion = matchMedia('(prefers-reduced-motion: reduce)').matches;
      const animation = spinner.getAnimations()[0];
      assert(reducedMotion || animation?.playState === 'running',
        'The loading indicator must use an actual running animation');
      const before = liveMilliseconds(kind);
      const animationTime = animation?.currentTime;
      await sleep(220);
      assert(liveMilliseconds(kind) >= before + 100,
        'Elapsed milliseconds must advance while the same network request is still blocked');
      assert(reducedMotion || animation.currentTime > animationTime,
        'The spinner animation must advance rather than remain a static icon');
    };
    const assertStopped = () => assert(!status().querySelector('.console-status-spinner')
      && !status().querySelector('.console-status-elapsed'),
    'Finished status must remove its spinner and stop the live counter');
    const setEndpoint = (service, endpoint) => setSource(cm('console-editor'),
      `#check status-${service}\n#endpoint\n${service === 'rest' ? 'GET ' : ''}${endpoint}${service === 'rest' ? '\n#body\n' : ''}`);
    const pending = () => wait(() => !!operationBlock('Discovery')
      && status().classList.contains('is-running'),
    'Discovery must immediately replace the normal status while its network response is blocked');
    const finished = (outcome = 'OK') => wait(() => status().textContent.includes(`Discovery · ${outcome}`)
      && status().classList.contains(outcome === 'OK' ? 'is-ok' : 'is-error'),
    () => `Discovery completion must retain its identity and outcome: ${status().textContent}`);
    for (const [service, endpoint] of Object.entries({
      mysql: 'mysql://status.fixture.invalid/database',
      rest: 'https://status.fixture.invalid/rest-schema',
      graphql: 'https://status.fixture.invalid/graphql-schema',
      grpc: 'grpc://status.fixture.invalid:9000',
    })) {
      await click(testid(`console-service-${service}`));
      setEndpoint(service, endpoint);
      const source = cm('console-editor').getValue();
      const gate = discoveryGate = gateFor(service, endpoint);
      await key(service === 'rest' ? 'Control+Space' : 'Alt+Space');
      await wait(() => gate.captured);
      await pending();
      await checkLiveStatus('Discovery');
      await click(status());
      await wait(() => visible(find('.console-debug-panel')) && find('.console-debug-panel').textContent.includes('discover'));
      await click(status());
      if (service === 'mysql') {
        const beforeSwitch = liveMilliseconds();
        await click(testid('console-service-terminal'));
        assert(!status().textContent.includes('Discovery'), 'Discovery status must not leak between Console services');
        assertStopped();
        await click(testid('console-service-mysql'));
        await pending();
        assert(liveMilliseconds() >= beforeSwitch,
          'Returning to a pending operation must retain its real start time, not restart its counter');
      }
      gate.release();
      await finished();
      assertStopped();
      const completed = status().textContent;
      await sleep(220);
      assert(status().textContent === completed, 'Completed discovery status must not keep ticking');
      assert(/\d+(?:\.\d+)?(?:ms|s|min)\b/.test(statusText()),
        'A finished discovery must include elapsed time');
      assert(cm('console-editor').getValue() === source,
        'Discovery must not write an ordinary request response into the Console');
      discoveryGate = null;
    }
    await click(testid('console-service-rest'));
    const endpoint = 'https://status.fixture.invalid/automatic/';
    setEndpoint('rest', endpoint);
    discoveryGate = gateFor('rest', endpoint);
    await commitRestUrl(cm('console-editor'));
    await wait(() => discoveryGate.captured, 'Typing a URL slash must start automatic discovery');
    await pending();
    discoveryGate.release();
    await finished();
    discoveryGate = null;
    await key('Escape');
    for (const httpError of [false, true]) {
      const endpoint = `https://status.fixture.invalid/discovery-error-${httpError}`;
      setEndpoint('rest', endpoint);
      const gate = discoveryGate = gateFor('rest', endpoint,
        httpError ? 'Discovery HTTP failure' : 'Discovery result failure', httpError);
      await key('Control+Space');
      await wait(() => gate.captured);
      await pending();
      gate.release();
      await finished('ERROR');
      assertStopped();
      assert(status().textContent.includes(gate.failure), 'Discovery errors must show their cause in the status');
      assert(![...document.querySelectorAll('[role="alert"]')].some(visible),
        'Manual discovery errors must not be duplicated in a dismissible banner');
      await click(status());
      await wait(() => visible(find('.console-debug-panel'))
        && find('.console-debug-panel').textContent.includes(gate.failure),
      'The complete discovery failure must remain available in status debug');
      await click(status());
      cm('console-editor').focus();
      const failedCount = discovered;
      const failedStatus = status().textContent;
      await type(' ');
      await sleep(600);
      assert(discovered === failedCount && status().textContent === failedStatus,
        'Neither result-level nor transport discovery errors may cause automatic repeated requests');
      discoveryGate = null;
      if (httpError) {
        setEndpoint('rest', 'https://fixture.invalid/failure');
        await key('Shift+Enter');
        await wait(() => status().textContent.startsWith('Request · ')
          && status().textContent.includes('ERROR') && status().classList.contains('is-error'),
          () => `Request status after discovery failure: ${JSON.stringify({
            status: status().textContent, source: cm('console-editor').getValue(),
            cursor: cm('console-editor').getCursor(), discovered,
          })}`);
        assert(!status().textContent.includes('Discovery'), 'An ordinary request error must replace the discovery error');
        const source = cm('console-editor').getValue();
        const response = source.slice(source.indexOf('\n#responce ') + 1);
        assert(/^#responce [^\n]+ · ERROR\n\s*$/.test(response),
          'A failed connection must leave only time and ERROR in the response header, with no fabricated server body');
        assert(status().textContent.includes('UI error fixture'),
          'The connection failure must remain visible in the status line instead of the server response');
        setEndpoint('rest', 'https://fixture.invalid/server-failure');
        await key('Shift+Enter');
        await wait(() => cm('console-editor').getValue().includes('Server unavailable'));
        const serverResponse = cm('console-editor').getValue().split('\n#responce ')[1];
        const [header, ...body] = serverResponse.split('\n');
        assert(header.endsWith(' · ERROR') && !header.includes('Server unavailable')
          && JSON.parse(body.join('\n')).error === 'Server unavailable',
          'An actual HTTP error response must retain its server-provided body and a compact ERROR header');
      }
    }
    const overlapEndpoint = 'https://status.fixture.invalid/request-first';
    setEndpoint('rest', overlapEndpoint);
    requestGate = gateFor('rest', overlapEndpoint);
    await key('Shift+Enter');
    await wait(() => requestGate.captured && statusText().startsWith('Request · Running'));
    await checkLiveStatus('Request');
    discoveryGate = gateFor('rest', overlapEndpoint);
    await key('Control+Space');
    await wait(() => discoveryGate.captured);
    assert(operationBlock('Request') && status().classList.contains('is-running'),
      'Background schema discovery must not replace an executing check request or merge another clock into its status');
    requestGate.release();
    await wait(() => cm('console-editor').getValue().includes('#responce'));
    assert(statusText() === 'Discovery…', 'An older ordinary reply must not overwrite newer discovery progress');
    await checkLiveStatus('Discovery');
    discoveryGate.release();
    await finished();
    requestGate = null;
    discoveryGate = null;
    const newerEndpoint = 'https://status.fixture.invalid/discovery-first';
    setEndpoint('rest', newerEndpoint);
    const oldDiscovery = discoveryGate = gateFor('rest', newerEndpoint);
    await key('Control+Space');
    await wait(() => oldDiscovery.captured);
    await pending();
    requestGate = gateFor('rest', newerEndpoint);
    await key('Shift+Enter');
    await wait(() => requestGate.captured && statusText().startsWith('Request · Running'));
    await checkLiveStatus('Request');
    oldDiscovery.release();
    await wait(() => oldDiscovery.returned);
    await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    assert(statusText().startsWith('Request · Running'),
      'An older discovery reply must not overwrite a newer ordinary request');
    requestGate.release();
    await wait(() => status().textContent.startsWith('Request · ') && status().textContent.includes('OK')
      && !status().classList.contains('is-running'));
    assertStopped();
    requestGate = null;
    discoveryGate = null;
    const previous = status().textContent;
    const count = discovered;
    const observed = [];
    const observer = new MutationObserver(() => observed.push(status().textContent));
    observer.observe(status(), { childList: true, characterData: true, subtree: true });
    const editor = cm('console-editor');
    editor.setCursor(2, editor.getLine(2).length);
    await type(' ');
    await sleep(650);
    observer.disconnect();
    assert(discovered === count && status().textContent === previous
      && observed.every(text => text.startsWith('Request · ')),
    'Cached automatic discovery must not send another request or overwrite the ordinary status');
    await key('Escape');
    const firstEndpoint = 'https://owned-first.fixture.invalid/api/';
    const secondEndpoint = 'https://owned-second.fixture.invalid/api/';
    setSource(editor, `#check discovery-first\n#endpoint\nGET ${firstEndpoint}\n\n#check discovery-second\n#endpoint\nGET ${secondEndpoint}\n\n#check discovery-idle\n#endpoint\nGET ${fixtureUrl}/idle`);
    const selectCheck = async index => {
      const links = () => [...testid('console-check-menu').querySelectorAll('button')];
      await click(links()[index]);
      const line = Number(links()[index].dataset.line) - 1;
      editor.focus();
      editor.setCursor(line + 2, editor.getLine(line + 2).length);
      editor.scrollTo(null, editor.heightAtLine(line, 'local'));
      await wait(() => links()[index].classList.contains('active'));
    };
    await wait(() => testid('console-check-menu').querySelectorAll('button').length === 3);
    await selectCheck(0);
    const firstOwned = discoveryGate = gateFor('rest', firstEndpoint);
    await commitRestUrl(editor, editor.getCursor().line);
    await wait(() => firstOwned.captured,
      () => `First owned discovery did not reach its endpoint: ${JSON.stringify({status:statusText(),cursor:editor.getCursor(),source:editor.getValue(),discovered})}`);
    await selectCheck(1);
    const secondOwned = discoveryGate = gateFor('rest', secondEndpoint);
    await commitRestUrl(editor, editor.getCursor().line);
    await selectCheck(2);
    assert(status().classList.contains('is-ready'), 'A third check must not inherit another check discovery status');
    firstOwned.release();
    await wait(() => secondOwned.captured);
    assert(status().classList.contains('is-ready'),
      'Queued discovery must retain its original check owner, not the check selected when dispatch becomes possible');
    await selectCheck(1);
    await pending();
    const beforeHide = discovered;
    await selectCheck(2);
    await commitRestUrl(editor, editor.getCursor().line);
    await sleep(50);
    assert(discovered === beforeHide, 'A queued third discovery must wait for the current request');
    window.dispatchEvent(new Event('services-console-hidden'));
    window.dispatchEvent(new Event('services-console-shown'));
    secondOwned.release();
    await wait(() => secondOwned.returned);
    await sleep(150);
    assert(discovered === beforeHide && status().classList.contains('is-ready'),
      'Hiding then immediately showing the panel must discard queued discovery instead of starting it on a late reply');
    await selectCheck(1);
    await finished();
    await selectCheck(0);
    assert(status().textContent.includes('Discovery · OK'),
      'Returning to an earlier check must restore its completed discovery status');
    discoveryGate = null;
    await key('Escape');
  };
  for (const service of ['rest', 'mysql', 'graphql', 'grpc', 'splunk', 'terminal', 'console', 'vars', 'dictionary']) {
    await choose(service);
    const editor = find('.CodeMirror');
    assert(editor.CodeMirror.charCoords({ line: 0, ch: 0 }, 'local').left >= 0, `${service}: editor gutter`);
    if (service === 'console') {
      assert(getComputedStyle(find('.console-toolbar')).flexDirection === 'column', 'Console tools form a vertical menu');
    } else if (service === 'vars' || service === 'dictionary') {
      assert(editor.getBoundingClientRect().height > 600, 'Variables must fill viewport');
      assert(find('.vars-status').getBoundingClientRect().y > 900, 'Variables status at bottom');
    } else {
      assert(testid('run-request').getBoundingClientRect().height >= 24, `${service}: full-height Send target`);
      const response = testid('response-output');
      assert([...response.querySelectorAll('.tab-section-title')].some(el => el.textContent === 'Debug'), `${service}: Debug inside Response`);
      assert(getComputedStyle(find('.tab-method-url-row .CodeMirror')).resize === 'none', `${service}: compact endpoint`);
    }
  }
  await choose('rest');
  const root = find('.service-view');
  let request = root.querySelector('#tab-panels > .tab-panel:not([hidden]) > .tab-section');
  const group = request.querySelector('.tab-group');
  const groupBody = group.querySelector(':scope > .tab-group-body');
  await click(button('Endpoint', group));
  assert(!visible(groupBody), 'Click active Endpoint collapses group');
  await click(button('Params', group));
  assert(visible(groupBody), 'Switching group tab expands it');
  const panels = group.querySelectorAll('.tab-group-panel');
  const panelEditor = panel => panel.querySelector('.CodeMirror').CodeMirror;
  panelEditor(panels[1]).setValue('page: 1');
  await click(button('Headers', group));
  panelEditor(panels[2]).setValue('X-UI-Parity: yes');
  await click(button('Params', group));
  assert(panelEditor(panels[1]).getValue() === 'page: 1', 'Tab text retained');
  await click(button('Endpoint', group));
  cm('field-url-editor').setValue('https://fixture.invalid/ui');
  setSource(cm('field-body-editor'), '{}');
  cm('field-body-editor').setCursor(0, 1);
  await key('Tab');
  assert(cm('field-body-editor').getValue() === '{  }', 'Tab inserts two spaces');
  await choose('graphql');
  await choose('rest');
  request = root.querySelector('#tab-panels > .tab-panel:not([hidden]) > .tab-section');
  assert(cm('field-url-editor').getValue() === 'https://fixture.invalid/ui', 'Service switch retains text');
  await click(request.querySelector(':scope > .tab-section-header'));
  assert(!visible(request.querySelector(':scope > .tab-section-body')), 'Collapse request');
  assert(visible(testid('run-request')), 'Send stays visible when collapsed');
  await click(testid('run-request'));
  await wait(() => testid('response-output').textContent.includes('ui-parity-result'));
  assert(!visible(request.querySelector(':scope > .tab-section-body')), 'Running does not expand Request');
  let response = testid('response-output');
  const tree = response.querySelector('.tab-response-body .json-node');
  let inner = tree.querySelectorAll('.json-node')[1];
  await click(inner.querySelector(':scope > .json-line > .json-toggle'));
  assert(inner.textContent.includes('retained-node'), 'Nested JSON retained');
  await click(tree.querySelector(':scope > .json-line > .json-toggle'));
  assert(!visible(tree.querySelector(':scope > .json-children')), 'JSON collapse');
  await click(tree.querySelector(':scope > .json-line > .json-toggle'));
  inner = tree.querySelectorAll('.json-node')[1];
  assert(visible(inner.querySelector('.cm-string')), 'Nested JSON expand state');
  const responseGroup = response.querySelector('.tab-group');
  await click(button('Headers', responseGroup));
  assert(responseGroup.textContent.includes('x-ui-parity'), 'Response headers');
  await click(button('Headers', responseGroup));
  assert(!visible(responseGroup.querySelector('.tab-group-body')), 'Response group collapse');
  await click(button('Body', responseGroup));
  let debug = response.querySelector('.tab-section');
  await click(debug.querySelector(':scope > .tab-section-header'));
  assert(visible(debug.querySelector(':scope > .tab-section-body')), 'Debug expands');
  await click(request.querySelector(':scope > .tab-section-header'));
  await click(find('#new-tab-btn'));
  cm('field-url-editor').setValue('https://fixture.invalid/second');
  await click(root.querySelectorAll('.tab-button')[0]);
  assert(cm('field-url-editor').getValue() === 'https://fixture.invalid/ui', 'First request retained');
  await click(root.querySelectorAll('.tab-button')[1]);
  assert(cm('field-url-editor').getValue() === 'https://fixture.invalid/second', 'Second request retained');
  await click(root.querySelectorAll('.tab-button')[1].querySelector('.tab-close'));
  assert(root.querySelectorAll('.tab-button').length === 1, 'Close request tab');
  await choose('mysql');
  cm('field-dsn-editor').setValue('mysql://fixture.invalid/database');
  cm('field-sql-editor').setValue('select * from fixture');
  await click(testid('run-request'));
  await wait(() => testid('response-output').querySelectorAll('tbody tr').length === 2);
  const resultSections = testid('response-output').querySelectorAll('.tab-section');
  const result = resultSections[1];
  assert(result.querySelector('tbody tr').children[1].textContent === 'NULL', 'SQL NULL rendering');
  const cell = result.querySelectorAll('tbody tr')[1].children[1];
  await click(cell);
  let detail = testid('response-output').querySelectorAll('.tab-section')[2];
  assert(visible(detail.querySelector('.tab-detail-body')), 'SQL detail opens');
  assert(detail.querySelector('pre').textContent === '{"nested":{"flag":true}}'
    && !detail.querySelector('.json-toggle,.json-tree,.cm-atom'),
  'SQL text cells must stay flat and must not automatically parse JSON-looking content');
  await click(result.querySelectorAll('tbody tr')[1].children[2]);
  detail = testid('response-output').querySelectorAll('.tab-section')[2];
  assert(detail.querySelector('.cm-atom')?.textContent === 'true'
    && detail.querySelector('pre').textContent.includes('\n')
    && !detail.querySelector('.json-toggle,.json-tree'),
  'Explicit JSON cells must have flat highlighted pretty text, never a folding tree');
  assert(result.querySelector('tbody tr').children[2].textContent === 'NULL',
    'Nullable typed JSON columns must preserve SQL NULL display');
  await click(cell);
  await click(result.querySelector(':scope > .tab-section-header'));
  assert(visible(result.querySelector('thead')), 'Collapsed table keeps column headers');
  assert([...result.querySelectorAll('tbody tr')].filter(visible).length === 1, 'Collapsed table keeps selected row');
  await click([...result.querySelectorAll('tbody tr')].find(visible).children[1]);
  detail = testid('response-output').querySelectorAll('.tab-section')[2];
  assert(!visible(detail.querySelector('.tab-detail-body')), 'SQL detail closes');
  assert([...result.querySelectorAll('tbody tr')].find(visible).children[0].textContent === '1', 'Collapsed table defaults to first row');
  await choose('rest');
  request = root.querySelector('#tab-panels > .tab-panel:not([hidden]) > .tab-section');
  response = testid('response-output');
  debug = response.querySelector('.tab-section');
  if (!visible(request.querySelector(':scope > .tab-section-body')))
    await click(request.querySelector(':scope > .tab-section-header'));
  cm('field-url-editor').setValue('https://fixture.invalid/failure');
  await click(testid('run-request'));
  await wait(() => response.querySelector('.tab-response-body')?.textContent === 'UI error fixture');
  assert(visible(debug.querySelector(':scope > .tab-section-body')), 'Errors retain debug');
  await choose('console');
  await checkRestPathInsertion();
  await checkRestDiscoveryOutcomes();
  await checkRestDiscoveryRecovery();
  await checkRestDiscoveryPolicy();
  await checkDiscoveryStatus();
  await checkIncompleteDiscoveryAddresses();
  await checkGraphqlDiscoveryRecovery();
  await click(testid('console-service-grpc'));
  await checkGrpcBodyBootstrap(cm('console-editor'), '#check grpc-body\n#endpoint\n');
  await checkGrpcStagedDiscovery(cm('console-editor'), '#check grpc-staged\n#endpoint\n');
  for (const [service, section] of Object.entries({
    mysql: 'query', rest: 'body', graphql: 'query', grpc: 'message', splunk: 'query', terminal: 'command',
  })) {
    await click(testid(`console-service-${service}`));
    const folding = cm('console-editor');
    setSource(folding, [
      '#check first 😀', '#endpoint', 'https://fixture.invalid', '#headers', 'X-Fold: kept',
      `#${section}`, 'first-input', '#response OK', 'first-output',
      '#responce NEXT', 'second-output', '#checkSuffix literal',
      '  #CHECK second', `#${section}`, 'second-input',
      '#request legacy-third', `#${section}`, 'third-input', '#response FINAL', 'output-tail',
      '#check empty', '#check last', `#${section}`, 'last-input',
    ].join('\n'));
    await testDriver.checkConsoleFolding(folding);
  }
  await click(button('Hints…'));
  assert(visible(testid('console-hints-editor')), 'Hints open in the right panel');
  await click(button('Hints…'));
  assert(!visible(document.querySelector('[data-testid="console-tool-panel"]')), 'Hints toggle closes the panel');
  await click(find('.console-status'));
  assert(visible(find('.console-debug-panel')), 'Console debug open');
  await choose('vars');
  await click(button(/^env /));
  assert(cm('variables-editor').getOption('readOnly') === true, 'Read-only environment');
  await choose('console');
  const saved = {
    rest: '#endpoint\nhttps://fixture.invalid\n#headers\nX-Hint-Audit: persisted\n',
    splunk: '#endpoint\nhttps://splunk.fixture.invalid\n#headers\nAuthorization: fixture-http\nindex=http\n',
  };
  for (const [service, text] of Object.entries(saved)) {
    await click(testid(`console-service-${service}`));
    const globalHints=(await(await fetchReal(`/api/console/hints/${service}`)).json()).text;
    await click(button('Hints…'));
    const panel = testid('console-tool-panel');
    cm('console-hints').setValue(text);
    assert(![...panel.querySelectorAll('button')].some(el => ['Save','Close'].includes(el.textContent)),
      'Hints must not require Save or Close buttons');
    await wait(async()=>(await testDriver.sessionLibrary()).sessions
      .find(session=>session.id===Number(testid('console-session-name').dataset.sessionId)).snapshot.hints[service]===text,
      'Hints autosave must persist in the selected named session');
    assert((await(await fetchReal(`/api/console/hints/${service}`)).json()).text===globalHints,
      'Session hints must not replace the original native hints file');
    await click(button('Hints…'));
    await click(button('Hints…'));
    assert(cm('console-hints').getValue() === text, 'Reopening hints must restore saved session text');
    await click(button('Hints…'));
  }
  await click(testid('console-service-rest'));
  let editor = cm('console-editor');
  setSource(editor, '');
  await key('Control+Space');
  await wait(() => candidates().length === 1 && candidates()[0] === '#check');
  await pick(editor, '#check');
  assert(editor.getValue() === '#check ', 'Request completion inserts space');
  setSource(editor, '#check\n#');
  await pick(editor, '#endpoint');
  assert(editor.getValue() === '#check\n#endpoint\n' && editor.getCursor().line === 2, 'Section completion inserts newline');
  await wait(() => candidates().includes('GET '));
  await pick(editor, 'GET ');
  await wait(() => candidates().includes('https://'));
  await pickThenEnter(editor, '#check\n#endpoint\nGET https://fixture.invalid/\n#headers\nX-Hi', 'X-Hint-Audit: persisted');
  const discover = async () => {
    const initial = discovered;
    await key('Alt+Space');
    await wait(() => discovered > initial);
    await sleep(50);
  };
  await click(testid('console-service-graphql'));
  editor = cm('console-editor');
  setSource(editor, '#check\n#endpoint\nhttps://hints.fixture.invalid/graphql\n#query\n{\n  viewer {\n    ad');
  await discover();
  await checkConsoleSeparatorBlocks(editor);
  await checkJsonVariableSnippets(editor, '#check graphql-json-snippets\n#variables\n');
  setSource(editor, '#check graphql-json-only\n#endpoint\nhttps://hints.fixture.invalid/graphql\n#variables\n');
  await key('Control+Space');
  await wait(() => candidates().length === 1 && candidates()[0] === '{\n  \n}',
    'Console #variables must offer only a JSON template, not GraphQL operations or fields');
  await pick(editor, '{\n  \n}');
  await type('pro');
  await key('Control+Space');
  await wait(() => candidates().length === 0,
    'Console #variables must never fall back to discovered GraphQL fields');
  setSource(editor, '#check\n#endpoint\nhttps://hints.fixture.invalid/graphql\n#query\n{\n  viewer {\n    ad');
  await pick(editor, 'address');
  assert(editor.getValue().endsWith('    address') && candidates().length === 0,
    'Choosing a field must insert only its name and wait for an explicit space');
  await type(' ');
  await wait(() => candidates().includes('{\n      \n    }') && !candidates().includes('()'),
    'Fields without arguments must offer only their selection set');
  await pick(editor, '{\n      \n    }');
  assert(editor.getCursor().ch === 6 && editor.getValue().endsWith('    address {\n      \n    }'), 'GraphQL nested snippet indentation');
  await wait(() => candidates().includes('city'));
  await pick(editor, 'city');
  assert(candidates().length === 0, 'GraphQL scalar closes menu');
  await pickThenEnter(editor, '#check\n#endpoint\nhttps://hints.fixture.invalid/graphql\n#query\n{\n  viewer {\n    na', 'name');
  const graphqlHeader = '#check graphql-context\n#endpoint\nhttps://hints.fixture.invalid/graphql\n#query';
  setSource(editor, graphqlHeader);
  await key('Enter');
  await wait(() => candidates().slice(0, 3).join(',') === 'query,mutation,subscription',
    'GraphQL body must start with operation keywords, before selection fields');
  await pick(editor, 'query');
  await type(' GetUser($id: ');
  await wait(() => candidates().includes('ID') && candidates().includes('Filter'),
    'GraphQL variable types must include discovered scalar and input types');
  assert(!candidates().includes('User') && !candidates().includes('viewer'), 'Variable types must exclude output types');
  await pick(editor, 'ID');
  await type('!) {\n  ');
  await pick(editor, 'user');
  assert(candidates().length === 0, 'Required field arguments must wait for an explicit space');
  await type(' ');
  await wait(() => candidates().length === 1 && candidates()[0] === '()',
    'Required arguments must be offered before a selection set');
  await pick(editor, '()');
  assert(editor.getCursor().ch === 8 && editor.getLine(editor.getCursor().line) === '  user ()',
    'GraphQL arguments must stay inline with the cursor inside parentheses');
  await wait(() => candidates().includes('id: ') && candidates().includes('filter: '));
  assert(!candidates().includes('name') && !candidates().includes(')'), 'Required arguments precede result fields and closing parentheses');
  await pick(editor, 'id: ');
  await wait(() => candidates().includes('$id'));
  await pick(editor, '$id');
  assert(editor.getLine(editor.getCursor().line).trim() === 'user (id: $id)', 'Variable completion must replace its dollar prefix');
  setSource(editor, `${graphqlHeader}\nquery GetUser($id: ID!) {\n  user(\n    id: $id\n    `);
  await key('Control+Space');
  await wait(() => candidates().includes('filter: '));
  assert(!candidates().includes('id: '), 'Used argument names must be excluded');
  await pick(editor, 'filter: ');
  await key('Control+Space');
  const inputSnippet = '{\n      \n    }';
  await wait(() => candidates().includes(inputSnippet));
  const inputIndex = candidates().indexOf(inputSnippet);
  const inputSelected = editor.state.completionActive.widget.selectedHint;
  for (let i = 0; i < Math.abs(inputIndex - inputSelected); i++)
    await key(inputIndex > inputSelected ? 'ArrowDown' : 'ArrowUp');
  CodeMirror.signal(editor, 'cursorActivity', editor);
  await sleep(40);
  assert(candidates()[editor.state.completionActive?.widget?.selectedHint] === inputSnippet,
    'Refreshing GraphQL hints must preserve the selected input object instead of resetting to null');
  await key('Enter');
  await wait(() => candidates().includes('state: ') && candidates().includes('nested: '),
    () => `GraphQL input context: ${JSON.stringify({ source: editor.getValue(), cursor: editor.getCursor(), candidates: candidates() })}`);
  assert(!candidates().includes('name'), 'Input objects must not suggest output fields');
  await pick(editor, 'state: ');
  await wait(() => candidates().includes('OPEN') && candidates().includes('CLOSED'));
  await pick(editor, 'OPEN');
  await key('Enter');
  await pick(editor, 'nested: ');
  await pick(editor, '{\n        \n      }');
  await pick(editor, 'active: ');
  await wait(() => candidates().includes('true') && candidates().includes('false'));
  assert(!candidates().includes('null'), 'Non-null input values must not suggest null');
  await pick(editor, 'true');
  setSource(editor, `${graphqlHeader}\nquery GetUser($tags: [String!]!) { user(id: "x", filter: { tags: $`);
  await key('Control+Space');
  await wait(() => candidates().includes('$tags'));
  setSource(editor, `${graphqlHeader}\nquery GetUser { user(id: "x", filter: { tags: ["one", `);
  await key('Control+Space');
  await wait(() => candidates().includes('""') && candidates().includes(']'));
  assert(!candidates().includes('null'), 'List item completion must preserve inner non-null wrappers');
  for (const [operation, field] of [['mutation', 'updateUser'], ['subscription', 'userChanged']]) {
    setSource(editor, `${graphqlHeader}\n${operation} Watch {\n  `);
    await key('Control+Space');
    await wait(() => candidates().includes(field));
    assert(!candidates().some(text => text.startsWith('viewer')), `${operation} must use its own root`);
  }
  const graphqlVariableHeader = '#check graphql-json-variables\n#endpoint\nhttps://hints.fixture.invalid/graphql\n#variables\n{"skus":["private-fixture"]}\n#query';
  setSource(editor, `${graphqlVariableHeader}\nquery`);
  await type(' ');
  await wait(() => candidates().length === 2);
  assert(candidates()[0] === '{\n  \n}' && candidates()[1] === '()',
    'Operation continuations must offer a selection set before arguments');
  setSource(editor, `${graphqlVariableHeader}\nquery `);
  await pick(editor, '()');
  assert(editor.getLine(editor.getCursor().line) === 'query ()' && editor.getCursor().ch === 7,
    'Operation variable definitions must start inline');
  await wait(() => candidates().includes('$skus: '), 'Declarations must offer keys from this request #variables JSON');
  assert(!candidates().some(text => text.includes('private-fixture')), 'Variable hints must not expose JSON values');
  await type('$s');
  await pick(editor, '$skus: ');
  assert(editor.getLine(editor.getCursor().line) === 'query ($skus: )',
    'Known JSON variable completion must replace the dollar prefix without inserting another one');
  await type('[String]');
  await pick(editor, ')');
  assert(editor.getLine(editor.getCursor().line) === 'query ($skus: [String])',
    'Completing a closing parenthesis must consume its existing match');
  await pick(editor, ' {\n  \n}');
  await pick(editor, 'products');
  assert(editor.getLine(editor.getCursor().line) === '  products' && candidates().length === 0,
    'Choosing products must insert only its name without immediately opening continuations');
  await type(' ');
  await wait(() => candidates().includes('()') && candidates().includes('{\n    \n  }'),
    'Products must offer arguments or a selection set after its name');
  assert(candidates()[0] === '{\n    \n  }' && candidates()[1] === '()',
    'Field continuations must offer a selection set before optional arguments');
  const braceHint = editor.state.completionActive.data.list.find(item => item.text === '{\n    \n  }');
  assert(braceHint.displayText === '{} — Selection set',
    'Selection hints must have compact labels without changing their multiline insertion text');
  const hintRows = [...document.querySelectorAll('.CodeMirror-hint')].filter(visible);
  const braceRow = hintRows.find(row => row.textContent === braceHint.displayText);
  const argumentsRow = hintRows.find(row => row.textContent === '() — Arguments');
  assert(braceRow && argumentsRow && braceRow.getBoundingClientRect().height <= argumentsRow.getBoundingClientRect().height + 1,
    'The brace hint must occupy one row, just like the arguments hint');
  await pick(editor, '()');
  await pick(editor, 'skus: ');
  await pick(editor, '$skus');
  await key('ArrowRight');
  assert(editor.getLine(editor.getCursor().line) === '  products (skus: $skus)',
    'Field arguments must stay on one line without duplicate closing parentheses');
  await pick(editor, ' {\n    \n  }');
  await pick(editor, 'sku');
  await sleep(80);
  assert(candidates().length === 0, 'Selecting a scalar must not immediately offer sibling fields');
  await key('Enter');
  await wait(() => candidates().includes('images'),
    'Enter after a scalar must automatically offer sibling fields');
  await pick(editor, 'images');
  assert(candidates().length === 0, 'Nested field continuations must wait for a space');
  await type(' ');
  await wait(() => candidates().includes('()') && candidates().includes('{\n      \n    }'),
    'Nested fields with optional arguments must also offer arguments or a selection set');
  await pick(editor, '{\n      \n    }');
  await pick(editor, 'url');
  assert(editor.getValue() === `${graphqlVariableHeader}\nquery ($skus: [String]) {\n  products (skus: $skus) {\n    sku\n    images {\n      url\n    }\n  }\n}`,
    'Nested GraphQL template must have inline arguments and balanced selection sets');
  const completedGraphql = editor.getValue();
  editor.setCursor(graphqlVariableHeader.split('\n').length, 'query ($s'.length);
  await pick(editor, '$skus');
  assert(editor.getValue() === completedGraphql,
    'Completing inside an existing variable declaration must preserve the remaining name and type without duplicating its colon');
  setSource(editor, `${graphqlVariableHeader}\nquery ($skus: [String]) {\n  products (skus: $skus)`);
  await type(' ');
  await wait(() => candidates().length === 3 && candidates().some(text => text.startsWith('{'))
    && candidates().includes('@include()') && candidates().includes('@skip()'),
  'A completed argument list must offer a selection set and directives, never another argument list');
  setSource(editor, `${graphqlVariableHeader}\nquery {\n  products { sku }\n}`);
  editor.setCursor(graphqlVariableHeader.split('\n').length + 1, '  products '.length);
  await key('Control+Space');
  await wait(() => candidates().length === 3 && candidates().includes('()')
    && candidates().includes('@include()') && candidates().includes('@skip()'),
  'An existing selection set to the right must leave only arguments and directives, not a duplicate body');
  const mouseTarget = editor.charCoords(editor.getCursor(), 'window');
  const wrapper = editor.getWrapperElement();
  const wrapperBox = wrapper.getBoundingClientRect();
  await click(wrapper, mouseTarget.left - wrapperBox.left, mouseTarget.top - wrapperBox.top + 3);
  await sleep(550);
  assert(candidates().length === 0, 'Clicking the editor must close hints and prevent deferred reopening');
  CodeMirror.signal(editor, 'cursorActivity', editor);
  await sleep(40);
  assert(candidates().length === 0, 'Cursor movement must not open a new completion menu');
  setSource(editor, `${graphqlVariableHeader}\nquery { products { sk`);
  await type('u');
  await sleep(80);
  assert(candidates().length === 0, 'Typing an exact scalar name must not offer sibling fields');
  await type(' ');
  await wait(() => candidates().includes('@include()') && candidates().includes('@skip()'));
  assert(candidates().every(candidate => candidate.startsWith('@')),
    'A space after a scalar must offer directives, not sibling fields');
  await key('Escape');
  await key('Enter');
  await wait(() => candidates().includes('images'), 'Sibling fields must appear after Enter');
  setSource(editor, `${graphqlVariableHeader}\nquery { products {\n  __ty`);
  await pick(editor, '__typename');
  assert(editor.getLine(editor.getCursor().line).trim() === '__typename' && candidates().length === 0,
    '__typename must insert as a plain scalar without arguments or a selection set');
  await key('Enter');
  await wait(() => candidates().includes('... on ') && !candidates().includes('__typename'),
    'Used __typename must be excluded from the same selection set');
  assert(!candidates().includes('...'), 'Generic ellipsis hints must be replaced by explicit inline-fragment hints');
  await pick(editor, '... on ');
  await wait(() => candidates().includes('SimpleProductView') && candidates().includes('ComplexProductView'));
  assert(!candidates().includes('User') && !candidates().includes('Filter'),
    'Fragment conditions must exclude unrelated objects and input types');
  await pick(editor, 'SimpleProductView');
  assert(candidates().length === 0, 'A selected condition must wait for a space before offering its body');
  await type(' ');
  const fragmentIndent = editor.getLine(editor.getCursor().line).match(/^\s*/)[0];
  await pick(editor, `{\n${fragmentIndent}  \n${fragmentIndent}}`);
  await wait(() => candidates().includes('price') && candidates().includes('__typename'));
  assert(!candidates().includes('options'), 'An inline fragment must use the selected subtype fields');
  await pick(editor, 'price');
  await type(' ');
  await pick(editor, `{\n${fragmentIndent}    \n${fragmentIndent}  }`);
  await wait(() => candidates().includes('amount'), 'Nested subtype fields must retain their own selection context');
  setSource(editor, `${graphqlHeader}\nfragment CountryFields on Country {\n  cap`);
  await pick(editor, 'capital');
  assert(editor.getValue().endsWith('  capital'), 'Console fragment bodies must use their declared type fields');
  await checkAliasAndManualHeaderHints(editor, `${graphqlHeader}\n`);
  await checkAdvertisedDirectives(editor, `${graphqlHeader}\n`);
  await checkUsefulTypeConditions(editor, `${graphqlHeader}\n`);
  await checkUndeclaredVariableHints(editor,
    '#check variable-prefix\n#endpoint\nhttps://hints.fixture.invalid/graphql\n#variables\n{"hideCurrency":false}\n#query\n',
    'hideCurrency');
  await checkSingleOperationHints(editor, `${graphqlHeader}\n`);
  await checkMultilineOperationFolding(editor, `${graphqlHeader}\n`);
  setSource(editor, `${graphqlHeader}\n${fragmentColorSource}`);
  await expectFragmentColors(editor);
  setSource(editor, `${graphqlHeader}\nfragment UserFields on User { id }\nfragment CountryFields on Country { code name capital currency }\nquery { country(code: "PL") {\n    `);
  await key('Control+Space');
  await wait(() => candidates().includes('...CountryFields') && candidates().includes('code'),
    'Console field hints must include complete compatible fragment spreads alongside ordinary fields');
  assert(!candidates().includes('...UserFields'),
    'Direct fragment hints must exclude incompatible types');
  await pick(editor, '...CountryFields');
  assert(editor.getLine(editor.getCursor().line).trim() === '...CountryFields'
    && candidates().length === 0, 'A direct fragment hint must insert the complete spread exactly once');
  await key('Enter');
  await wait(() => candidates().includes('capital'));
  assert(!candidates().includes('...CountryFields'),
    'A fragment already used in this selection must not appear as a direct hint again');
  setSource(editor, `${graphqlHeader}\nfragment CountryFields on Country { code name capital currency }\nquery CompareCountries($first: ID!) {\n  country(code: $first) {\n    ...Cou`);
  await pick(editor, 'CountryFields');
  assert(editor.getValue().endsWith('...CountryFields'), 'Console spreads must complete preceding named fragments');
  setSource(editor, `${graphqlHeader}\nquery { products { sku images } }\n#variables\n{"images":{},"sku":"fixture"}`);
  await expectObjectColors(editor, ['products', 'images'], ['sku']);
  const exploreConsolePrefix = `#check explore-countries\n#endpoint\nhttps://countries.explore.fixture.invalid/graphql\n#variables\n${JSON.stringify(testDriver.graphqlExplore.variables, null, 2)}\n#query\n`;
  await checkExploreCountries(editor, exploreConsolePrefix);
  const beforeConsoleExplore = exploreRequests.length;
  await key('Shift+Enter');
  await wait(() => exploreRequests.length > beforeConsoleExplore
    && editor.getValue().includes('Warsaw') && editor.getValue().includes('Berlin'),
  () => `ExploreCountries Console requests: ${exploreRequests.length}; response: ${editor.getValue().slice(-1000)}`);
  assert(objectFieldMarks(editor).length === 6,
    'ExploreCountries response JSON must not acquire GraphQL object field marks');
  await checkBracketHighlighting(editor, `${graphqlHeader}\n`);
  await choose('graphql');
  const graphqlQuery = cm('field-query-editor');
  setSource(graphqlQuery, '');
  await key('Control+Space');
  await wait(() => candidates().slice(0, 3).join(',') === 'query,mutation,subscription',
    'GraphQL form and Console must share operation completion');
  setSource(cm('field-url-editor'), 'https://graphql.fixture.invalid');
  await key('Alt+Space');
  setSource(graphqlQuery, 'query {\n  pro');
  await pick(graphqlQuery, 'products');
  assert(candidates().length === 0, 'The form must also wait for an explicit space after a field');
  await type(' ');
  await wait(() => candidates().includes('()') && candidates().includes('{\n    \n  }'),
    'The separate GraphQL form must offer the same field continuation choices');
  await pick(graphqlQuery, '{\n    \n  }');
  assert(graphqlQuery.getValue() === 'query {\n  products {\n    \n  }',
    'Optional field arguments must be skippable by choosing a selection set directly');
  await checkBracketHighlighting(graphqlQuery);
  await checkAliasAndManualHeaderHints(graphqlQuery);
  await checkAdvertisedDirectives(graphqlQuery);
  await checkUsefulTypeConditions(graphqlQuery);
  await checkSingleOperationHints(graphqlQuery);
  await checkMultilineOperationFolding(graphqlQuery);
  setSource(graphqlQuery, fragmentColorSource);
  await expectFragmentColors(graphqlQuery);
  setSource(graphqlQuery, 'query { products { sku images');
  await expectObjectColors(graphqlQuery, ['products', 'images'], ['sku']);
  const originalProductMark = objectFieldMarks(graphqlQuery)
    .find(mark => graphqlQuery.getRange(mark.find().from, mark.find().to) === 'products');
  graphqlQuery.replaceRange('\n', { line: 0, ch: 0 });
  await expectObjectColors(graphqlQuery, ['products', 'images'], ['sku']);
  assert(objectFieldMarks(graphqlQuery).includes(originalProductMark),
    'Unchanged object fields must retain their existing marks when lines move');
  setSource(graphqlQuery, 'fragment ProductFields on Product { sku images { url } }\nquery { products { ...ProductFields ... on SimpleProductView { price { amount } } } }');
  await expectObjectColors(graphqlQuery, ['images', 'products', 'price'], ['sku', 'url', 'amount']);
  setSource(graphqlQuery, 'query { products(skus: ["unfinished');
  await expectObjectColors(graphqlQuery, ['products']);
  setSource(graphqlQuery, 'query { products { sku images # comment at EOF');
  await expectObjectColors(graphqlQuery, ['products', 'images'], ['sku']);
  const dynamicField = { name: 'dynamicField', type: 'String' };
  schema.graphql.types.Product.fields.push(dynamicField);
  cm('field-url-editor').setValue('https://hints.fixture.invalid/graphql');
  setSource(graphqlQuery, 'query { products { sku dynamicField } }');
  await discover();
  await expectObjectColors(graphqlQuery, ['products'], ['sku', 'dynamicField']);
  const historyBeforeColors = JSON.stringify(graphqlQuery.historySize());
  const sourceBeforeColors = graphqlQuery.getValue();
  dynamicField.type = 'Image';
  await discover();
  await expectObjectColors(graphqlQuery, ['products', 'dynamicField'], ['sku']);
  dynamicField.type = 'String';
  await discover();
  await expectObjectColors(graphqlQuery, ['products'], ['sku', 'dynamicField']);
  assert(graphqlQuery.getValue() === sourceBeforeColors
    && JSON.stringify(graphqlQuery.historySize()) === historyBeforeColors,
  'Refreshing semantic colors must not edit the source or undo history');
  assert(candidates().length === 0, 'Semantic color refresh must not open completion menus');
  const graphqlVariables = cm('field-variables-editor');
  await click(button('Variables'));
  await wait(() => visible(graphqlVariables.getWrapperElement()));
  await checkJsonVariableSnippets(graphqlVariables);
  const jsonBrackets = '{"{key}":"(value)","array":[1,2]}';
  setSource(graphqlVariables, jsonBrackets);
  assert(objectFieldMarks(graphqlVariables).length === 0, 'GraphQL object colors must not leak into Variables JSON');
  for (const at of [jsonBrackets.indexOf('{key'), jsonBrackets.indexOf('(value')]) {
    graphqlVariables.setSelection(graphqlVariables.posFromIndex(at), graphqlVariables.posFromIndex(at + 1));
    await expectBracketMarks(graphqlVariables, []);
  }
  graphqlVariables.setCursor(graphqlVariables.posFromIndex(jsonBrackets.indexOf('[')));
  await expectBracketMarks(graphqlVariables, [jsonBrackets.indexOf('['), jsonBrackets.indexOf(']')]);
  setSource(graphqlVariables, '');
  await key('Control+Space');
  await wait(() => candidates().length === 1 && candidates()[0] === '{\n  \n}',
    () => `GraphQL Variables JSON context: ${JSON.stringify({
      source: graphqlVariables.getValue(), cursor: graphqlVariables.getCursor(),
      service: graphqlVariables.__servicesCompletion?.service, field: graphqlVariables.__servicesCompletion?.field,
      attached: graphqlVariables.getWrapperElement().isConnected, candidates: candidates(),
      local: graphqlVariables.state.completionActive?.data?.list?.map(item => item.text),
    })}`);
  await pick(graphqlVariables, '{\n  \n}');
  await type('pro');
  await key('Control+Space');
  await wait(() => candidates().length === 0,
    'GraphQL Variables must not suggest body operations, fields or saved hints');
  cm('field-variables-editor').setValue('{"formOnly":["private-form-fixture"]}');
  await click(button('Query'));
  await wait(() => visible(graphqlQuery.getWrapperElement()));
  setSource(graphqlQuery, 'query ($');
  await pick(graphqlQuery, '$formOnly: ');
  assert(graphqlQuery.getValue() === 'query ($formOnly: ', 'Form declarations must use its own Variables JSON');
  await click(button('Variables'));
  await wait(() => visible(graphqlVariables.getWrapperElement()));
  setSource(graphqlVariables, '{"updatedName":true}');
  await click(button('Query'));
  await wait(() => visible(graphqlQuery.getWrapperElement()));
  setSource(graphqlQuery, 'query ($');
  await key('Control+Space');
  await wait(() => candidates().includes('$updatedName: '), 'Changing Variables JSON must refresh declaration hints');
  assert(!candidates().includes('$formOnly: ') && !candidates().includes('$skus: '),
    'Variable hints must not retain old form values or leak names from the Console');
  await checkUndeclaredVariableHints(graphqlQuery, '', 'updatedName');
  await click(button('Variables'));
  await wait(() => visible(graphqlVariables.getWrapperElement()));
  setSource(graphqlVariables, '{"first":"PL","second":"DE"}');
  await click(button('Query'));
  await wait(() => visible(graphqlQuery.getWrapperElement()));
  cm('field-url-editor').setValue('https://countries.fixture.invalid/graphql');
  const fragmentHeight = graphqlQuery.getWrapperElement().style.height;
  // Keep this multiline grammar fixture clear of CodeMirror's scroll-to-close popup behavior.
  graphqlQuery.setSize(null, graphqlQuery.defaultTextHeight() * 24);
  setSource(graphqlQuery, '');
  await key('Control+Space');
  await wait(() => candidates().includes('fragment'), 'A new document must offer fragment definitions');
  await pick(graphqlQuery, 'fragment');
  await type(' CountryFields ');
  await wait(() => candidates().includes('on '), 'A named fragment must offer its type-condition keyword');
  await pick(graphqlQuery, 'on ');
  await wait(() => candidates().includes('Country'));
  assert(!candidates().includes('ID') && !candidates().includes('Filter'),
    'Fragment definitions must offer output composite types, not scalars or inputs');
  await pick(graphqlQuery, 'Country');
  await type(' ');
  await pick(graphqlQuery, '{\n  \n}');
  for (const field of ['code', 'name', 'capital', 'currency']) {
    await pick(graphqlQuery, field);
    if (field !== 'currency') await key('Enter');
  }
  graphqlQuery.setCursor(graphqlQuery.lastLine(), graphqlQuery.getLine(graphqlQuery.lastLine()).length);
  await type('\n\n');
  await wait(() => candidates().includes('query') && candidates().includes('fragment'),
    () => `Leading fragments must leave operation and additional-fragment completion available: ${JSON.stringify({
      source: graphqlQuery.getValue(), cursor: graphqlQuery.getCursor(), candidates: candidates(),
      focused: graphqlQuery.hasFocus(), enabled: graphqlQuery.__servicesHintsEnabled,
      local: graphqlQuery.__servicesHints?.list?.map(item => item.text), scroll: graphqlQuery.getScrollInfo(),
    })}`);
  await pick(graphqlQuery, 'query');
  await type(' CompareCountries($first: ID!, $second: ID!) ');
  await pick(graphqlQuery, '{\n  \n}');
  for (const [alias, variable] of [['firstCountry', 'first'], ['secondCountry', 'second']]) {
    await type(`${alias}: country(code: $${variable}) `);
    await pick(graphqlQuery, '{\n    \n  }');
    await key('Control+Space');
    await wait(() => candidates().includes('...CountryFields'),
      'Field hints must directly offer the compatible fragment declared before this query');
    await pick(graphqlQuery, '...CountryFields');
    assert(graphqlQuery.getLine(graphqlQuery.getCursor().line).trim() === '...CountryFields'
      && candidates().length === 0, 'A named spread must insert only its name and close the menu');
    if (alias === 'firstCountry') {
      const closing = graphqlQuery.getCursor().line + 1;
      graphqlQuery.setCursor(closing, graphqlQuery.getLine(closing).length);
      await key('Enter');
    }
  }
  await click(testid('run-request'));
  await wait(() => countryRequest && testid('response-output').textContent.includes('firstCountry')
    && testid('response-output').textContent.includes('secondCountry'),
  () => `Countries fragment request: ${JSON.stringify(countryRequest)}, response: ${testid('response-output').textContent}`);
  for (const alias of ['firstCountry', 'secondCountry']) {
    const property = [...testid('response-output').querySelectorAll('.tab-response-body .cm-property')]
      .find(el => el.textContent === `"${alias}": `);
    assert(property, `Country response field missing: ${alias}`);
    const node = property.closest('.json-node');
    if (!visible(node.querySelector(':scope > .json-children')))
      await click(node.querySelector(':scope > .json-line > .json-toggle'));
  }
  await wait(() => testid('response-output').textContent.includes('Warsaw')
    && testid('response-output').textContent.includes('Berlin'), 'Both country fragments must render their returned fields');
  graphqlQuery.setSize(null, fragmentHeight);
  await click(button('Variables'));
  await wait(() => visible(graphqlVariables.getWrapperElement()));
  setSource(graphqlVariables, JSON.stringify(testDriver.graphqlExplore.variables, null, 2));
  await click(button('Query'));
  await wait(() => visible(graphqlQuery.getWrapperElement()));
  cm('field-url-editor').setValue('https://countries.explore.fixture.invalid/graphql');
  await checkExploreCountries(graphqlQuery);
  const beforeFormExplore = exploreRequests.length;
  await click(testid('run-request'));
  await wait(() => exploreRequests.length > beforeFormExplore
    && testid('response-output').textContent.includes('matchingCountries'),
  'The GraphQL form must submit the exact ExploreCountries reference and Variables');
  await choose('grpc');
  const grpc = cm('field-endpointText-editor');
  setSource(grpc, 'grpc://hints.fixture.invalid:50051/demo.Greeter/Hel');
  await discover();
  setSource(grpc, 'grpc://hints.fixture.invalid:50051/de');
  await pick(grpc, 'demo.Greeter/');
  await wait(() => candidates().includes('Hello'));
  await pickThenEnter(grpc, 'grpc://hints.fixture.invalid:50051/demo.Greeter/Hel', 'Hello');
  await checkGrpcBodyBootstrap(cm('field-requestMessage-editor'));
  await checkGrpcStagedDiscovery(grpc);
  await choose('mysql');
  cm('field-dsn-editor').setValue('mysql://hints.fixture.invalid/database');
  const sql = cm('field-sql-editor');
  setSource(sql, 'SELECT * FROM users u WHERE u.em');
  await discover();
  await pickThenEnter(sql, 'SELECT * FROM users u WHERE u.em', 'email');
  await choose('console');
  await click(testid('console-service-mysql'));
  await wait(() => cm('console-editor').getOption('mode').variant === 'mysql',
    'Console must mount the selected MySQL editor before typing');
  editor = cm('console-editor');
  setSource(editor, '#check sql-context\n#endpoint\nmysql://hints.fixture.invalid/database\n#query');
  await key('Enter');
  await wait(() => candidates().includes('SELECT'), 'New #query line must suggest SQL commands');
  assert(candidates().every(text => !text.startsWith('field_') && text !== 'email'),
    'Discovered columns must not bury SQL commands');
  await discover();
  await pick(editor, 'SELECT');
  await type(' ');
  await wait(() => candidates().includes('*'));
  assert(!candidates().includes('users') && !candidates().includes('FROM'), 'SELECT expects expressions, not tables or FROM');
  await pick(editor, '*');
  await type(' ');
  await wait(() => candidates().length === 1 && candidates()[0] === 'FROM', 'SELECT * expects only FROM');
  await pick(editor, 'FROM');
  await type(' ');
  await wait(() => candidates().includes('users') && candidates().includes('orders'));
  assert(candidates().every(text => ['users', 'orders'].includes(text)), 'FROM expects table names only');
  await pick(editor, 'users');
  await type(' u ');
  await wait(() => candidates().includes('LEFT JOIN') && candidates().includes('WHERE'));
  assert(!candidates().includes('email') && !candidates().includes('orders'), 'After table expects clauses');
  await pick(editor, 'LEFT JOIN');
  await type(' ');
  await pick(editor, 'orders');
  await type(' o ');
  await pick(editor, 'ON');
  await type(' ');
  await wait(() => candidates().includes('u.email') && candidates().includes('o.user_id'));
  await type('u.id = o.user_id;');
  await wait(() => candidates().length === 0, 'Semicolon must close SQL hints');
  await key('Control+Space');
  assert(candidates().length === 0, 'Explicit completion after semicolon stays quiet');
  await key('Enter');
  assert(candidates().length === 0, 'Newline after completed SQL stays quiet');
  await type('S');
  await wait(() => candidates().includes('SELECT'));
  assert(candidates().every(text => ['SELECT', 'SHOW'].includes(text)), 'Uppercase starts a new language construct');
  setSource(editor, '#check uppercase\n#endpoint\nmysql://hints.fixture.invalid/database\n#query\nSELECT S');
  await key('Control+Space');
  await wait(() => candidates().includes('SUM('));
  assert(!candidates().includes('status'), 'Uppercase SQL intent must suppress matching field names');
  await choose('splunk');
  setSource(cm('field-baseUrl-editor'), 'http');
  await key('Control+Space');
  await wait(() => candidates().includes('https://splunk.fixture.invalid'));
  assert(candidates().every(text => text.includes('://')), 'Splunk endpoint must exclude headers and SPL');
  await testDriver.checkConsoleTools('https://fixture.invalid/panel-tools');
  window.fetch = fetchReal;
  return true;
})()
