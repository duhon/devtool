// Browser test driver injected into every extension page before the application loads.
// Exposes `testDriver` (DOM/input helpers and condition waits) plus native update hooks.
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
  // Keeps the status on screen long enough to count as seen (`SEEN_AFTER_MS` in indicator.rs).
  const seen = () => sleep(2100);
  // Records application installs; with `installError` or `installResult` set, answers them instead
  // of the native host, after `deliver()` when `hold` is set.
  globalThis.updateFixture = {installs:[],installError:null,installResult:null,hold:false,deliver:null};
  // Holds native replies to requests matching `match` until `release()`, e.g. to keep a request in flight.
  const nativeHold = {match:null, ports:new Set(),
    release() { this.match = null; this.ports.forEach(release => release()); }};
  const connectNative = globalThis.chrome?.runtime?.connectNative?.bind(chrome.runtime);
  if (connectNative) chrome.runtime.connectNative = (...args) => {
    const port = connectNative(...args);
    const listeners = new Set();
    const wrapped = new Map();
    const held = new Set();
    const queue = [];
    nativeHold.ports.add(() => {
      held.clear();
      queue.splice(0).forEach(([listener, message]) => listener(message));
    });
    const addListener = port.onMessage.addListener.bind(port.onMessage);
    const removeListener = port.onMessage.removeListener.bind(port.onMessage);
    port.onMessage.addListener = listener => {
      listeners.add(listener);
      const hold = message => held.has(message?.id) ? queue.push([listener, message]) : listener(message);
      wrapped.set(listener, hold);
      addListener(hold);
    };
    port.onMessage.removeListener = listener => {
      listeners.delete(listener);
      removeListener(wrapped.get(listener) ?? listener);
      wrapped.delete(listener);
    };
    const send = port.postMessage.bind(port);
    port.postMessage = message => {
      if (message.service === 'updates' && message.op === 'install') {
        updateFixture.installs.push(message);
        if (updateFixture.installError || updateFixture.installResult) {
          const reply = updateFixture.installError
            ? {id:message.id,ok:false,error:updateFixture.installError}
            : {id:message.id,ok:true,result:updateFixture.installResult};
          const deliver = () => listeners.forEach(listener => listener(reply));
          if (updateFixture.hold) updateFixture.deliver = deliver;
          else queueMicrotask(deliver);
          return;
        }
      }
      if (nativeHold.match?.(message)) held.add(message.id);
      return send(message);
    };
    return port;
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
  const editorOf = (name, doc = document) => {
    const element = doc.querySelector(`[data-testid="${name}"]`);
    assert(element, `Editor element not found: ${name}`);
    const editor = element.editor || element.nextElementSibling?.editor;
    assert(editor, `Editor not mounted: ${name}`);
    return editor;
  };
  // The editor of `name` if it is mounted, for waits.
  const editorAt = (name, doc = document) => { try { return editorOf(name, doc); } catch { return null; } };
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
      F3: ['F3', 'F3', 114, ''],
    };
    if (/^[A-Z]$/.test(raw)) {
      const letter = parts.includes('Shift') ? raw : raw.toLowerCase();
      keys[raw] = [letter, `Key${raw}`, raw.charCodeAt(0), letter];
    }
    assert(keys[raw], `Unknown test key: ${name}`);
    const [value, code, windowsVirtualKeyCode, text] = keys[raw];
    // Mod is the platform's primary shortcut modifier: Command on macOS, Control elsewhere.
    const mod = parts.includes('Mod') ? (/Mac/.test(navigator.platform) ? 4 : 2) : 0;
    const modifiers = (parts.includes('Control') ? 2 : 0) | (parts.includes('Alt') ? 1 : 0)
      | (parts.includes('Meta') ? 4 : 0) | (parts.includes('Shift') ? 8 : 0) | mod;
    const params = { key: value, code, windowsVirtualKeyCode, modifiers };
    await input('Input.dispatchKeyEvent', { ...params, type: 'keyDown', text: modifiers && name !== 'Shift+Enter' ? '' : text });
    await input('Input.dispatchKeyEvent', { ...params, type: 'keyUp' });
  };
  const type = text => input('Input.insertText', { text });
  const move = (x, y) => input('Input.dispatchMouseEvent', { type: 'mouseMoved', x, y });
  const frame = () => new Promise(resolve => requestAnimationFrame(() => resolve()));
  // The element's box once the layout stops moving: a click lands where it was measured.
  const settled = async element => {
    let box = element.getBoundingClientRect();
    for (let tries = 0; tries < 20; tries++) {
      await frame();
      const next = element.getBoundingClientRect();
      if (next.left === box.left && next.top === box.top && next.width === box.width && next.height === box.height) break;
      box = next;
    }
    return box;
  };
  const click = async (element, x, y, clickCount = 1) => {
    element.scrollIntoView({ block: 'nearest' });
    const box = await settled(element);
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
  // Waits until the App allows switching sessions; on timeout it reports what kept it busy.
  const sessionsIdle = async (root = document) => {
    const state = () => root.querySelector('[data-testid="console-session-name"]')?.dataset ?? {};
    await wait(() => state().busy === 'false',
      () => `Sessions must become idle; busy because: ${state().busyReason}`);
  };
  const activateSession = async (id, root = document) => {
    await wait(() => visible(root.querySelector('[data-testid="console-session-name"]')),
      'Console session controls must finish asynchronous startup');
    if (testid('console-session-name',root).dataset.sessionId === String(id)) return;
    // The toolbar may still shift while panel modules load: a click that missed is repeated.
    await wait(async () => {
      const toggle=testid('console-sessions-toggle',root);
      if(toggle.getAttribute('aria-pressed')==='true') return true;
      await click(toggle);
      await sleep(200);
      return toggle.getAttribute('aria-pressed')==='true';
    }, 'The Sessions panel must open');
    await sessionsIdle(root);
    const switcher = await wait(() => [...root.querySelectorAll(`[data-testid="session-switch-${id}"]`)].find(visible),
      () => `Session ${id} must be listed: ${root.querySelector(`[data-testid="sessions-panel"]`)?.innerText.slice(0,300)}`);
    await click(switcher);
    await wait(()=>testid('console-session-name',root).dataset.sessionId===String(id),
      ()=>`Session ${id} must become current: ${root.querySelector(`[data-testid="sessions-panel"]`)?.innerText.slice(0,300)}`);
  };
  let sessionInitialized=false;
  const openConsole = async () => {
    if(!sessionInitialized) {
      // Scenarios work in imported session 1 unless they pick another one via testDriver.session.
      await activateSession(globalThis.testDriver.session ?? 1);
      sessionInitialized=true;
    }
    await wait(() => [...document.querySelectorAll('.console-root .editor')].some(visible));
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
  const candidates = (doc = document) => [...doc.querySelectorAll('.editor')]
    .filter(visible).flatMap(el => (el.editor.state.completionActive?.data?.list || [])
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
  // Types `value` into a module UI field and commits it (its `on_input`, then `on_change`).
  const fill = (field, value) => {
    field.value = value;
    field.dispatchEvent(new InputEvent('input', {bubbles:true}));
    field.dispatchEvent(new Event('change', {bubbles:true}));
  };
  // Picks `value` in a module UI select.
  const choose = (select, value) => {
    assert([...select.options].some(option => option.value === value), `No option ${value} in ${select.dataset.testid}`);
    select.value = value;
    select.dispatchEvent(new Event('change', {bubbles:true}));
  };
  // Opens the side panel of the module `id` and waits for its UI.
  const openPanel = async (id, frame = `${id}-panel`) => {
    const toggle = testid(`console-${id}-toggle`);
    if (toggle.getAttribute('aria-pressed') !== 'true') await click(toggle);
    return wait(() => [...document.querySelectorAll(`[data-testid="${frame}"]`)].find(visible), `Panel ${id} must open`);
  };
  const closePanel = async id => {
    const toggle = testid(`console-${id}-toggle`);
    if (toggle.getAttribute('aria-pressed') === 'true') await click(toggle);
    await wait(() => toggle.getAttribute('aria-pressed') !== 'true', `Panel ${id} must close`);
  };
  // Opens Settings on one of its tabs: modules, sources or agent.
  const settingsTab = async name => {
    await openPanel('settings', 'settings');
    const tab = await wait(() => document.querySelector(`[data-testid="settings-tab-${name}"]`), `Settings tab ${name}`);
    if (tab.getAttribute('aria-selected') !== 'true') await click(tab);
    await wait(() => visible(document.querySelector(`[data-testid="settings-tab-${name}-body"]`)),
      `Settings tab ${name} must show its group`);
  };
  // Adds a folder source in the open Settings panel, then returns to the tab shown before.
  const addFolderSource = async (name, path) => {
    const previous = document.querySelector('[data-testid="settings-tabs"] [aria-selected="true"]')?.dataset.testid.slice('settings-tab-'.length);
    await settingsTab('sources');
    await click(testid('source-add'));
    fill(await wait(() => document.querySelector('[data-testid="source-add-name"]'), 'Source form'), name);
    choose(testid('source-add-kind'), 'folder');
    await wait(() => document.querySelector('[data-testid="source-add-path"]') && !document.querySelector('[data-testid="source-add-repository"]'),
      'A folder source asks only for its folder');
    fill(testid('source-add-path'), path);
    await settle();
    await click(testid('source-add-save'));
    if (previous && previous !== 'sources') await settingsTab(previous);
  };
  globalThis.testDriver = { assert, sleep, seen, wait, waitFor, during, holds, settle, stable, frames, visible, find, testid, editorOf, editorAt, key, type, move, input,
    click, doubleClick, button, openConsole, activateSession, sessionsIdle, setSource, commitRestUrl, candidates, suggest, pick, pickThenEnter,
    assertNativeScrollbars, nativeTrackPaint, sessionLibrary, nativeHold, settingsTab, addFolderSource, fill, choose, openPanel, closePanel, clearSessions: () => sessionRecord('delete') };
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
      clipboardReads: 0, saves: 0, ports: 0, discoveries: [], discoveryRuns: new Set(), chunkMessages: 0, maxMessageBytes: 0,
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
          // A Kernel module discovers through driver calls tagged `discover:{run}:{n}`: one run
          // is one discovery, whatever number of candidate documents it tries.
          const run = message.service === 'drivers'
            && /^discover:([^:]+):/.exec(message.tag || '')?.[1];
          if (run && !nativeProbe.discoveryRuns.has(`${message.module}:${run}`)) {
            nativeProbe.discoveryRuns.add(`${message.module}:${run}`);
            nativeProbe.discoveries.push({service: message.module, params: {url: message.request?.url}});
          }
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
