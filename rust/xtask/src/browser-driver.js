// Browser test driver injected into every extension page before the application loads.
// Exposes `testDriver` (DOM/input helpers and condition waits) plus native/update fetch hooks.
// Scenario scripts must wait for conditions with `waitFor`/`wait`; `sleep` is only for timed behaviour under test.
(() => {
  let next = 0;
  const pending = new Map();
  globalThis.__testResolve = (id,result) => { pending.get(id)(result); pending.delete(id); };
  const input = (method, params) => new Promise(resolve => {
    const id = ++next;
    pending.set(id, resolve);
    __testInput(JSON.stringify({ id, method, params }));
  });
  const assert = (value, message) => { if (!value) throw new Error(typeof message === 'function' ? message() : message); };
  const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
  globalThis.updateFixture = {release:null,error:null,requests:[],installs:[],branch:'devtool'};
  const connectNative = globalThis.chrome?.runtime?.connectNative?.bind(chrome.runtime);
  if (connectNative) chrome.runtime.connectNative = (...args) => {
    const port = connectNative(...args);
    const listeners = new Set();
    const addListener = port.onMessage.addListener.bind(port.onMessage);
    const removeListener = port.onMessage.removeListener.bind(port.onMessage);
    port.onMessage.addListener = listener => {
      listeners.add(listener);
      addListener(listener);
    };
    port.onMessage.removeListener = listener => {
      listeners.delete(listener);
      removeListener(listener);
    };
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
        if (updateFixture.installError || updateFixture.installResult) {
          const reply = updateFixture.installError
            ? {id:message.id,ok:false,error:updateFixture.installError}
            : {id:message.id,ok:true,result:updateFixture.installResult};
          queueMicrotask(() => listeners.forEach(listener => listener(reply)));
          return;
        }
        if (updateFixture.archive)
          message = {...message, archivePath:updateFixture.archive};
      }
      return send(message);
    };
    return port;
  };
  const fetchOriginal = globalThis.fetch.bind(globalThis);
  globalThis.fetch = async (input, options) => {
    const url = typeof input === 'string' ? input : input.url;
    const match = /^https:\/\/github\.com\/([^/]+\/[^/]+)\/releases\/latest\/download\/version\.json$/.exec(url);
    if (!match) return fetchOriginal(input, options);
    assert(input.credentials === 'include' && input.cache === 'no-store',
      'Private release checks must use browser-managed credentials and bypass caches');
    updateFixture.requests.push(url);
    if (updateFixture.pauseCheck)
      await new Promise(resolve => { updateFixture.resumeCheck = resolve; });
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
  const describe = message => typeof message === 'function' ? message() : message;
  // Polls until the predicate is truthy; the failure names what was awaited.
  const waitFor = async (check, { timeout = 15000, message = 'Browser condition', interval = 25 } = {}) => {
    const deadline = Date.now() + timeout;
    while (Date.now() < deadline) {
      const value = await check();
      if (value) return value;
      await sleep(interval);
    }
    const value = await check();
    if (value) return value;
    throw new Error(`Timed out after ${timeout}ms waiting for: ${describe(message)}`);
  };
  const wait = (check, message = 'Browser condition', timeout = 15000) => waitFor(check, { message, timeout });
  const frames = (count = 2) => new Promise(resolve => {
    const step = left => left ? requestAnimationFrame(() => step(left - 1)) : setTimeout(resolve, 0);
    step(count);
  });
  // Lets queued input handlers, microtasks, zero-delay timers and the next paints run.
  const settle = () => frames(2);
  // Waits until a layout measurement stops changing across consecutive frames.
  const stable = async (measure, { frames: needed = 3, timeout = 5000, message = 'Stable layout' } = {}) => {
    let previous, same = 0;
    return waitFor(async () => {
      await frames(1);
      const current = JSON.stringify(await measure());
      same = current === previous ? same + 1 : 0;
      previous = current;
      return same >= needed && JSON.parse(current);
    }, { timeout, message: () => `${describe(message)}: last ${previous}` });
  };
  // Re-checks an assertion throughout a time window; used only when the window itself is the
  // behaviour under test (no timer/debounce/retry may fire during it). Fails as soon as it breaks.
  const during = async (ms, check) => {
    assert(Number.isFinite(ms) && ms > 0, 'during() needs an explicit window');
    const end = Date.now() + ms;
    do {
      await check();
      await sleep(Math.min(20, Math.max(0, end - Date.now())));
    } while (Date.now() < end);
    await check();
  };
  const holds = (check, message, ms) => during(ms, async () => assert(await check(), describe(message)));
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
    await input('Input.dispatchKeyEvent', { ...params, type: 'keyDown', text: modifiers && name !== 'Shift+Enter' ? '' : text });
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
  const openConsole = async () => {
    if(!sessionInitialized) {
      // Scenarios work in imported session 1 unless they pick another one via testDriver.session.
      await activateSession(globalThis.testDriver.session ?? 1);
      sessionInitialized=true;
    }
    await wait(() => [...document.querySelectorAll('.console-root .CodeMirror')].some(visible));
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
  globalThis.testDriver = { assert, sleep, wait, waitFor, during, holds, settle, stable, frames, visible, find, testid, cm, key, type, move, input,
    click, doubleClick, button, openConsole, activateSession, setSource, commitRestUrl, candidates, suggest, pick, pickThenEnter,
    assertNativeScrollbars, nativeTrackPaint, sessionLibrary, clearSessions: () => sessionRecord('delete') };
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
