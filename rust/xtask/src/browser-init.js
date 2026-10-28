(() => {
  let next = 0;
  const pending = new Map();
  globalThis.__testResolve = (id,result) => { pending.get(id)(result); pending.delete(id); };
  const input = (method, params) => new Promise(resolve => {
    const id = ++next;
    pending.set(id, resolve);
    __testInput(JSON.stringify({ id, method, params }));
  });
  const assert = (value, message) => { if (!value) throw new Error(message); };
  const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
  globalThis.updateFixture = {release:null,error:null,requests:[],installs:[],branch:'devtool'};
  const connectNative = globalThis.chrome?.runtime?.connectNative?.bind(chrome.runtime);
  if (connectNative) chrome.runtime.connectNative = (...args) => {
    const port = connectNative(...args);
    port.onMessage.addListener(message => {
      if (message.ok && message.result?.currentVersion && message.result?.branch)
        updateFixture.branch = message.result.branch;
    });
    const send = port.postMessage.bind(port);
    port.postMessage = message => {
      if (message.service === 'updates' && message.op === 'install') {
        assert(!('archivePath' in message),
          'The interface must request native downloading, not supply a browser download');
        updateFixture.installs.push(message);
        if (updateFixture.archive)
          message = {...message, archivePath:updateFixture.archive};
      }
      return send(message);
    };
    return port;
  };
  const fetchOriginal = globalThis.fetch.bind(globalThis);
  globalThis.fetch = (input, options) => {
    const url = typeof input === 'string' ? input : input.url;
    const match = /^https:\/\/github\.com\/([^/]+\/[^/]+)\/releases\/latest\/download\/version\.json$/.exec(url);
    if (!match) return fetchOriginal(input, options);
    assert(input.credentials === 'include' && input.cache === 'no-store',
      'Private release checks must use browser-managed credentials and bypass caches');
    updateFixture.requests.push(url);
    if (updateFixture.error)
      return Promise.resolve(new Response('Not found', {status:404}));
    const version = globalThis.chrome.runtime.getManifest().version;
    const base = `https://github.com/${match[1]}/releases/download/devtool-v${version}`;
    const release = updateFixture.release || {
      schemaVersion:1,application:'ServicesDevTools',version,tag:`devtool-v${version}`,
      commit:'a'.repeat(40),repository:match[1],branch:updateFixture.branch,
      platform:'darwin',arch:'arm64',minimumMacOS:'13.5',
      archive:'services-macos-arm64.tar.gz',sha256:'b'.repeat(64),
      archiveUrl:`${base}/services-macos-arm64.tar.gz`,
      checksumsUrl:`${base}/checksums.txt`,installerUrl:`${base}/install.sh`,
    };
    return Promise.resolve(new Response(JSON.stringify(release), {
      headers:{'content-type':'application/json'},
    }));
  };
  if (globalThis.IDBObjectStore) {
    const put = IDBObjectStore.prototype.put;
    globalThis.__testFailSessionWrites = false;
    IDBObjectStore.prototype.put = function (...args) {
      if (this.name === 'sessions' && (globalThis.__testFailSessionWrites
        || new URL(location.href).searchParams.has('test-session-write-failure')))
        throw new DOMException('Fixture session storage write failed', 'QuotaExceededError');
      return put.apply(this, args);
    };
  }
  const wait = async (check, message = 'Browser assertion timed out', timeout = 15000) => {
    const deadline = Date.now() + timeout;
    while (Date.now() < deadline) {
      const value = await check();
      if (value) return value;
      await sleep(25);
    }
    throw new Error(typeof message === 'function' ? message() : message);
  };
  const visible = element => !!element && element.getBoundingClientRect().width > 0
    && element.getBoundingClientRect().height > 0;
  const find = (selector, doc = document) => {
    const element = [...doc.querySelectorAll(selector)].find(visible);
    assert(element, `Visible element not found: ${selector}`);
    return element;
  };
  const testid = (name, doc) => find(`[data-testid="${name}"]`, doc);
  const cm = (name, doc = document) => {
    const element = doc.querySelector(`[data-testid="${name}"]`);
    assert(element, `Editor element not found: ${name}`);
    const editor = element.CodeMirror || element.nextElementSibling?.CodeMirror;
    assert(editor, `CodeMirror not found: ${name}`);
    return editor;
  };
  const sessionRecord = async (operation = 'get', win = window) => {
    const db = await new Promise((resolve, reject) => {
      const request = win.indexedDB.open('servicesConsole', 1);
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    try {
      return await new Promise((resolve, reject) => {
        const transaction = db.transaction('sessions', operation === 'get' ? 'readonly' : 'readwrite');
        const store = transaction.objectStore('sessions');
        const request = operation === 'get' ? store.get('library') : store.delete('library');
        let result;
        request.onsuccess = () => { result = request.result; };
        transaction.oncomplete = () => resolve(result === undefined ? null : JSON.parse(result));
        transaction.onabort = () => reject(transaction.error || new Error('Session fixture transaction aborted'));
      });
    } finally { db.close(); }
  };
  const sessionLibrary = async (win = window) => {
    await wait(() => win.document.querySelector('[data-testid="console-session-name"]')?.dataset.storageBusy === 'false',
      'Session autosave must finish');
    return sessionRecord('get', win);
  };
  const assertNativeScrollbars = element => {
    const style=getComputedStyle(element);
    assert(style.colorScheme==='dark',
      'Native scrollbars must inherit the dark application scheme, including their expanded tracks');
    assert(style.scrollbarWidth==='auto' && style.scrollbarColor==='auto'
      && style.opacity==='1' && style.transitionDuration==='0s',
      `Scrollbar appearance and disappearance must remain native: ${JSON.stringify({
        element:element.className,
        opacity:style.opacity,transition:style.transitionDuration})}`);
  };
  const nativeTrackPaint = async (element,axis='vertical') => {
    const box=element.getBoundingClientRect();
    const screenshot=await input('Page.captureScreenshot',{format:'png'});
    const image=new Image();
    image.src=`data:image/png;base64,${screenshot.data}`;
    await image.decode();
    const canvas=document.createElement('canvas');
    const scale=image.width/window.innerWidth;
    canvas.width=Math.round(box.width*scale);
    canvas.height=Math.round(box.height*scale);
    const context=canvas.getContext('2d');
    context.drawImage(image,box.left*scale,box.top*scale,box.width*scale,box.height*scale,
      0,0,canvas.width,canvas.height);
    const vertical=axis==='vertical';
    const pixels=context.getImageData(vertical?canvas.width-4:0,vertical?0:canvas.height-4,
      vertical?1:canvas.width,vertical?canvas.height:1).data;
    const length=vertical?canvas.height:canvas.width;
    // A native track/thumb forms a continuous run; text beneath a faded overlay does not.
    let bright=0,total=0,run=0,longestRun=0;
    for(let y=20;y<length-20;y++) {
      if(Math.min(...pixels.slice(y*4,y*4+3))>60) {
        bright++;
        longestRun=Math.max(longestRun,++run);
      } else run=0;
      total++;
    }
    return {brightRatio:bright/total,brightRunRatio:longestRun/total};
  };
  const key = async name => {
    const parts = name.split('+');
    const raw = parts.pop();
    const keys = {
      Enter: ['Enter', 'Enter', 13, '\r'], Escape: ['Escape', 'Escape', 27, ''],
      Space: [' ', 'Space', 32, ' '], Tab: ['Tab', 'Tab', 9, '\t'],
      Backspace: ['Backspace', 'Backspace', 8, ''],
      ArrowDown: ['ArrowDown', 'ArrowDown', 40, ''], ArrowUp: ['ArrowUp', 'ArrowUp', 38, ''],
      ArrowRight: ['ArrowRight', 'ArrowRight', 39, ''],
      ArrowLeft: ['ArrowLeft', 'ArrowLeft', 37, ''],
      Home: ['Home', 'Home', 36, ''],
      End: ['End', 'End', 35, ''],
    };
    assert(keys[raw], `Unknown test key: ${name}`);
    const [value, code, windowsVirtualKeyCode, text] = keys[raw];
    const modifiers = (parts.includes('Control') ? 2 : 0) | (parts.includes('Alt') ? 1 : 0)
      | (parts.includes('Shift') ? 8 : 0);
    const params = { key: value, code, windowsVirtualKeyCode, modifiers };
    await input('Input.dispatchKeyEvent', { ...params, type: 'keyDown', text: modifiers ? '' : text });
    await input('Input.dispatchKeyEvent', { ...params, type: 'keyUp' });
  };
  const type = text => input('Input.insertText', { text });
  const move = (x, y) => input('Input.dispatchMouseEvent', { type: 'mouseMoved', x, y });
  const click = async (element, x, y, clickCount = 1) => {
    element.scrollIntoView({ block: 'nearest' });
    const box = element.getBoundingClientRect();
    x = box.left + (x ?? box.width / 2);
    y = box.top + (y ?? box.height / 2);
    await move(x, y);
    await input('Input.dispatchMouseEvent', { type: 'mousePressed', x, y, button: 'left', clickCount });
    await input('Input.dispatchMouseEvent', { type: 'mouseReleased', x, y, button: 'left', clickCount });
  };
  const doubleClick = async element => {
    await click(element);
    await click(element, undefined, undefined, 2);
  };
  const button = (text, root = document) => {
    const element = [...root.querySelectorAll('button')].find(el =>
      visible(el) && (text instanceof RegExp ? text.test(el.textContent.trim()) : el.textContent.trim() === text));
    assert(element, `Button not found: ${text}`);
    return element;
  };
  const activateSession = async (id, root = document) => {
    await wait(() => visible(root.querySelector('[data-testid="console-session-name"]')),
      'Console session controls must finish asynchronous startup');
    if (testid('console-session-name',root).dataset.sessionId === String(id)) return;
    const toggle=testid('console-sessions-toggle',root);
    if(toggle.getAttribute('aria-pressed')!=='true') await click(toggle);
    await wait(()=>testid('console-sessions',root).dataset.busy==='false');
    await click(testid(`session-switch-${id}`,root));
    await wait(()=>testid('console-session-name',root).dataset.sessionId===String(id));
  };
  let sessionInitialized=false;
  const choose = async service => {
    if(!sessionInitialized) {
      await activateSession(1);
      sessionInitialized=true;
    }
    const current = await wait(() => [...document.querySelectorAll('.service-view')].find(visible));
    if (current.dataset.service === 'console') {
      if (service === 'console') {
        await wait(() => [...current.querySelectorAll('.CodeMirror')].some(visible));
        return;
      }
      await click(testid('console-old'));
    }
    const picker = await wait(() => [...document.querySelectorAll('[data-testid="service-select"]')].find(visible));
    picker.value = service;
    picker.dispatchEvent(new Event('change', { bubbles: true }));
    await wait(() => picker.value === service && [...document.querySelectorAll('.CodeMirror')].some(visible));
    await sleep(30);
  };
  const setSource = (editor, source) => {
    editor.closeHint();
    editor.setValue(source);
    editor.focus();
    editor.setCursor(editor.lastLine(), editor.getLine(editor.lastLine()).length);
  };
  const commitRestUrl = async (editor, line = 2) => {
    editor.closeHint();
    const text = editor.getLine(line);
    editor.setCursor(line, text.length);
    if (text.endsWith('/'))
      editor.replaceRange('', {line, ch: text.length - 1}, {line, ch: text.length});
    await type('/');
  };
  const candidates = (doc = document) => [...doc.querySelectorAll('.CodeMirror')]
    .filter(visible).flatMap(el => (el.CodeMirror.state.completionActive?.data?.list || [])
      .map(item => typeof item === 'string' ? item : item.text));
  const suggest = async (editor, expected) => {
    await wait(async () => {
      await key('Control+Space');
      return candidates().includes(expected);
    }, `Completion not found: ${expected}`);
  };
  const pick = async (editor, expected) => {
    await suggest(editor, expected);
    const index = candidates().indexOf(expected);
    const selected = editor.state.completionActive.widget.selectedHint;
    for (let i = 0; i < Math.abs(index - selected); i++) await key(index > selected ? 'ArrowDown' : 'ArrowUp');
    assert(candidates()[editor.state.completionActive?.widget?.selectedHint] === expected,
      `Completion selection changed before insertion: ${expected}`);
    await key('Enter');
    assert(editor.hasFocus(), 'Completion must retain editor focus');
  };
  const pickThenEnter = async (editor, prefix, expected) => {
    setSource(editor, prefix);
    await pick(editor, expected);
    const lines = editor.lineCount();
    assert(candidates().length === 0, 'Completed value must close suggestions');
    await key('Enter');
    assert(editor.lineCount() === lines + 1, `${expected}: Enter must insert a line`);
  };
  const checkConsoleFolding = async editor => {
    await new Promise(resolve => requestAnimationFrame(resolve));
    const source = editor.getValue();
    const lines = source.split('\n');
    const history = JSON.stringify(editor.historySize());
    const regions = editor.getOption('servicesResponses') || [];
    const output = line => regions.some(region => line >= region.header && line < region.end);
    const checks = lines.flatMap((text, line) =>
      !output(line) && /^\s*#(?:check|request)(?:\s|$)/i.test(text) ? [line] : []);
    const folds = () => editor.getAllMarks().filter(mark => mark.__isFold);
    const toggle = async line => {
      editor.setCursor({ line, ch: 0 });
      editor.scrollIntoView({ line, ch: 0 }, 60);
      const gutter = await wait(() => {
        const marker = editor.lineInfo(line).gutterMarkers?.['console-fold-gutter'];
        return marker?.isConnected && visible(marker) && marker;
      }, `Console fold gutter missing on ${lines[line]}`);
      await click(gutter);
    };
    const expectRange = (mark, line, end) => {
      const range = mark.find();
      assert(range.from.line === line && range.from.ch === lines[line].length
        && range.to.line === end - 1 && range.to.ch === lines[end - 1].length,
      `Check must fold through responses to the next check or EOF: ${JSON.stringify({ range, line, end })}`);
      assert(editor.getRange(range.from, range.to) === `\n${lines.slice(line + 1, end).join('\n')}`,
        'A check fold must contain every section and response without consuming the next check');
    };
    assert(checks.length > 0 && folds().length === 0, 'Check folding fixture must begin unfolded');
    for (let index = 0; index < checks.length; index++) {
      const line = checks[index];
      const end = checks[index + 1] ?? lines.length;
      if (end <= line + 1) continue;
      await toggle(line);
      await wait(() => folds().length === 1);
      expectRange(folds()[0], line, end);
      await toggle(line);
      await wait(() => folds().length === 0);
    }
    const firstEnd = checks[1] ?? lines.length;
    const section = lines.findIndex((text, line) =>
      line > checks[0] && line < firstEnd && !output(line) && /^\s*#endpoint(?:\s|$)/i.test(text));
    const response = regions[0]?.header ?? lines.findIndex(text => /^\s*#respon(?:ce|se)(?:\s|$)/i.test(text));
    for (const child of [section, response].filter(line => line > checks[0] && line < firstEnd - 1)) {
      await toggle(child);
      await wait(() => folds().length === 1);
      const nested = folds()[0];
      const range = JSON.stringify(nested.find());
      await toggle(checks[0]);
      await wait(() => folds().length === 2);
      expectRange(folds().find(mark => mark !== nested), checks[0], firstEnd);
      await toggle(checks[0]);
      await wait(() => folds().length === 1);
      assert(folds()[0] === nested && JSON.stringify(nested.find()) === range,
        'Unfolding a check must preserve independent section and response folds');
      await toggle(child);
      await wait(() => folds().length === 0);
    }
    assert(editor.getValue() === source && JSON.stringify(editor.historySize()) === history,
      'Check folding must preserve the complete document and undo history');
  };
  const checkConsoleMoves = async () => {
    await choose('console');
    await click(testid('console-service-terminal'));
    let editor = cm('console-editor');
    const block = (name, command) => `#check ${name}\n#endpoint\n/bin/sh -c\n#command\n${command}`;
    setSource(editor, [
      block('move-first', "printf '%s\\n' 'first-executed' '#check literal-output' '#endpoint' 'not-a-request'"),
      block('move-second', "printf '%s\\n' 'second-executed'"),
      block('move-last', "printf '%s\\n' 'moved-executed'"),
    ].join('\n\n'));
    const checks = () => [...testid('console-check-menu').querySelectorAll('button')];
    const regions = () => editor.getOption('servicesResponses') || [];
    const line = name => editor.getValue().split('\n').indexOf(`#check ${name}`);
    const run = async (name, output) => {
      editor.focus();
      editor.setCursor(line(name) + 4, 0);
      editor.scrollTo(null, editor.heightAtLine(line(name), 'local'));
      await wait(() => checks().find(check => check.classList.contains('active'))?.dataset.line
        === String(line(name) + 1), 'The scoped status must follow the check being executed');
      await key('Shift+Enter');
      await wait(() => !find('.console-status').classList.contains('is-running')
        && find('.console-status').textContent.includes('OK')
        && editor.getValue().split('\n').includes(output),
        () => `Moved check must execute its own command: ${JSON.stringify({name, status:find('.console-status').textContent, source:editor.getValue(), regions:regions()})}`);
    };
    const paste = text => {
      const clipboard = new DataTransfer();
      clipboard.setData('text/plain', text);
      editor.getInputField().dispatchEvent(new ClipboardEvent('paste', {
        clipboardData:clipboard,bubbles:true,cancelable:true,
      }));
    };
    await run('move-first','first-executed');
    await wait(() => checks().length === 3);
    assert(editor.getValue().split('\n').includes('#check literal-output'),
      'Fixture must contain a real literal check marker from Terminal output');
    const last = line('move-last');
    editor.setSelection({line:last,ch:0}, {line:editor.lastLine(),ch:editor.getLine(editor.lastLine()).length});
    const moved = editor.getSelection();
    editor.replaceSelection('', 'start', 'cut');
    await wait(() => checks().length === 2);
    const cut = editor.getValue();
    editor.setCursor(regions()[0].end,0);
    paste(`${moved}\n\n`);
    await wait(() => checks().length === 3 && line('move-last') === regions()[0].end,
      'Pasting a moved check at a response boundary must create its own numbered link');
    const pasted = editor.getValue();
    editor.undo();
    await wait(() => editor.getValue() === cut && checks().length === 2);
    editor.redo();
    await wait(() => editor.getValue() === pasted && checks().length === 3);
    await click(testid('console-service-rest'));
    await click(testid('console-service-terminal'));
    const restored = cm('console-editor');
    restored.undo();
    await wait(() => restored.getValue() === cut && checks().length === 2,
      'Response ownership must survive editor remounts and Undo');
    restored.redo();
    await wait(() => restored.getValue() === pasted && checks().length === 3);
    editor = restored;
    await run('move-last','moved-executed');
    await wait(() => checks().length === 3);
    const first = regions().find(region => region.request === 0);
    editor.setSelection({line:0,ch:0},{line:first.end,ch:0});
    const complete = editor.getSelection();
    editor.getInputField().dispatchEvent(new ClipboardEvent('copy', {
      clipboardData:new DataTransfer(),bubbles:true,cancelable:true,
    }));
    const beforeAppend=editor.getValue(), beforeAppendRegions=JSON.stringify(regions());
    const tail=beforeAppend.trimEnd().split('\n');
    const appendAt={line:tail.length-1,ch:tail.at(-1).length};
    const appendLine=appendAt.line+2;
    editor.operation(()=>{
      editor.replaceRange('\n',appendAt,
        {line:editor.lastLine(),ch:editor.getLine(editor.lastLine()).length},'+input');
      editor.setCursor(editor.lastLine(),0);
      paste(complete);
    });
    await wait(()=>checks().length===4&&regions().some(region=>region.request===appendLine),
      ()=>`An automatic separator must preserve ownership when pasting a complete response at EOF: ${JSON.stringify({
        appendLine,checks:checks().map(check=>check.dataset.line),regions:regions(),source:editor.getValue(),
      })}`);
    assert(editor.getLine(appendLine-1)===''&&editor.getLine(appendLine)==='#check move-first',
      'A pasted executed check must gain one blank before its header, not inside its literal output');
    const appended=editor.getValue(), appendedRegions=JSON.stringify(regions());
    editor.undo();
    await wait(()=>editor.getValue()===beforeAppend&&JSON.stringify(regions())===beforeAppendRegions,
      'Pasting an owned response and adding its separator must undo together');
    editor.redo();
    await wait(()=>editor.getValue()===appended&&JSON.stringify(regions())===appendedRegions);
    editor.undo();
    await wait(()=>editor.getValue()===beforeAppend);
    const copiedLine = line('move-second');
    editor.setCursor(copiedLine,0);
    paste(complete);
    await wait(() => checks().length === 4 && regions().filter(region => region.request === copiedLine).length === 1,
      'Copying a complete check must preserve literal markers inside its response');
    const copied = regions().find(region => region.request === copiedLine);
    editor.setSelection({line:copiedLine,ch:0},{line:copied.end,ch:0});
    const cutComplete = editor.getSelection();
    editor.replaceSelection('', 'start', 'cut');
    await wait(() => checks().length === 3);
    editor.setCursor(0,0);
    paste(cutComplete);
    await wait(() => checks().length === 4 && regions().some(region => region.request === 0),
      'Moving a complete executed check must preserve its response ownership');
    const inside = regions().find(region => region.request === 0).header + 2;
    editor.setCursor(inside,0);
    paste(`${block('move-inserted', "printf '%s\\n' 'inserted-executed'")}\n\n`);
    await wait(() => checks().length === 5 && line('move-inserted') === inside+1,
      'A user-pasted check inside output must become a request without promoting the old output markers');
    const beforeUndo = editor.getValue();
    const beforeRegions = JSON.stringify(regions());
    editor.undo();
    await wait(() => checks().length === 4 && line('move-inserted') === -1);
    editor.redo();
    await wait(() => editor.getValue() === beforeUndo && JSON.stringify(regions()) === beforeRegions);
    await run('move-inserted','inserted-executed');
    assert(checks().length === 5, 'Running a check inserted in output must not expose old output as phantom requests');
    await run('move-first','first-executed');
    assert(checks().length === 5, 'Refreshing a split response must preserve all real checks and protect literal output');
  };
  const assertSignalGeometry = () => {
    const lights = [...document.querySelectorAll('.console-root .console-state-light')].filter(visible);
    assert(lights.length > 0, 'Navigation signals must exist before an operation');
    for (const light of lights) {
      const bounds = light.getBoundingClientRect();
      const tab = light.parentElement.getBoundingClientRect();
      const left = light.parentElement.matches('.console-check-link');
      const right = light.parentElement.parentElement.matches('.console-toolbar');
      if (left || right) {
        assert(Math.abs(bounds.height-tab.height)<0.01 && bounds.width===2
          && Math.abs(bounds.top-tab.top)<0.01
          && Math.abs(left?bounds.right-tab.right-1:bounds.left-tab.left+1)<0.01,
          'Vertical signals must cover the entire edge touching the workspace');
      } else {
        assert(Math.abs(bounds.width-tab.width)<0.01 && bounds.height===2
          && Math.abs(bounds.left-tab.left)<0.01 && Math.abs(bounds.bottom-tab.bottom-1)<0.01,
          'Horizontal signals must cover the entire bottom contact edge');
      }
      const style=getComputedStyle(light);
      if (light.dataset.state === 'ready') {
        assert(style.backgroundColor === 'rgba(0, 0, 0, 0)' && style.animationName==='none',
          'Idle and acknowledged signals must be transparent and still');
      } else if(light.dataset.state==='running') {
        assert(style.backgroundImage!=='none'
          && (style.animationName==='console-state-flow'||matchMedia('(prefers-reduced-motion: reduce)').matches),
          'Running signals must use a green flowing gradient, respecting reduced motion');
      } else if(light.dataset.state==='error') {
        assert(style.backgroundColor==='rgb(255, 137, 128)' && style.animationName==='none',
          'Errors must be steady red, never animated');
      }
    }
  };
  const checkConsoleTools = async endpoint => {
    const storedHints = async () => {
      const id=Number(testid('console-session-name').dataset.sessionId);
      return (await sessionLibrary()).sessions.find(s=>s.id===id).snapshot.hints.rest;
    };
    await choose('vars');
    await click(button(/^env /));
    await choose('console');
    await click(testid('console-service-rest'));
    const editor = cm('console-editor');
    const assertStatusBounds = () => {
      const root = find('.console-root').getBoundingClientRect();
      assert(root.left <= 2 && root.top <= 2
        && window.innerWidth - root.right <= 2 && window.innerHeight - root.bottom <= 2,
      'Console outer margins must be at most 2px on every side');
      for (const menu of [find('.console-tab-strip'), testid('console-check-menu'), find('.console-toolbar')]) {
        assert(parseFloat(getComputedStyle(menu).gap) <= 2,
          'Console menus must use compact gaps of at most 2px');
      }
      const toolbar=find('.console-toolbar');
      assert([...toolbar.querySelectorAll('button')].every(button=>getComputedStyle(button).textAlign==='left'),
        'Right-hand tool labels must be left aligned');
      const badge=testid('console-session-name'), badgeText=badge.textContent;
      const toolbarWidth=toolbar.getBoundingClientRect().width;
      badge.textContent='A very long session name '.repeat(30);
      const badgeBounds=badge.getBoundingClientRect(), toolbarBounds=toolbar.getBoundingClientRect();
      badge.textContent=badgeText;
      assert(Math.abs(toolbarBounds.width-toolbarWidth)<0.5 && badgeBounds.width<=toolbarBounds.width+0.5
        && Math.abs(badgeBounds.left-toolbarBounds.left)<0.5
        && getComputedStyle(badge).textAlign==='left' && getComputedStyle(badge).textOverflow==='ellipsis'
        && getComputedStyle(badge).borderTopWidth==='1px',
        'Even a long session name must remain a left-aligned badge bounded by the tool menu');
      const body = find('.console-workspace-body').getBoundingClientRect();
      const rightPane=[...document.querySelectorAll('.console-right-pane')].find(visible)?.getBoundingClientRect();
      if(rightPane) {
        const divider=getComputedStyle([...document.querySelectorAll('.console-right-pane')].find(visible));
        assert(divider.borderLeftWidth==='3px' && divider.borderLeftStyle==='solid',
          'The auxiliary pane must have a clear three-pixel separator');
      }
      const status = find('.console-status-row').getBoundingClientRect();
      const mainStatus = testid('console-main-status').getBoundingClientRect();
      const stop = testid('console-stop');
      const stopBounds = stop.getBoundingClientRect();
      assert(stop.textContent === 'S' && testid('console-main-status').contains(stop)
        && Math.abs(stopBounds.right - mainStatus.right + 1) < 1
        && Math.abs(stopBounds.top - mainStatus.top) < 1
        && Math.abs(stopBounds.bottom - mainStatus.bottom + 1) < 1,
      'The small S control must form the right edge of its own status');
      const stopStyle = getComputedStyle(stop);
      assert(stopBounds.width <= 24 && stopStyle.color === 'rgb(255, 137, 128)'
        && stopStyle.backgroundColor === 'rgb(74, 48, 48)',
      'Each status Stop control must be a small red S');
      const tabs = find('.console-tab-strip');
      const tabBounds = tabs.getBoundingClientRect();
      assert(getComputedStyle(tabs).borderBottomWidth === '0px'
        && Math.abs(tabBounds.bottom - body.top) < 1
        && Math.abs(tabBounds.left - body.left) < 1,
      'Service tabs must dock directly onto the central frame without a separate floating line');
      for (const tab of tabs.querySelectorAll('button')) {
        assert(Math.abs(tab.getBoundingClientRect().bottom - body.top) < 1,
          'Every service tab must touch the top edge of the central frame');
      }
      for (const tab of testid('console-check-menu').querySelectorAll('button')) {
        assert(Math.abs(tab.getBoundingClientRect().right - body.left) < 1,
          'Check tabs must touch the left edge of the central frame');
      }
      for (const tab of find('.console-toolbar').querySelectorAll('button')) {
        assert(Math.abs(tab.getBoundingClientRect().left - (rightPane?.right??body.right)) < 1,
          'Tool tabs must touch the right edge of the central frame');
      }
      const heading = [...document.querySelectorAll('.console-panel-heading')].find(visible);
      if (heading) {
        const panel = testid('console-tool-panel');
        const headingBounds = heading.getBoundingClientRect();
        const rightEditor = find('.CodeMirror', panel).getBoundingClientRect();
        const mainEditor = editor.getWrapperElement().getBoundingClientRect();
        const firstTab = heading.querySelector('.tab-button').getBoundingClientRect();
        assert(heading.closest('.console-top-bar') && !panel.contains(heading)
          && Math.abs(headingBounds.top - tabBounds.top) < 1
          && Math.abs(headingBounds.bottom - body.top) < 1
          && Math.abs(firstTab.left - rightEditor.left) < 1,
        'Right-panel headings must dock over their editor in the service-menu row, outside the panel body');
        assert(Math.abs(mainEditor.top - rightEditor.top) < 1,
          'Both editors must start at the same height without an internal panel header');
      }
      assert(Math.abs(body.left - status.left) < 1 && Math.abs(body.right - status.right) < 1,
        'Console status must align with the inner workspace, excluding both side menus');
      const statusStyle = getComputedStyle(find('.console-status'));
      for (const area of document.querySelectorAll('.console-root .CodeMirror, .console-root .CodeMirror-gutters')) {
        if (visible(area)) {
          assert(getComputedStyle(area).backgroundColor === 'rgb(37, 39, 42)',
            'Every Console editor and gutter must use a slightly darker neutral shade than the menu tabs');
        }
      }
      assert(Math.abs(body.bottom - status.top) < 1
        && statusStyle.borderTopWidth === '0px'
        && statusStyle.borderTopLeftRadius === '0px'
        && statusStyle.borderTopRightRadius === '0px'
        && statusStyle.backgroundColor === 'rgb(43, 45, 48)',
      'Status must dock directly below the central frame with the menu-tab background');
      const debug = document.querySelector('.console-debug-panel');
      if (visible(debug)) {
        const bounds = debug.getBoundingClientRect();
        assert(Math.abs(mainStatus.left - bounds.left) < 1 && Math.abs(mainStatus.right - bounds.right) < 1,
          'Expanded Debug must align with its own status column');
      }
      return [status.left, rightPane?.right??status.right];
    };
    const closedStatusBounds = assertStatusBounds();
    const links = () => [...testid('console-check-menu').querySelectorAll('button')];
    const active = () => testid('console-check-menu').querySelector('.active .console-check-number')?.textContent;
    const top = () => editor.lineAtHeight(editor.getScrollInfo().top, 'local');
    setSource(editor, ['cursor-one', 'cursor-two', 'cursor-three'].map(name =>
      `#check ${name}\n#endpoint\nGET ${endpoint}`).join('\n\n'));
    editor.setCursor(0, 0);
    editor.scrollTo(null, 0);
    await wait(() => links().length === 3 && active() === '1');
    const checkBackground = name => {
      const line=[...editor.getWrapperElement().querySelectorAll('pre.CodeMirror-line')]
        .find(line=>line.textContent.includes(`#check ${name}`));
      assert(line,`A visible check line must be rendered: ${name}`);
      return getComputedStyle(line,'::before').backgroundColor;
    };
    assert(checkBackground('cursor-one')===checkBackground('cursor-three')
      && checkBackground('cursor-one')!==checkBackground('cursor-two'),
      'Whole checks must alternate between two neutral backgrounds');
    const firstEndpoint=[...editor.getWrapperElement().querySelectorAll('pre.CodeMirror-line')]
      .find(line=>line.textContent.includes(`GET ${endpoint}`));
    assert(getComputedStyle(firstEndpoint,'::before').backgroundColor===checkBackground('cursor-one'),
      'Request sections must keep their owning check background, not alternate by physical row');
    const lastPoint = editor.charCoords({line:10,ch:0}, 'window');
    const scroller = editor.getScrollerElement().getBoundingClientRect();
    assert(lastPoint.bottom <= scroller.top + editor.getScrollInfo().clientHeight,
      'The cursor-selection regression must keep all three checks visible in one viewport');
    const point = editor.charCoords({line:6,ch:4}, 'window');
    const bounds = editor.getWrapperElement().getBoundingClientRect();
    await click(editor.getWrapperElement(), point.left - bounds.left + 1, (point.top + point.bottom) / 2 - bounds.top);
    await wait(() => active() === '2' && top() === 0,
      'Clicking inside check 2 must activate it even when the viewport still starts at check 1');
    await key('ArrowDown');
    await key('ArrowDown');
    await wait(() => active() === '3' && top() === 0,
      'Arrow-key cursor movement must activate check 3 without requiring a scroll');
    await key('ArrowUp');
    await wait(() => active() === '2');
    await key('Shift+Enter');
    await wait(() => testid('console-main-status').textContent.includes('OK')
      && testid('console-main-status').textContent.includes('#check cursor-two') && active() === '2',
      'Executing the visible cursor check must immediately use that same check for status and navigation');
    const response=(editor.getOption('servicesResponses')||[])[0];
    assert(response && editor.getLine(response.header-1).trim()!==''
      && editor.getLine(response.end-1)==='' && editor.getLine(response.end-2).trim()!==''
      && editor.getLine(response.end).startsWith('#check cursor-three'),
      'A response must touch its request, while the next check has exactly one empty separating line');
    const responseHeading=[...editor.getWrapperElement().querySelectorAll('pre.CodeMirror-line')]
      .find(line=>line.textContent.startsWith('#responce'));
    assert(responseHeading && getComputedStyle(responseHeading,'::before').backgroundColor===checkBackground('cursor-two'),
      'An inserted response must keep the same background as its request');
    const cursorStatus = testid('console-main-status').textContent;
    await click(testid('console-main-status'));
    const cursorDebug = testid('console-main-status-debug').textContent;
    assert(cursorDebug.trim() && !cursorDebug.includes('No debug info yet'),
      'Running the cursor-selected check must populate its Debug');
    await click(testid('console-main-status'));
    await click(links()[0]);
    await wait(() => editor.getCursor().line === 0 && active() === '1');
    assert(!testid('console-main-status').textContent.includes('#check cursor-two'),
      'Explicit numbered navigation must move the cursor and select the matching status');
    await click(links()[1]);
    await wait(() => active() === '2' && testid('console-main-status').textContent === cursorStatus);
    await click(testid('console-main-status'));
    assert(testid('console-main-status-debug').textContent === cursorDebug,
      'The cursor-selected check must retain its own Debug after numbered navigation');
    await click(testid('console-main-status'));
    const lines = Array.from({length: 110}, (_, i) =>
      [0, 99, 104].includes(i) ? `#check navigation-${i + 1}` : `line-${i + 1}`);
    lines[80]='wide-scroll-fixture '.repeat(160);
    setSource(editor, lines.join('\n'));
    editor.scrollTo(null, 0);
    await wait(() => links().length === 3 && active() === '1');
    assertStatusBounds();
    assert(testid('console-check-menu').getBoundingClientRect().width === 28
      && links().every(link => link.getBoundingClientRect().height === 22),
      'Check tabs must use a compact 28px rail and 22px targets');
    const railSource=editor.getValue();
    const handle=testid('console-check-resizer'), handleBox=handle.getBoundingClientRect();
    const x=handleBox.left+handleBox.width/2,y=handleBox.top+10;
    await input('Input.dispatchMouseEvent',{type:'mousePressed',x,y,button:'left',clickCount:1});
    await input('Input.dispatchMouseEvent',{type:'mouseMoved',x:x+44,y,button:'left',buttons:1});
    await input('Input.dispatchMouseEvent',{type:'mouseReleased',x:x+44,y,button:'left',clickCount:1});
    await wait(()=>testid('console-check-menu').getBoundingClientRect().width===72,
      ()=>`Dragging must set a 72px rail: ${JSON.stringify({
        width:testid('console-check-menu').getBoundingClientRect().width,
        value:testid('console-check-resizer').getAttribute('aria-valuenow'),
      })}`);
    const comment=links()[0].querySelector('.console-check-comment');
    assert(comment.textContent==='navigation-1' && comment.scrollWidth>comment.clientWidth
      && getComputedStyle(comment).whiteSpace==='nowrap' && editor.getValue()===railSource,
      'Dragging the rail must reveal a clipped check comment without altering the document');
    const widthFrame=document.createElement('iframe');
    widthFrame.style.cssText='position:fixed;left:-10000px;top:0;width:1197px;height:960px';
    widthFrame.src=location.href;
    document.body.appendChild(widthFrame);
    try {
      await wait(()=>widthFrame.contentDocument?.querySelector('[data-testid="console-check-menu"]')
        ?.getBoundingClientRect().width===28,'A fresh Temporary workspace must not inherit another session width');
    } finally {widthFrame.remove();}
    handle.focus();
    await key('Home');
    await wait(()=>testid('console-check-menu').getBoundingClientRect().width===28);
    assert(getComputedStyle(links()[0].querySelector('.console-check-comment')).display==='none',
      'The compact rail must show numbers only');
    await move(1, 1);
    const tabStyle = element => {
      const style = getComputedStyle(element);
      return ['backgroundColor', 'color', 'fontSize', 'fontWeight', 'borderTopColor', 'borderTopWidth']
        .map(property => style[property]).join('|');
    };
    assert(tabStyle(testid('console-service-rest')) === tabStyle(links()[0]),
      'Active service tabs and check tabs must share the same visual style');
    assert(tabStyle(testid('console-service-mysql')) === tabStyle(testid('console-old')),
      'Inactive service tabs and tool tabs must share the same visual style');
    assert(links().map(el => el.dataset.line).join(',') === '1,100,105',
      'Check numbers must point to actual document lines');
    await click(testid('console-agent-toggle'));
    for (const [index, line] of [[1, 99], [2, 104], [0, 0]]) {
      await click(links()[index]);
      await wait(() => top() === line && active() === String(index + 1),
        () => `Check jump must align even the last check: ${JSON.stringify({index, line, top:top(), scroll:editor.getScrollInfo()})}`);
      const wrapper=editor.getWrapperElement();
      assertNativeScrollbars(editor.getScrollerElement());
      assert(editor.getOption('scrollbarStyle')==='native-direct'
        && !wrapper.querySelector('.CodeMirror-vscrollbar,.CodeMirror-hscrollbar'),
        'Console must use its real native scroller, not a synchronized proxy scrollbar');
      const filler=wrapper.querySelector('.CodeMirror-scrollbar-filler');
      assert(getComputedStyle(filler).backgroundColor==='rgba(0, 0, 0, 0)',
        'The corner between editor scrollbars must not flash the vendor default white background');
      if(editor.display.nativeBarWidth===0) {
        await sleep(2600);
        for(const axis of ['vertical','horizontal']) {
          const paint=await nativeTrackPaint(editor.getScrollerElement(),axis);
          assert(paint.brightRunRatio<0.03,
            `Native scrollbar paint must fade after a check jump without moving the pointer: ${JSON.stringify({index,axis,paint})}`);
        }
        const box=wrapper.getBoundingClientRect();
        const point={x:box.left+box.width/2,y:box.top+box.height/2};
        await move(point.x,point.y);
        const beforeWheel=editor.getScrollInfo().top;
        await input('Input.dispatchMouseEvent',{type:'mouseWheel',...point,deltaX:80,deltaY:index===0?60:-60});
        await wait(()=>editor.getScrollInfo().top!==beforeWheel,
          'Manual wheel scrolling must work immediately after a check jump with the Agent open');
        await sleep(2600);
        for(const axis of ['vertical','horizontal']) {
          const paint=await nativeTrackPaint(editor.getScrollerElement(),axis);
          assert(paint.brightRunRatio<0.03,
            `Native scrollbar paint must fade after jump then wheel with the Agent open, without moving the pointer to its track: ${JSON.stringify({index,axis,paint})}`);
        }
      }
    }
    const wheelBox=editor.getWrapperElement().getBoundingClientRect();
    await move(wheelBox.left+wheelBox.width/2,wheelBox.top+wheelBox.height/2);
    await input('Input.dispatchMouseEvent',{type:'mouseWheel',x:wheelBox.left+wheelBox.width/2,
      y:wheelBox.top+wheelBox.height/2,deltaX:80,deltaY:100});
    await wait(()=>editor.getScrollInfo().left>0 && editor.getScrollInfo().top>0,
      'Native editor scrollbars must preserve real wheel scrolling in both directions');
    assertNativeScrollbars(editor.getScrollerElement());
    if(editor.display.nativeBarWidth===0) {
      const bar=editor.getScrollerElement();
      const b=bar.getBoundingClientRect();
      const x=b.right-5;
      const y=b.top+(bar.scrollTop+bar.clientHeight/2)*bar.clientHeight/bar.scrollHeight;
      await move(x,y);
      await sleep(400);
      const beforeDrag=editor.getScrollInfo().top;
      await input('Input.dispatchMouseEvent',{type:'mousePressed',x,y,button:'left',clickCount:1});
      await sleep(1400);
      assert((await nativeTrackPaint(bar)).brightRatio>0.5,
        'A real native scrollbar interaction must remain active while held, without a custom hide timer');
      await input('Input.dispatchMouseEvent',{type:'mouseMoved',x,y:y+80,button:'left',buttons:1});
      await input('Input.dispatchMouseEvent',{type:'mouseReleased',x,y:y+80,button:'left',clickCount:1});
      await wait(()=>editor.getScrollInfo().top>beforeDrag+20,
        'The native thumb must remain draggable after a programmatic check jump');
      const viewport=editor.getViewport();
      const topLine=editor.lineAtHeight(editor.getScrollInfo().top,'local');
      assert(viewport.from<=topLine && viewport.to>topLine,
        'Dragging the real scrollbar must keep the visible document rendered');
      await move(wheelBox.left+wheelBox.width/2,wheelBox.top+wheelBox.height/2);
      await sleep(2600);
      assert((await nativeTrackPaint(bar)).brightRunRatio<0.03,
        'The real native track must fade after dragging ends and the pointer leaves it');
      editor.scrollTo(0,editor.heightAtLine(80,'local'));
      await sleep(2600);
      const lineBox=editor.charCoords({line:80,ch:0},'page');
      const wrapperBox=editor.getWrapperElement().getBoundingClientRect();
      const edge={left:bar.getBoundingClientRect().right-6,top:(lineBox.top+lineBox.bottom)/2};
      const expected=editor.coordsChar(edge,'page');
      await click(editor.getWrapperElement(),edge.left-wrapperBox.left,edge.top-wrapperBox.top);
      await wait(()=>editor.getCursor().line===80 && Math.abs(editor.getCursor().ch-expected.ch)<=1,
        'A faded native scrollbar must not intercept clicks on editor text beneath its edge');
    }
    await click(testid('console-agent-toggle'));
    await click(links()[1]);
    editor.setCursor(103, 0);
    editor.scrollTo(null, editor.heightAtLine(98, 'local'));
    await wait(() => active() === '2',
      'Scrolling into check 1 must keep check 2 active while its cursor is still visible');
    editor.scrollTo(null, 0);
    await wait(() => active() === '1',
      'Viewport ownership must take over only after the cursor leaves the visible editor');
    editor.scrollTo(null, editor.heightAtLine(101, 'local'));
    await wait(() => active() === '2', 'Revealing the cursor again must restore its check');
    editor.scrollTo(null, editor.heightAtLine(105, 'local'));
    await wait(() => active() === '3', 'Scrolling past the cursor must select the check at the top edge');
    editor.scrollTo(null, editor.heightAtLine(101, 'local'));
    await wait(() => active() === '2');
    editor.replaceRange('#check inserted\n', {line:50,ch:0});
    await wait(() => links().length === 4);
    assert(links().map(el => el.querySelector('.console-check-number').textContent).join(',') === '1,2,3,4'
      && links()[2].dataset.line === '101', 'Inserting a check renumbers later links and updates line targets');
    editor.replaceRange('', {line:50,ch:0}, {line:51,ch:0});
    await wait(() => links().length === 3 && links()[1].dataset.line === '100');
    const source = editor.getValue();
    assert(!document.querySelector('[data-testid="service-select"]'),
      'The new Console must not render the legacy service picker');
    const old = testid('console-old');
    const oldBounds = old.getBoundingClientRect();
    const assertOldAlignment = () => {
      const old = testid('console-old').getBoundingClientRect();
      const status = find('.console-status').getBoundingClientRect();
      assert(Math.abs(old.top - status.top) < 1 && Math.abs(old.bottom - status.bottom) < 1
        && Math.abs(old.right - status.left) < 1 && old.width === 28,
        'O must align with the status row and dock directly to its left');
    };
    assert(old.textContent === 'O' && !find('.console-toolbar').contains(old)
      && !testid('console-check-menu').contains(old),
      'O must be outside the scrolling check list and right menu');
    const inactiveCheck = links().find(link => !link.classList.contains('active'));
    const leftTabStyle = element => {
      const style = getComputedStyle(element);
      return ['padding', 'borderRadius', 'borderRightWidth', 'minHeight', 'lineHeight']
        .map(property => style[property]).join('|');
    };
    assert(tabStyle(old) === tabStyle(inactiveCheck)
      && leftTabStyle(old) === leftTabStyle(inactiveCheck),
      'O must use the standard inactive left check-tab style, including its shape and dimensions');
    assertOldAlignment();
    await click(find('.console-status'));
    await wait(() => visible(document.querySelector('.console-debug-panel')));
    assertOldAlignment();
    await click(find('.console-status'));
    await wait(() => !visible(document.querySelector('.console-debug-panel')));
    setSource(editor, Array.from({length: 60}, (_, index) =>
      `#check Scroll ${index + 1}\n#endpoint\nGET ${endpoint}`).join('\n\n'));
    await wait(() => links().length === 60);
    const checkMenu = testid('console-check-menu').querySelector('.console-check-links');
    checkMenu.scrollTop = checkMenu.scrollHeight;
    assert(checkMenu.scrollTop > 0 && checkMenu.scrollHeight > checkMenu.clientHeight
      && old.getBoundingClientRect().bottom === oldBounds.bottom,
      'Scrolling check navigation must not move the legacy-interface button');
    assert(testid('console-check-resizer').getBoundingClientRect().top
      ===testid('console-check-menu').getBoundingClientRect().top,
      'The check-menu resize handle must remain accessible while its entries scroll');
    setSource(editor, source);
    await wait(() => links().length === 3);
    await click(old);
    await wait(() => visible(document.querySelector('.service-view[data-service="terminal"]')));
    assert(getComputedStyle(document.body).padding === '12px',
      'Compact Console spacing must not affect the legacy interface');
    assert(getComputedStyle(cm('field-command-editor').getWrapperElement()).backgroundColor === 'rgb(40, 42, 54)',
      'Console editor background overrides must not affect legacy service editors');
    assert(testid('service-select').value === 'terminal' && visible(testid('field-command-editor')),
      'Old must open the legacy Terminal client, not a Console tool panel');
    await choose('console');
    assert(cm('console-editor').getValue() === source && visible(testid('console-service-rest')),
      'Returning through the legacy menu must preserve the Console service and document');
    await click(testid('console-hints-toggle'));
    const panel = testid('console-tool-panel');
    const workspace = find('.console-top-bar');
    const toolStatus = () => [...document.querySelectorAll('.console-right-status .console-status')].find(visible);
    const mainStatusText = () => testid('console-main-status').textContent;
    const profileToolLight = tool => testid(`console-${tool}-toggle`).querySelector('.console-state-light')?.dataset.state;
    await wait(() => Math.abs(panel.closest('.console-right-pane').getBoundingClientRect().width
      - workspace.clientWidth / 2) < 1);
    assert(JSON.stringify(assertStatusBounds()) === JSON.stringify(closedStatusBounds),
      'Opening the right panel must not change status boundaries');
    assert(getComputedStyle(find('.console-toolbar')).flexDirection === 'column'
      && getComputedStyle(testid('console-check-menu')).flexDirection === 'column',
      'Both side menus must be vertical');
    const hints = cm('console-hints');
    assert(testid('console-hints-title').textContent === 'REST completion hints'
      && tabStyle(testid('console-hints-title')) === tabStyle(testid('console-service-rest')),
      'Hints must have a tab-styled title matching the active service tabs');
    const originalHints = hints.getValue();
    const changedHints = `${originalHints.trimEnd()}\n#headers\nX-Console-Panel-Fixture: saved\n`;
    hints.setValue(changedHints);
    await click(testid('console-hints-toggle'));
    await wait(async () => await storedHints() === changedHints,
      'Closing hints must persist the edited text in the named session');
    await click(testid('console-hints-toggle'));
    await wait(() => cm('console-hints').getValue() === changedHints);
    cm('console-hints').setValue('## accidental-service-boundary\ninvalid fixture');
    await wait(() => toolStatus().textContent.includes('ERROR')
      && toolStatus().textContent.includes('service headers'));
    assert(await storedHints() === changedHints, 'Invalid hints drafts must not replace the last valid persisted hints');
    const hintsError = toolStatus().textContent;
    const hintsMainStatus = mainStatusText();
    await click(toolStatus());
    assertStatusBounds();
    assert(testid('console-hints-status-debug').textContent.includes('Hints validation'),
      'Hints errors must reach the right tool Debug, not the check Debug');
    await click(testid('console-hints-toggle'));
    assert(mainStatusText() === hintsMainStatus, 'Hints validation must not overwrite the main check status');
    assert(editor.getValue() === source, 'Tool errors must not modify the Console document');
    await click(testid('console-hints-toggle'));
    await wait(() => cm('console-hints').getValue().startsWith('## accidental'));
    assert(toolStatus().textContent === hintsError, 'Reopening hints must retain its own error status');
    await click(testid('console-variables-toggle'));
    await wait(() => visible(document.querySelector('[data-testid="console-variables-editor"]')));
    assert(JSON.stringify(assertStatusBounds()) === JSON.stringify(closedStatusBounds),
      'Switching right-panel tools must not change status boundaries');
    assert(!visible(document.querySelector('[data-testid="console-hints-editor"]'))
      && document.querySelectorAll('.console-tool-panel').length === 1,
      'Variables and Hints must share one panel without overlapping editors');
    const profileTabs = () => [...find('.console-panel-tabs').querySelectorAll('[role="tab"]')];
    const writable = await wait(() => profileTabs().find(el => !el.textContent.startsWith('env ')));
    assert(profileTabs().map(el => el.querySelector('.tab-label').textContent).join(',') === 'var,env (read-only)'
      && !document.querySelector('[data-testid="console-profile-add"]'),
      'Variables must contain only fixed var and read-only env, without profile CRUD');
    await click(writable);
    const profile = writable.querySelector('.tab-label').textContent;
    const originalVars = cm('console-variables').getValue();
    assertSignalGeometry();
    cm('console-variables').setValue(`${originalVars}\nCONSOLE_PANEL_FIXTURE: saved`);
    await wait(() => profileToolLight('variables') === 'running',
      'Variables must expose its pending autosave on the tool button');
    await wait(() => toolStatus().textContent.includes('Variables save · OK')
      && profileToolLight('variables') === 'ready',
      'A visible save result must be acknowledged without clearing its status');
    cm('console-variables').setValue('missing delimiter');
    await wait(() => toolStatus().textContent.includes('ERROR')
      && toolStatus().textContent.includes('NAME: value') && profileToolLight('variables') === 'ready');
    const varsError = toolStatus().textContent;
    const varsMainStatus = mainStatusText();
    await click(testid('console-variables-toggle'));
    assert(mainStatusText() === varsMainStatus, 'Variables validation must not overwrite the main check status');
    assert(profileToolLight('variables') === 'ready', 'Collapsing Variables must not relight an already viewed validation failure');
    await click(testid('console-variables-toggle'));
    await wait(() => cm('console-variables').getValue() === 'missing delimiter');
    assert(toolStatus().textContent === varsError, 'Reopening Variables must retain its own validation error');
    await click(button(/^env /, find('.console-panel-tabs')));
    await wait(() => cm('console-variables').getOption('readOnly') === true);
    assert(profileToolLight('variables') === 'ready',
      'Selecting env must not relight a viewed error belonging to var');
    await click(button(/^var$/, find('.console-panel-tabs')));
    await wait(() => cm('console-variables').getValue() === 'missing delimiter');
    cm('console-variables').setValue(`${originalVars}\nCONSOLE_PANEL_FIXTURE: shared`);
    await wait(() => toolStatus().textContent.includes('Variables save · OK'));
    await click(button(/^env /, find('.console-panel-tabs')));
    await wait(() => cm('console-variables').getOption('readOnly') === true);
    await choose('vars');
    assert(cm('variables-editor').getOption('readOnly') === true,
      'Console variable selection must not change the legacy editor selection');
    await click(button(new RegExp(`^${profile}(?:×)?$`), find('.vars-tab-group')));
    await wait(() => cm('variables-editor').getValue().includes('CONSOLE_PANEL_FIXTURE: shared'));
    cm('variables-editor').setValue(`${originalVars}\nCONSOLE_PANEL_FIXTURE: legacy`);
    await sleep(800);
    await choose('console');
    await click(testid('console-variables-toggle'));
    await click(button(new RegExp(`^${profile}(?:×)?$`), find('.console-panel-tabs')));
    await wait(() => cm('console-variables').getValue().includes('CONSOLE_PANEL_FIXTURE: legacy'),
      'Reopening the new editor must reload changes to the shared file from the legacy editor');
    cm('console-variables').setValue(`${originalVars}\nCONSOLE_PANEL_FIXTURE: {{unknown_fixture:MISSING}}`);
    await click(button(/^env /, find('.console-panel-tabs')));
    await wait(() => cm('console-variables').getOption('readOnly') === true
      && profileToolLight('variables') === 'error',
      'An unseen background validation failure must light Variables without assigning the error to env');
    await click(testid('console-variables-toggle'));
    await click(testid('console-variables-toggle'));
    await wait(() => cm('console-variables').getOption('readOnly') === true
      && !toolStatus().classList.contains('is-running'));
    assert(profileToolLight('variables') === 'error' && !toolStatus().classList.contains('is-error'),
      'Backend validation of var must keep its aggregate error without incorrectly assigning that error to env');
    await click(button(/^var$/, find('.console-panel-tabs')));
    await wait(() => cm('console-variables').getOption('readOnly') !== true
      && toolStatus().textContent.includes('unknown_fixture') && profileToolLight('variables') === 'ready',
      'Opening the failed profile must acknowledge its signal while retaining its error status');
    await click(button(/^env /, find('.console-panel-tabs')));
    await click(testid('console-variables-toggle'));
    await click(testid('console-variables-toggle'));
    await wait(() => cm('console-variables').getOption('readOnly') === true
      && !toolStatus().classList.contains('is-running'));
    assert(profileToolLight('variables') === 'ready',
      'Background reload must not relight an unchanged validation failure that was already viewed');
    await click(button(/^var$/, find('.console-panel-tabs')));
    await wait(() => cm('console-variables').getOption('readOnly') !== true);
    cm('console-variables').setValue(originalVars);
    await wait(() => toolStatus().textContent.includes('Variables save · OK')
      && profileToolLight('variables') === 'ready',
      'Fixing var must clear its aggregate error without leaving a stale error on env');
    await click(testid('console-dictionary-toggle'));
    await wait(() => visible(document.querySelector('[data-testid="console-dictionary-editor"]')));
    assertStatusBounds();
    assert(profileTabs().every(el => !['var','env (read-only)'].includes(el.querySelector('.tab-label').textContent))
      && !visible(document.querySelector('[data-testid="console-variables-editor"]')),
      'Dictionary must be a separate tool with no var or env tabs');
    const dictionaryProfile = profileTabs().find(el => el.classList.contains('active')).querySelector('.tab-label').textContent;
    const originalDictionary = cm('console-dictionary').getValue();
    assertSignalGeometry();
    cm('console-dictionary').setValue(`${originalDictionary}\nDICTIONARY_PANEL_FIXTURE: shared`);
    await wait(() => profileToolLight('dictionary') === 'running',
      'Dictionary must expose its pending autosave independently of Variables');
    await wait(() => toolStatus().textContent.includes('Dictionary save · OK')
      && profileToolLight('dictionary') === 'ready');
    await choose('dictionary');
    await click(button(new RegExp(`^${dictionaryProfile}(?:×)?$`), find('.vars-tab-group')));
    await wait(() => cm('variables-editor').getValue().includes('DICTIONARY_PANEL_FIXTURE: shared'));
    cm('variables-editor').setValue(`${originalDictionary}\nDICTIONARY_PANEL_FIXTURE: legacy`);
    await sleep(800);
    await choose('console');
    await click(testid('console-dictionary-toggle'));
    await wait(() => cm('console-dictionary').getValue().includes('DICTIONARY_PANEL_FIXTURE: legacy'));
    const invalidDictionary = `${originalDictionary}\nDICTIONARY_PANEL_FIXTURE: {{unknown_fixture:MISSING}}`;
    cm('console-dictionary').setValue(invalidDictionary);
    await click(testid('console-variables-toggle'));
    await wait(() => visible(document.querySelector('[data-testid="console-variables-editor"]')));
    await click(button(/^var$/, find('.console-panel-tabs')));
    assert(cm('console-variables').getValue() === originalVars,
      'Dictionary edits and drafts must not alter var');
    await wait(() => profileToolLight('dictionary') === 'error');
    assert(profileToolLight('dictionary') === 'error' && profileToolLight('variables') === 'ready',
      'A hidden Dictionary error must remain visible without contaminating the Variables indicator');
    await click(testid('console-dictionary-toggle'));
    await wait(() => cm('console-dictionary').getValue() === invalidDictionary
      && toolStatus().textContent.includes('ERROR') && profileToolLight('dictionary') === 'ready',
      'Opening Dictionary must acknowledge its failure but preserve the saved request and status');
    cm('console-dictionary').setValue(originalDictionary);
    await wait(() => toolStatus().textContent.includes('Dictionary save · OK')
      && profileToolLight('dictionary') === 'ready');
    const prompt = window.prompt;
    const confirm = window.confirm;
    try {
      window.prompt = () => 'console-panel-fixture';
      await click(testid('console-profile-add'));
      await wait(() => profileTabs().some(el => el.classList.contains('active') && el.textContent.includes('console-panel-fixture')));
      const tab = profileTabs().find(el => el.textContent.includes('console-panel-fixture'));
      tab.dispatchEvent(new MouseEvent('dblclick', {bubbles:true}));
      const rename = await wait(() => document.querySelector('[data-testid="console-profile-rename"]'));
      rename.value = 'console-panel-renamed';
      rename.dispatchEvent(new InputEvent('input', {bubbles:true}));
      await key('Enter');
      await wait(() => profileTabs().some(el => el.textContent.includes('console-panel-renamed')));
      const renamed = profileTabs().find(el => el.textContent.includes('console-panel-renamed'));
      renamed.dispatchEvent(new DragEvent('dragstart', {bubbles:true}));
      await sleep(0);
      profileTabs()[0].dispatchEvent(new DragEvent('drop', {bubbles:true,cancelable:true}));
      await wait(() => toolStatus().textContent.includes('Dictionary reorder · OK')
        && profileTabs()[0].textContent.includes('console-panel-renamed'),
        'Console profile tabs must reorder through the existing native API');
      window.confirm = () => true;
      await click(profileTabs().find(el => el.textContent.includes('console-panel-renamed')).querySelector('.tab-close'));
      await wait(() => !profileTabs().some(el => el.textContent.includes('console-panel-renamed')));
      const renameDictionary = async (from, to) => {
        const tab = profileTabs().find(el => el.querySelector('.tab-label')?.textContent === from);
        assert(tab, `Dictionary profile to rename was not found: ${from}`);
        tab.dispatchEvent(new MouseEvent('dblclick', {bubbles:true}));
        const input = await wait(() => document.querySelector('[data-testid="console-profile-rename"]'));
        input.value = to;
        input.dispatchEvent(new InputEvent('input', {bubbles:true}));
        await key('Enter');
        await wait(() => profileTabs().some(el => el.querySelector('.tab-label')?.textContent === to)
          && toolStatus().textContent.includes('Dictionary rename · OK'));
      };
      const renamedProfile = 'console-no-default-fixture';
      await renameDictionary(dictionaryProfile, renamedProfile);
      assert(!profileTabs().some(el => el.querySelector('.tab-label')?.textContent === 'default'),
        'Dictionary loading regression must run without a profile named default');
      const frame = document.createElement('iframe');
      frame.src = location.href;
      frame.style.cssText = 'position:fixed;inset:0;width:100vw;height:100vh;z-index:9999';
      document.body.append(frame);
      try {
        await wait(() => frame.contentDocument?.querySelector('[data-testid="console-dictionary-toggle"]'));
        const doc = frame.contentDocument;
        const toggle = () => testid('console-dictionary-toggle', doc);
        const state = () => toggle().querySelector('.console-state-light')?.dataset.state;
        await click(toggle());
        await wait(() => doc.querySelector('[data-testid="console-dictionary-status"]')?.textContent.includes('Dictionary load · OK'));
        await wait(() => state() === 'ready',
          'A first Dictionary load without default must finish its tool signal, not leave a phantom running profile');
        assert(doc.querySelector('[data-testid="console-dictionary-status-stop"]').disabled,
          'A completed Dictionary load must not offer a running operation to stop');
        await click(toggle());
        await click(toggle());
        await wait(() => cm('console-dictionary', doc).getValue() === originalDictionary && state() === 'ready',
          'Reopening Dictionary without default must not relight a phantom loading operation');
      } finally {
        frame.remove();
        await renameDictionary(renamedProfile, dictionaryProfile);
      }
    } finally {
      window.prompt = prompt;
      window.confirm = confirm;
    }
    await click(testid('console-hints-toggle'));
    assert(cm('console-hints').getValue().startsWith('## accidental'),
      'Invalid hints drafts survive tool and legacy-interface switches');
    cm('console-hints').setValue(originalHints);
    await wait(() => toolStatus().textContent.includes('Hints save · OK'));
    cm('console-hints').setValue('{{incomplete:');
    await wait(() => toolStatus().textContent.includes('ERROR'));
    await click(testid('console-hints-toggle'));
    setSource(editor, `#check panel-status\n#endpoint\nGET ${endpoint}\n`);
    await key('Shift+Enter');
    await wait(() => find('.console-status').textContent.startsWith('Request · ')
      && find('.console-status').textContent.includes('OK'),
      'An ordinary request must use only its own status, independent of the retained tool error');
    const frame = document.createElement('iframe');
    frame.src = location.href;
    frame.style.cssText = 'position:fixed;inset:0;width:100vw;height:100vh;z-index:9999';
    document.body.append(frame);
    try {
      await wait(() => frame.contentDocument?.querySelector('[data-testid="console-hints-toggle"]'));
      await activateSession(Number(testid('console-session-name').dataset.sessionId),frame.contentDocument);
      await click(testid('console-hints-toggle', frame.contentDocument));
      await wait(() => cm('console-hints', frame.contentDocument).getValue() === '{{incomplete:',
        'An invalid hints draft must survive a new application instance');
    } finally {
      frame.remove();
    }
    await click(testid('console-hints-toggle'));
    assert(cm('console-hints').getValue() === '{{incomplete:', 'A request must not discard an invalid hints draft');
    cm('console-hints').setValue(originalHints);
    await wait(() => toolStatus().textContent.includes('Hints save · OK'));
    await click(testid('console-hints-toggle'));
    await checkConsoleMoves();
  };
  globalThis.testDriver = { assert, sleep, wait, visible, find, testid, cm, key, type, move, input,
    click, doubleClick, button,     choose, activateSession, setSource, commitRestUrl, candidates, suggest, pick, pickThenEnter, checkConsoleFolding, checkConsoleTools, assertSignalGeometry, assertNativeScrollbars,
    sessionLibrary, clearSessions: () => sessionRecord('delete') };
  globalThis.testNativeHealth = () => new Promise(resolve => {
    const port = chrome.runtime.connectNative('com.chrome_extentions.host');
    const timer = setTimeout(() => { port.disconnect(); resolve({ ok: false, error: 'timeout' }); }, 10000);
    port.onDisconnect.addListener(() => {
      const error = chrome.runtime.lastError?.message;
      clearTimeout(timer);
      resolve({ ok: false, error: error || 'disconnected' });
    });
    port.onMessage.addListener(reply => {
      clearTimeout(timer);
      resolve({ ok: reply.ok === true && reply.result?.protocolVersion === 1 });
      port.disconnect();
    });
    port.postMessage({ id: 71, type: 'health' });
  });
  const installNativeProbe = () => {
    if (!globalThis.chrome?.runtime?.connectNative) return false;
    const connect = chrome.runtime.connectNative.bind(chrome.runtime);
    globalThis.nativeProbe = {
      clipboardReads: 0, saves: 0, ports: 0, discoveries: [], chunkMessages: 0, maxMessageBytes: 0,
      clipboard: { text: "curl -X POST 'https://fixture.invalid/import' -H 'X-Fixture: imported' --data '{\"fixture\":true}'" },
    };
    chrome.runtime.connectNative = (...args) => {
      nativeProbe.ports++;
      const port = connect(...args);
      const listeners = [];
      const saves = new Set();
      const emit = message => {
        nativeProbe.maxMessageBytes = Math.max(nativeProbe.maxMessageBytes,
          new TextEncoder().encode(JSON.stringify(message)).length);
        if (message.type === 'nativeChunk') nativeProbe.chunkMessages++;
        if (saves.delete(message.id)) nativeProbe.saves++;
        for (const listener of listeners) listener(message);
      };
      port.onMessage.addListener(emit);
      return {
        onMessage: { addListener: listener => listeners.push(listener) },
        onDisconnect: port.onDisconnect,
        disconnect: () => port.disconnect(),
        postMessage: message => {
          if (message.type === 'discover')
            nativeProbe.discoveries.push({service: message.service, params: message.params});
          if (message.type === 'clipboardRead') {
            nativeProbe.clipboardReads++;
            const fixture = nativeProbe.clipboard;
            queueMicrotask(() => emit(fixture.error
              ? { id: message.id, ok: false, error: fixture.error }
              : { id: message.id, ok: true, result: { text: fixture.text } }));
          } else {
            if (['vars', 'dictionary'].includes(message.service) && message.op === 'save') saves.add(message.id);
            port.postMessage(message);
          }
        },
      };
    };
    return true;
  };
  // Child-frame extension APIs can appear after CDP's document-start hook.
  const deadline = Date.now() + 15000;
  const installWhenReady = () => {
    if (['http:', 'https:'].includes(location.protocol)
      || (location.protocol === 'about:' && window === window.top)) return;
    if (installNativeProbe()) return;
    if (Date.now() >= deadline) throw new Error(`Native API was not initialized for the browser fixture: ${location.href}`);
    setTimeout(installWhenReady, 0);
  };
  installWhenReady();
})();
