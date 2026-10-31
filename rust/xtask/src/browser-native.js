(async () => {
  const { assert, wait, during, holds, settle, visible, find, testid, cm, key, type, move, click, button,
    openConsole, setSource, commitRestUrl, candidates, pick } = testDriver;
  await wait(() => document.querySelector('[data-testid="native-status"]')?.hidden,
    'Installed companion must connect');
  await openConsole();
  const services = ['mysql', 'rest', 'graphql', 'grpc', 'splunk', 'terminal', 'stomp'];
  for (const service of services) {
    await click(testid(`console-service-${service}`));
    await click(testid('console-editor-editor'), 80, 16);
    const editor = cm('console-editor');
    assert(editor.hasFocus(), `${service}: mouse click must focus empty Console`);
    await during(100, () => assert(candidates().length === 0, `${service}: mouse click must not open suggestions`));
    await key('Control+Space');
    await wait(() => candidates().includes('#check'));
    await key('Enter');
    await type('browser-fixture');
    await key('Enter');
    await wait(() => candidates()[0] === '#endpoint');
    assert(editor.getValue() === '#check browser-fixture\n', `${service}: Enter must retain request name`);
    await key('Enter');
    assert(editor.getValue() === '#check browser-fixture\n#endpoint\n', `${service}: section insertion`);
    await key('Escape');
  }
  const endpoints = {
    mysql: 'mysql://mysql.fixture.invalid', rest: 'https://rest.fixture.invalid',
    graphql: 'https://graphql.fixture.invalid', grpc: 'grpc://grpc.fixture.invalid:9000',
    splunk: 'https://splunk.fixture.invalid', stomp: 'stomp://stomp.fixture.invalid:61613/orders',
  };
  const methods = {rest: 'GET ', stomp: 'SEND '};
  for (const service of services) {
    await click(testid(`console-service-${service}`));
    const editor = cm('console-editor');
    setSource(editor, `#check scope\n#endpoint\n${methods[service] || ''}`);
    await key('Control+Space');
    await wait(() => candidates().some(text => text.includes(endpoints[service] || 'bash -c')),
      () => `${service}: saved endpoint missing from ${JSON.stringify(candidates())}`);
    for (const [other, endpoint] of Object.entries(endpoints)) {
      assert(other === service || !candidates().some(text => text.includes(endpoint)), `${service}: hints leaked from ${other}`);
    }
    await key('Escape');
    editor.replaceRange('{{', editor.getCursor());
    await key('Control+Space');
    await wait(() => candidates().includes('{{env:'));
    assert(candidates().every(text => text.startsWith('{{')), `${service}: placeholder completion isolation`);
    await key('Escape');
  }
  // Runs one Console check and returns the text written below its #responce marker.
  const runConsole = async (service, source, done, message) => {
    await click(testid(`console-service-${service}`));
    const editor = cm('console-editor');
    setSource(editor, source);
    await key('Shift+Enter');
    const output = () => {
      const value = editor.getValue();
      const marker = value.indexOf('\n#responce ');
      return marker < 0 ? '' : value.slice(value.indexOf('\n', marker + 1) + 1);
    };
    await wait(() => !find('.console-status').classList.contains('is-running') && done(output()),
      () => `${message}: ${JSON.stringify({status: find('.console-status').textContent, output: output().slice(0, 2000)})}`);
    return output();
  };
  const envOutput = await runConsole('rest',
    `#check env\n#endpoint\nGET ${fixtureUrl}/browser?env={{env:SERVICES_DEVTOOLS_ENV_PROBE}}&interactive={{env:SERVICES_DEVTOOLS_INTERACTIVE_PROBE}}`,
    output => output.includes('rust-ui-fixture'), 'The REST fixture must answer through the companion');
  assert(envOutput.includes('/browser?env=companion-fixture-only'), 'Login shell env substitution');
  assert(envOutput.includes('interactive=interactive-fixture-only'), 'Interactive shell env substitution');
  const beforeChunks = nativeProbe.chunkMessages;
  const expectedLarge = [...`native-large-start:${'🦀"\\\n'.repeat(180000)}:native-large-end`];
  // The Console shows the first MAX_RESPONSE_CHARS characters and counts the rest, so the
  // exact prefix plus the exact omitted count prove that no byte was lost in transit.
  const shownLarge = `${expectedLarge.slice(0, 300000).join('')}\n\n… response truncated (${expectedLarge.length - 300000} more characters omitted)`;
  const largeOutput = await runConsole('rest', `#check large\n#endpoint\nGET ${fixtureUrl}/native-large-response`,
    output => output.includes('response truncated ('),
    'A response larger than 1 MiB must survive real Chrome native messaging');
  assert(largeOutput.startsWith(shownLarge),
    'A response larger than 1 MiB must survive real Chrome native messaging without losing bytes');
  assert(nativeProbe.chunkMessages > beforeChunks, 'The large native response must use chunking');
  assert(nativeProbe.maxMessageBytes <= 1024 * 1024, 'Every serialized native reply must fit Chrome\'s 1 MiB limit');
  assert(document.querySelector('[data-testid="native-status"]').hidden,
    'The large response must not disconnect the companion');
  await runConsole('terminal',
    '#check env\n#endpoint\n/bin/sh -c\n#command\nprintf rust-ui-terminal; printf "$SERVICES_DEVTOOLS_INTERACTIVE_PROBE"',
    output => output.includes('rust-ui-terminal') && output.includes('interactive-fixture-only'),
    'Terminal commands must run with the interactive shell environment');
  await runConsole('terminal',
    "#check failure\n#endpoint\n/bin/sh -c\n#command\nprintf partial; printf 'first\\nfixture-failure: boom\\n' >&2; exit 3",
    output => output.includes('fixture-failure: boom'),
    'A failed Terminal command must show its stderr');
  const failedStatus = testid('console-main-status').textContent;
  assert(failedStatus.includes('ERROR · exit 3 · fixture-failure: boom'),
    `A failed Terminal command must be an ERROR showing its exit code and last stderr line in the status: ${failedStatus}`);
  await click(testid('console-main-status'));
  assert(testid('console-main-status-debug').textContent.includes('fixture-failure: boom'),
    'Terminal Debug must keep the stderr of a failed command');
  await click(testid('console-main-status'));
  await click(testid('console-service-rest'));
  const bookEditor = cm('console-editor');
  const pasteRest = text => {
    const clipboard = new DataTransfer();
    clipboard.setData('text/plain', text);
    bookEditor.getInputField().dispatchEvent(new ClipboardEvent('paste', {
      clipboardData: clipboard, bubbles: true, cancelable: true,
    }));
  };
  const restDiscoveryCount = () => nativeProbe.discoveries.filter(request => request.service === 'rest').length;
  for (const site of ['site1', 'site2']) {
    const source = `#check ${site}\n#endpoint\nPOST ${fixtureUrl}/${site}/rest/v1/product\n#headers\nX-Token: first\n#body\n`;
    setSource(bookEditor, '');
    const before = restDiscoveryCount();
    pasteRest(source);
    await during(150, () => assert(restDiscoveryCount() === before, 'Pasting URL, headers and body must not start REST discovery'));
    await click(find('.console-status'));
    bookEditor.focus();
    await during(100, () => assert(restDiscoveryCount() === before, 'Focusing an uncached REST body must not start discovery'));
    await key('Control+Space');
    await wait(() => bookEditor.__servicesCompletion?.discovery?.specUrl === `${fixtureUrl}/${site}/swagger.json`);
    await wait(() => candidates().some(candidate => candidate.includes(`"${site}"`)),
      'The first body hint must discover the mounted API and reopen its required template');
    assert(restDiscoveryCount() === before + 1, 'The first body hint must start exactly one discovery');
  }
  const sitesLoaded = restDiscoveryCount();
  for (const site of ['site1', 'site2', 'site1']) {
    setSource(bookEditor, `#check cached\n#endpoint\nPOST ${fixtureUrl}/${site}/rest/v1/product\n#headers\nX-Token: changed\n#body\n`);
    await key('Control+Space');
    await wait(() => candidates().some(candidate => candidate.includes(`"${site}"`)));
    assert(!candidates().some(candidate => candidate.includes(`"${site === 'site1' ? 'site2' : 'site1'}"`)),
      'Cached mounted APIs must not exchange body fields');
  }
  assert(restDiscoveryCount() === sitesLoaded, 'Switching mounted APIs or changing headers must reuse their separate URL caches');
  const badOrigin = fixtureUrl.replace(/:\d+$/, ':0');
  assert(badOrigin !== fixtureUrl, 'The native recovery fixture must use a refused local port');
  setSource(bookEditor, '#check recovery\n#endpoint\nGET ');
  const beforePaste = restDiscoveryCount();
  pasteRest(`${badOrigin}/api/`);
  await during(100, () => assert(restDiscoveryCount() === beforePaste, 'Pasting an unreachable REST URL must not discover it'));
  await commitRestUrl(bookEditor);
  await wait(() => find('.console-status').textContent.includes('Discovery · ERROR'),
    'An explicit URL slash must report a real native discovery failure');
  assert(![...document.querySelectorAll('[role="alert"]')].some(visible),
    `Native automatic discovery failure must not display a dismissible banner: ${JSON.stringify({
      alerts: [...document.querySelectorAll('[role="alert"]')].filter(visible).map(element => element.textContent),
      requests: nativeProbe.discoveries,
    })}`);
  const failedRestCount = nativeProbe.discoveries.filter(request => request.service === 'rest').length;
  bookEditor.setSelection({line: 2, ch: 4}, {line: 2, ch: 4 + badOrigin.length});
  pasteRest(fixtureUrl);
  await during(100, () => assert(restDiscoveryCount() === failedRestCount, 'Correcting a host by paste must not automatically discover'));
  await commitRestUrl(bookEditor);
  await wait(() => find('.console-status').textContent.includes('Discovery · OK')
    && candidates().includes('/api/v1/Books'),
  'Typing a URL slash after correcting the host must discover and offer real native paths');
  assert(bookEditor.getLine(2) === `GET ${fixtureUrl}/api/`
    && bookEditor.getCursor().ch === bookEditor.getLine(2).length,
  'Native recovery must preserve the complete corrected URL');
  assert(nativeProbe.discoveries.filter(request => request.service === 'rest').length === failedRestCount + 1,
    'Only the corrected connection may start a fresh native discovery');
  await key('Enter');
  assert(bookEditor.getLine(2) === `GET ${fixtureUrl}/api/v1/Books`,
    'Recovered automatic path selection must preserve the complete local origin');
  await type('/');
  await type('/');
  await during(700, () => assert(nativeProbe.discoveries.filter(request => request.service === 'rest').length === failedRestCount + 1,
    'Typing path slashes after real native success must not rediscover the same API'));
  await key('Escape');
  const bookEndpoint = `${fixtureUrl}/api/v1/Books/`;
  const bookPrefix = method => `#check test1\n#endpoint\n${method} ${bookEndpoint}\n#headers\nContent-Type: application/json\nAccept: application/json\n#body\n`;
  const bookFields = ['"id": ', '"title": ""', '"description": ""', '"pageCount": ', '"excerpt": ""', '"publishDate": ""'];
  setSource(bookEditor, bookPrefix('POST'));
  await key('Control+Space');
  const bookLoaded = restDiscoveryCount();
  await key('Control+Space');
  const consoleObject = bookEditor.state.completionActive?.data?.list.find(item => item.text === '{\n  \n}');
  assert(consoleObject?.displayText === '{} — JSON object'
    && !consoleObject.displayText.includes('\n')
    && document.querySelector('.CodeMirror-hint')?.textContent === '{} — JSON object',
  'REST Console must show a compact one-line object label while keeping its multiline insertion text');
  await pick(bookEditor, '{\n  \n}');
  assert(bookEditor.getCursor().ch === 2 && bookEditor.getLine(bookEditor.getCursor().line).trim() === '',
    'A compact REST object label must still insert formatted JSON with the caret on its inner line');
  await wait(() => bookFields.every(field => candidates().includes(field)),
    'Real native OpenAPI discovery must automatically offer all Book fields for POST /api/v1/Books/');
  assert(bookEditor.__servicesCompletion.discovery.specUrl === `${fixtureUrl}/spec.json`,
    'Native discovery must find a schema published only at spec.json, as httpbin does');
  assert(document.querySelector('.CodeMirror-hints')?.textContent.includes('date-time (ISO 8601)'),
    'The actual date menu label must retain the OpenAPI date-time format instead of reporting only string');
  await type('"');
  await wait(() => bookFields.every(field => candidates().includes(field)),
    'Typing an opening quote must retain Book property suggestions, not stop at a generic quote');
  for (const [index, [name, value]] of [
    ['title', 'Test book'], ['id', 123], ['description', 'REST client test'],
    ['pageCount', 100], ['excerpt', 'First chapter'], ['publishDate', '2026-10-24T00:00:00Z'],
  ].entries()) {
    const candidate = `"${name}": ${typeof value === 'string' ? '""' : ''}`;
    await pick(bookEditor, candidate);
    await type(String(value));
    if (typeof value === 'string') await key('ArrowRight');
    if (index < 5) await key('Enter');
  }
  assert(JSON.parse(bookEditor.getValue().split('#body\n')[1]).publishDate === '2026-10-24T00:00:00Z',
    'Selecting Book fields must produce valid JSON, including automatic commas');
  assert(bookEditor.getLine(2) === `POST ${bookEndpoint}`,
    'Schema matching must not remove the trailing slash from the actual request URL');
  for (const method of ['GET', 'POST']) {
    setSource(bookEditor, `${bookPrefix(method)}{\n  "`);
    await key('Control+Space');
    await wait(() => method === 'POST' ? bookFields.every(field => candidates().includes(field)) : candidates().length === 0,
      'REST body fields must follow the selected HTTP method from the already loaded schema');
  }
  assert(restDiscoveryCount() === bookLoaded, 'Book body editing and method changes must not repeat discovery');
  await key('Escape');
  await click(testid('console-service-rest'));
  const magentoEditor = cm('console-editor');
  const magentoPath = '/rest/default/V1/integration/admin/token';
  const magentoUrl = `http://{{var:magento}}${magentoPath}`;
  const magentoPrefix = endpoint =>
    `#check magento\n#endpoint\nPOST ${endpoint}\n#headers\nContent-Type: application/json\n#body\n`;
  const credentials = ['"username": ""', '"password": ""'];
  const checkLocalBodyHints = async () => {
    for (const source of [magentoPrefix(magentoUrl).trimEnd(), magentoPrefix(magentoUrl)]) {
      setSource(magentoEditor, source);
      await settle();
      await wait(() => !find('.console-status').classList.contains('is-running'),
        'The focus bootstrap must finish before counting shortcut-triggered discovery');
      const count = restDiscoveryCount();
      const keyNames = [];
      const recordKey = event => {
        if (event.code === 'Space') keyNames.push(CodeMirror.keyName(event));
      };
      const input = magentoEditor.getInputField();
      input.addEventListener('keydown', recordKey, true);
      try {
        for (let index = 0; index < 12; index++) {
          await key('Control+Space');
          await settle();
        }
        // Window longer than any discovery debounce: repeated shortcuts must never schedule a discovery.
        await holds(() => restDiscoveryCount() === count && magentoEditor.getValue() === source,
          'Repeated Ctrl-Space on #body must not discover or modify the request', 650);
        assert(keyNames.length === 12 && keyNames.every(name => name === 'Ctrl-Space'),
          'The repeated shortcut fixture must deliver actual Ctrl-Space events, not Alt-Space');
        assert(restDiscoveryCount() === count && magentoEditor.getValue() === source,
          `Repeated Ctrl-Space on #body or its empty content must not discover or modify the request: ${JSON.stringify({
            cursor: magentoEditor.getCursor(), before: count, after: restDiscoveryCount(),
            status: find('.console-status').textContent, keyNames,
          })}`);
      } finally {
        input.removeEventListener('keydown', recordKey, true);
      }
    }
    setSource(magentoEditor, magentoPrefix(magentoUrl));
  };
  const beforeMagento = restDiscoveryCount();
  setSource(magentoEditor, magentoPrefix(magentoUrl));
  await during(100, () => assert(restDiscoveryCount() === beforeMagento, 'A new variable URL and body must wait for a hint request'));
  await key('Control+Space');
  const magentoFixtureUrl = fixtureUrl.replace('127.0.0.1', 'localhost');
  await wait(() => magentoEditor.__servicesCompletion?.discovery?.specUrl === `${magentoFixtureUrl}/rest/default/swagger.json`);
  const magentoLoaded = restDiscoveryCount();
  assert(nativeProbe.discoveries.filter(request => request.service === 'rest').at(-1).params.url === magentoUrl,
    'Native discovery must receive the original variable-based URL for its normal materialization');
  await checkLocalBodyHints();
  await pick(magentoEditor, '{\n  \n}');
  await wait(() => credentials.every(field => candidates().includes(field)),
    'A loaded native Swagger schema must automatically suggest username/password for a variable-based Magento URL');
  assert(magentoEditor.__servicesCompletion.discovery.specUrl === `${magentoFixtureUrl}/rest/default/swagger.json`,
    'The credentials must come from the actual discovered Swagger 2 document, not hardcoded body fields');
  await type('"');
  await wait(() => credentials.every(field => candidates().includes(field)));
  await pick(magentoEditor, '"username": ""');
  await type('fixture-admin');
  await key('ArrowRight');
  await key('Enter');
  await pick(magentoEditor, '"password": ""');
  await type('fixture-password');
  await key('ArrowRight');
  assert(JSON.stringify(JSON.parse(magentoEditor.getValue().split('#body\n')[1]))
    === JSON.stringify({username: 'fixture-admin', password: 'fixture-password'}),
  'Schema hints must create a valid credentials body without sending an authentication request');
  assert(magentoEditor.getLine(2) === `POST ${magentoUrl}`,
    'Body completion must preserve the raw host variable in the request document');
  for (const endpoint of [
    `{{var:magentoBase}}${magentoPath}`,
    '{{var:magentoToken}}',
    `http://{{:magento}}${magentoPath}`,
  ]) {
    setSource(magentoEditor, `${magentoPrefix(endpoint)}{\n  `);
    await type('"');
    await wait(() => credentials.every(field => candidates().includes(field)),
      `Cached body hints must also resolve this URL template: ${endpoint}`);
    assert(magentoEditor.getLine(2) === `POST ${endpoint}`,
      'Looking up a body schema must not expand variables inside the editor');
  }
  await during(650, () => assert(restDiscoveryCount() === magentoLoaded,
    'Editing credentials and changing equivalent URL templates must not refetch the loaded API schema'));
  setSource(magentoEditor, magentoPrefix(magentoUrl));
  await key('Control+Space');
  const requiredHint = magentoEditor.state.completionActive?.data?.list.find(item =>
    item.displayText.endsWith(' — Required fields'));
  assert(requiredHint && !requiredHint.displayText.includes('\n'),
    'Empty REST body must offer a compact required-fields template from discovered Swagger metadata');
  const requiredBody = JSON.parse(requiredHint.text);
  assert(Object.keys(requiredBody).sort().join(',') === 'password,username'
    && requiredBody.username === '' && requiredBody.password === '',
  'The template must include only required request fields, excluding optional and readOnly properties');
  assert(candidates().includes('{\n  \n}'), 'The ordinary empty-object choice must remain available');
  await pick(magentoEditor, requiredHint.text);
  let valueCursor = magentoEditor.getCursor();
  assert(magentoEditor.getLine(valueCursor.line).slice(valueCursor.ch - 1, valueCursor.ch + 1) === '""',
    'Choosing required fields must put the caret inside the first empty string value');
  const firstRequired = magentoEditor.getLine(valueCursor.line).match(/"(password|username)":/)[1];
  await type('first-fixture');
  const otherRequired = firstRequired === 'password' ? 'username' : 'password';
  const otherLine = magentoEditor.getValue().split('\n').findIndex(line => line.includes(`"${otherRequired}": ""`));
  magentoEditor.setCursor(otherLine, magentoEditor.getLine(otherLine).indexOf(': ""') + 3);
  await type('second-fixture');
  await key('ArrowRight');
  const lastProperty = magentoEditor.getValue().split('\n').findIndex(line => line.includes(`"${Object.keys(requiredBody).at(-1)}":`));
  magentoEditor.setCursor(lastProperty, magentoEditor.getLine(lastProperty).length);
  await key('Enter');
  await wait(() => candidates().includes('"scope": ""'));
  assert(!candidates().some(text => text.startsWith('"username"') || text.startsWith('"password"') || text.startsWith('"generated"')),
    'After filling the required template, hints must offer optional fields without duplicates or readOnly fields');
  await pick(magentoEditor, '"scope": ""');
  await type('fixture-scope');
  const completedCredentials = JSON.parse(magentoEditor.getValue().split('#body\n')[1]);
  assert(completedCredentials.scope === 'fixture-scope' && Object.keys(completedCredentials).length === 3,
    'Adding an optional field after the required template must preserve valid JSON and automatic commas');
  assert(restDiscoveryCount() === magentoLoaded && magentoEditor.getLine(2) === `POST ${magentoUrl}`,
    'Required-template selection and filling must not rediscover or rewrite the connection URL');
  await key('Escape');
  const enumUrl = `http://{{var:magento}}/rest/default/V1/enum-test`;
  const enumLoaded = restDiscoveryCount();
  const checkRestEnums = async (editor, prefix) => {
    setSource(editor, prefix);
    await key('Control+Space');
    await wait(() => editor.state.completionActive?.data?.list.some(item =>
      item.displayText.endsWith(' — Required fields')));
    const template = editor.state.completionActive.data.list.find(item =>
      item.displayText.endsWith(' — Required fields'));
    assert(JSON.parse(template.text).status === 'pending',
      'Required enum fields must use an allowed value instead of an invalid empty string');
    setSource(editor, `${prefix}{\n  `);
    await key('Control+Space');
    await wait(() => candidates().includes('"status": ""'));
    assert(editor.state.completionActive.data.list.find(item => item.text === '"status": ""')
      .displayText.endsWith(' — string · enum'), 'REST enum fields must be labeled with their type and enum');
    await pick(editor, '"status": ""');
    await wait(() => candidates().includes('"pending"') && candidates().includes('"active"'),
      'Choosing an enum property must automatically open its value menu inside the empty quotes');
    await pick(editor, '"active"');
    for (const [property, value] of [['priority', '2'], ['enabled', 'false']]) {
      await key('Enter');
      await pick(editor, `"${property}": `);
      await wait(() => candidates().includes(value), 'Scalar enums must offer unquoted JSON values');
      await pick(editor, value);
    }
    await key('Enter');
    await pick(editor, '"tags": []');
    await wait(() => candidates().includes('"red"') && candidates().includes('"blue"'),
      'Enum array items must open their value choices inside []');
    await pick(editor, '"red"');
    await type(', ');
    await wait(() => candidates().includes('"blue"'));
    await pick(editor, '"blue"');
    await key('ArrowRight');
    await key('Enter');
    await pick(editor, '"nullable": ""');
    await wait(() => candidates().includes('null'));
    await pick(editor, 'null');
    await key('Enter');
    await pick(editor, '"escaped": ""');
    const escaped = 'quote"slash\\ки😀"';
    await wait(() => candidates().includes(JSON.stringify(escaped)));
    await pick(editor, JSON.stringify(escaped));
    for (const [property, value] of [['constantArray', []], ['constantObject', {mode: 'fast'}]]) {
      await key('Enter');
      await pick(editor, `"${property}": `);
      await wait(() => candidates().includes(JSON.stringify(value)));
      await pick(editor, JSON.stringify(value));
    }
    await type('\n}');
    assert(JSON.stringify(JSON.parse(editor.getValue().slice(prefix.length)))
      === JSON.stringify({status: 'active', priority: 2, enabled: false,
        tags: ['red', 'blue'], nullable: null, escaped, constantArray: [], constantObject: {mode: 'fast'}}),
    'Enum selection must preserve real JSON types, escape strings and add commas without duplicate quotes');
    assert(restDiscoveryCount() === enumLoaded, 'Enum choices must use the cached schema without discovery');
    await key('Escape');
  };
  await click(testid('console-service-rest'));
  const categoryPath = '/rest/default/V1/categories/{categoryId}/move';
  const pathEditor = cm('console-editor');
  setSource(pathEditor, '#check move\n#endpoint\nPUT http://{{var:magento}}/rest/default/V1/categories/');
  await pick(pathEditor, categoryPath);
  const checkPathParameterColor = async (editor, line) => {
    await wait(() => editor.getLineTokens(line).some(token => token.type?.includes('rest-path-param')));
    const parameters = [...editor.getWrapperElement().querySelectorAll('.cm-rest-path-param')];
    assert(parameters.length === 1 && parameters[0].textContent === '{categoryId}'
      && getComputedStyle(parameters[0]).color === 'rgb(196, 154, 108)',
    'A discovered REST path parameter must be brown, including both braces');
    const variable = editor.getWrapperElement().querySelector('.cm-gw-placeholder');
    assert(variable && getComputedStyle(variable).color === 'rgb(255, 85, 85)',
      'Path parameter highlighting must not change the red profile-variable color');
    const text = editor.getLine(line);
    const start = text.indexOf('{categoryId}');
    editor.replaceRange('42', {line, ch: start}, {line, ch: start + '{categoryId}'.length});
    await wait(() => !editor.getLineTokens(line).some(token => token.type?.includes('rest-path-param')));
    assert(editor.getLine(line).includes('/categories/42/move')
      && editor.getLine(line).includes('{{var:magento}}'),
    'Replacing a path parameter must clear its highlight without modifying URL variables');
  };
  await checkPathParameterColor(pathEditor, 2);
  assert(pathEditor.getLine(2) === 'PUT http://{{var:magento}}/rest/default/V1/categories/42/move',
    'Discovery insertion must retain the full method and variable-based URL');
  setSource(pathEditor, `#check move\n#endpoint\nPUT http://{{var:magento}}${categoryPath}\n#body\n{"note":"{categoryId}"}`);
  await checkPathParameterColor(pathEditor, 2);
  assert(!pathEditor.getLineTokens(4).some(token => token.type?.includes('rest-path-param')),
    'Braces inside a REST JSON body must not become URL-parameter tokens');
  assert(restDiscoveryCount() === enumLoaded, 'Highlighting and replacing path parameters must not run discovery');
  await checkRestEnums(cm('console-editor'), magentoPrefix(enumUrl));
  await click(testid('console-service-graphql'));
  const discoveryEditor = cm('console-editor');
  const discoveryEndpoint = `${fixtureUrl}/graphql-native-discovery`;
  setSource(discoveryEditor, `#check protected-schema\n#endpoint\n${discoveryEndpoint}\n#query\n{\n  `);
  await key('Alt+Space');
  await wait(() => find('.console-status').textContent.includes('Discovery · ERROR')
    && find('.console-status').textContent.includes('GraphQL discovery requires X-Token'),
  'The real native registry must report GraphQL errors instead of caching an empty successful schema');
  assert(![...document.querySelectorAll('[role="alert"]')].some(visible),
    'A real native manual discovery error must appear only in status/debug');
  const authenticated = `#check protected-schema\n#endpoint\n${discoveryEndpoint}\n#headers\nX-Token: {{second:TOKEN}}\n#query\n{\n  `;
  setSource(discoveryEditor, authenticated);
  discoveryEditor.setCursor(4, discoveryEditor.getLine(4).length);
  await key('Enter');
  discoveryEditor.setCursor(discoveryEditor.lastLine(), discoveryEditor.getLine(discoveryEditor.lastLine()).length);
  await wait(() => find('.console-status').textContent.includes('Discovery · OK'),
    'GraphQL Console discovery must retry with materialized request headers');
  await key('Control+Space');
  await wait(() => candidates().includes('products'),
    'Authenticated native discovery must restore real root fields, not just __typename');
  await pick(discoveryEditor, 'products');
  await key('Space');
  await pick(discoveryEditor, '{\n    \n  }');
  await wait(() => candidates().includes('sku') && candidates().includes('images'),
    'Real native introspection must restore nested GraphQL field completion');
  await key('Escape');
  const materializationQuery = 'query ($skus: [String]) { products(skus: $skus) { sku images { url } } }';
  const materializationVariables = '{"skus":["{{var:sku}}"]}';
  await click(testid('console-service-graphql'));
  const materializationEditor = cm('console-editor');
  const materializationSource = `#check materialization\n#endpoint\n${fixtureUrl}/graphql-materialization\n#variables\n${materializationVariables}\n#query\n${materializationQuery}`;
  setSource(materializationEditor, materializationSource);
  await key('Shift+Enter');
  await wait(() => materializationEditor.getValue().includes('#responce')
    && materializationEditor.getValue().includes('fixture-materialized-sku'),
  'GraphQL Console Variables must resolve array placeholders through the real companion');
  const materializationOutput = materializationEditor.getValue().slice(materializationEditor.getValue().lastIndexOf('#responce'));
  assert(!materializationOutput.includes('{{var:sku}}') && materializationEditor.getValue().startsWith(materializationSource),
    'Materialization must change the network payload, not the editable Console template');
  await testDriver.checkConsoleFolding(materializationEditor);
  assert(nativeProbe.clipboardReads === 0, 'Startup and requests must not read clipboard');
  const panelEditor = name => document.querySelector(`[data-testid="console-${name}-editor"]`);
  if (!visible(panelEditor('dictionary'))) await click(testid('console-dictionary-toggle'));
  await click(await wait(() => [...find('.console-panel-tabs').querySelectorAll('.tab-label')]
    .find(el => el.textContent === 'default')));
  await wait(() => cm('console-dictionary').getValue().includes('{{second:TOKEN}}'),
    'Variable reference fixture must be present');
  let variables = cm('console-dictionary');
  testid('console-service-rest').focus();
  const cursor = JSON.stringify(variables.getCursor());
  const lines = variables.getValue().split('\n');
  const line = lines.findIndex(text => text.includes('{{second:TOKEN}}'));
  assert(line >= 0, 'Variable reference fixture must be present');
  const start = lines[line].indexOf('{{second:TOKEN}}');
  variables.scrollIntoView({ line, ch: start }, 20);
  assert(!variables.hasFocus(), 'Hover must work without focus');
  for (let offset = 0; offset < '{{second:TOKEN}}'.length; offset++) {
    const box = variables.charCoords({ line, ch: start + offset }, 'window');
    for (const fraction of [0.2, 0.8]) {
      await move(box.left + (box.right - box.left) * fraction, (box.top + box.bottom) / 2);
      await wait(() => document.querySelector('.gw-var-tooltip')?.textContent === '{{second:TOKEN}} = resolved-fixture');
    }
  }
  assert(!variables.hasFocus() && JSON.stringify(variables.getCursor()) === cursor, 'Hover must not focus or move cursor');
  const outsideBox = testid('console-service-rest').getBoundingClientRect();
  await move(outsideBox.left + 5, outsideBox.top + 5);
  await wait(() => !document.querySelector('.gw-var-tooltip'));
  const original = variables.getValue();
  setSource(variables, original);
  await key('Enter');
  assert(candidates().length === 0, 'New variable line must not offer Console sections');
  await type('NEW_ENTRY: ');
  await key('Control+Space');
  assert(candidates().length === 0, 'Variable value must not offer HTTP/SQL syntax');
  await type('{{second:');
  await wait(() => candidates().length === 1 && candidates()[0] === '{{second:TOKEN}}');
  await key('Escape');
  for (const suffix of ['\n\n', '\n#', '\nAc', '\nSEL', '\nqu', '\nNAME: ']) {
    setSource(variables, original + suffix);
    await key('Control+Space');
    assert(candidates().length === 0, `Variables leaked suggestions for ${JSON.stringify(suffix)}`);
  }
  const beforeRestore = nativeProbe.saves;
  setSource(variables, original);
  await wait(() => nativeProbe.saves > beforeRestore, 'The debounced autosave of the restored variables must finish');
  const initial = nativeProbe.saves;
  setSource(variables, `${original}\n\nPROBE: `);
  await type('{{env:');
  await wait(() => candidates().includes('{{env:HOME}}'));
  await wait(() => nativeProbe.saves > initial, 'Variable autosave');
  await key('Escape');
  await key('Control+Space');
  await wait(() => candidates().includes('{{env:HOME}}'));
  assert(variables.getValue() === `${original}\n\nPROBE: {{env:`, 'Autosave must preserve draft and blank lines');
  const saved = nativeProbe.saves;
  await key('Escape');
  for (let i = 0; i < 4; i++) await key('Backspace');
  await type('second:');
  await wait(() => candidates().includes('{{second:TOKEN}}'));
  await wait(() => nativeProbe.saves > saved);
  await key('Escape');
  await key('Control+Space');
  await wait(() => candidates().includes('{{second:TOKEN}}'));
  await key('Escape');
  const beforeCleanup = nativeProbe.saves;
  setSource(variables, original);
  await wait(() => nativeProbe.saves > beforeCleanup, 'The restored dictionary must be saved before the panel checks');
  await click(testid('console-variables-toggle'));
  await wait(() => visible(panelEditor('variables')));
  await click(button(/^env /, find('.console-panel-tabs')));
  await wait(() => cm('console-variables').getOption('readOnly') === true);
  variables = cm('console-variables');
  assert(variables.getValue().includes('HOME:'), 'env must include companion HOME');
  assert(variables.getValue().includes('SERVICES_DEVTOOLS_ENV_PROBE: companion-fixture-only'), 'env from .zprofile');
  assert(variables.getValue().includes('SERVICES_DEVTOOLS_INTERACTIVE_PROBE: interactive-fixture-only'), 'env from .zshrc');
  await click(button(/^var$/, find('.console-panel-tabs')));
  await wait(() => cm('console-variables').getOption('readOnly') !== true);
  await click(testid('console-variables-toggle'));
  await wait(() => !visible(panelEditor('variables')), 'The tool panel must close before the shared Console checks');
  await testDriver.checkConsoleTools(`${fixtureUrl}/panel-tools`);
  const frame = document.createElement('iframe');
  frame.allow = "clipboard-read 'none'; clipboard-write 'none'";
  frame.src = location.href;
  frame.style.cssText = 'position:fixed;inset:0;width:100vw;height:100vh;z-index:99999;background:white';
  document.body.append(frame);
  await wait(() => frame.contentWindow?.testDriver
    && [...frame.contentDocument.querySelectorAll('.CodeMirror')].some(visible));
  const doc = frame.contentDocument;
  const win = frame.contentWindow;
  assert(win.testDriver, 'CDP initialization must run inside the policy iframe');
  await wait(() => win.nativeProbe?.ports > 0, 'Iframe clipboard transport must be synthetic before Import');
  await win.testDriver.activateSession(0);
  await wait(() => doc.querySelector('[data-testid="console-service-rest"]'));
  await click(testid('console-service-rest', doc));
  assert(!doc.featurePolicy.allowsFeature('clipboard-read'), 'Iframe must deny clipboard access');
  let denied = false;
  try { await win.navigator.clipboard.readText(); }
  catch (error) { denied = /permissions policy/i.test(error.message); }
  assert(denied, 'Browser clipboard must fail under Permissions Policy');
  assert(win.nativeProbe.clipboardReads === 0, 'No automatic clipboard read');
  await click(button(/^Import/, doc));
  await wait(() => cm('console-editor', doc).getValue().includes('POST https://fixture.invalid/import'));
  assert(cm('console-editor', doc).getValue().includes('X-Fixture: imported'), 'Import headers');
  assert(win.nativeProbe.clipboardReads === 1, 'Import reads synthetic clipboard exactly once');
  win.nativeProbe.clipboard = { text: 'not a command' };
  await click(button(/^Import/, doc));
  const notice = () => doc.querySelector('[data-testid="app-notice"]');
  const importLight = () => testid('console-import', doc).querySelector('.console-state-light').dataset.state;
  await wait(() => notice()?.textContent.includes('Import failed:'), 'Invalid clipboard command must show parser error');
  assert(!notice().textContent.includes('Grant clipboard access'), 'Explicit parser error');
  assert(importLight() === 'error', 'A failed Import must light its button');
  await holds(() => notice() && importLight() === 'error', 'Only Dismiss may acknowledge a banner error', 100);
  await click(button('Dismiss', doc));
  await wait(() => !notice() && importLight() === 'ready', 'Dismiss must clear the banner and the Import light');
  await click(testid('console-service-grpc', doc));
  win.nativeProbe.clipboard = { text: "grpcurl -plaintext -d '{\"fixture\":\"ok\"}' grpc.fixture.invalid:9000 fixture.Echo/Call" };
  await click(button(/^Import/, doc));
  await wait(() => cm('console-editor', doc).getValue().includes('fixture.Echo/Call'));
  win.nativeProbe.clipboard = { error: 'Fixture clipboard reader failed' };
  await click(button(/^Import/, doc));
  await wait(() => notice()?.textContent.includes('Fixture clipboard reader failed') && importLight() === 'error',
    'Native clipboard reader error must be visible and light Import');
  await click(testid('console-service-rest', doc));
  assert(importLight() === 'ready', 'Only the service whose Import failed lights its Import button');
  frame.remove();
  return true;
})()
