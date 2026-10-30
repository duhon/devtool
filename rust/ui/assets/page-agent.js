(args => {
  const fail = message => { throw new Error(message); };
  const guideKey = '__servicesDevtoolsPageGuide';
  const stateKey = '__servicesDevtoolsPageAccess';
  const maxBytes = 6000;
  const bytes = value => new TextEncoder().encode(JSON.stringify(value)).length;
  const clip = (value, limit) => {
    const text = String(value).toWellFormed();
    let end = Math.min(text.length, limit);
    if (end && /[\uD800-\uDBFF]/.test(text[end - 1])) end--;
    return text.slice(0, end);
  };
  if (args.tool === 'access_cleanup') {
    const access = window[stateKey]?.get(args.accessId);
    if (access) access.dispose();
    return {cleared: !!access};
  }
  if (args.tool === 'guide_cleanup') {
    const guide = window[guideKey];
    if (!guide || guide.id !== args.guideId) return {cleared: false};
    guide.dispose();
    delete window[guideKey];
    return {cleared: true};
  }
  if (!['http:', 'https:'].includes(location.protocol))
    fail('Page tools are limited to inspected HTTP(S) pages.');
  const info = () => ({
    origin: location.origin,
    url: clip(location.origin + location.pathname, 600),
    title: clip(document.title, 200),
    pageId: String(performance.timeOrigin),
  });
  if (args.tool === 'metadata') return info();
  if (args.origin !== location.origin) fail('Page origin changed; new approval is required.');
  if ((args.tool !== 'page_snapshot' || args.snapshotId) && args.pageId !== info().pageId)
    fail('The document changed. Read a fresh page snapshot before acting.');
  if (args.tool === 'guide_status') {
    const guide = window[guideKey];
    if (!guide || guide.id !== args.guideId) return {...info(), action: 'missing'};
    guide.lastSeen = performance.now();
    guide.update();
    return {...info(), action: guide.action || 'waiting', untrusted: true};
  }
  const current = element => {
    if (!element.isConnected) return false;
    for (let doc = element.ownerDocument; doc !== document;) {
      const frame = doc.defaultView?.frameElement;
      if (!frame?.isConnected || frame.contentDocument !== doc) return false;
      doc = frame.ownerDocument;
    }
    return true;
  };
  const visible = element => {
    if (!current(element)) return false;
    const style = element.ownerDocument.defaultView.getComputedStyle(element);
    if (style.visibility === 'hidden' || style.display === 'none'
      || element.closest('[hidden],[aria-hidden="true"],[inert]')) return false;
    let box = element.getBoundingClientRect();
    if (style.display === 'contents') {
      const range = element.ownerDocument.createRange();
      range.selectNodeContents(element); box = range.getBoundingClientRect();
    }
    const host = element.getRootNode().host;
    const frame = element.ownerDocument.defaultView.frameElement;
    return box.width > 0 && box.height > 0 && (!host || visible(host)) && (!frame || visible(frame));
  };
  const privateText = 'input,textarea,select,script,style,noscript,[contenteditable],[role="textbox"],[role="searchbox"]';
  const privateNode = (element, pattern = privateText) => {
    for (let node = element; node; node = node.getRootNode().host) {
      if (node.closest(pattern) || node.isContentEditable) return true;
    }
    return false;
  };
  const textOf = (root, limit, maxNodes) => {
    const walker = root.ownerDocument.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    let text = '', scanned = 0;
    while (walker.nextNode() && text.length < limit && scanned++ < maxNodes) {
      const parent = walker.currentNode.parentElement;
      if (!parent || privateNode(parent) || !visible(parent)) continue;
      const value = walker.currentNode.textContent.trim();
      if (value) text += value + '\n';
    }
    return clip(text, limit).trim();
  };
  if (typeof args.accessId !== 'string' || !args.accessId) fail('Page access identity is missing.');
  if (!window[stateKey]) window[stateKey] = new Map();
  const accesses = window[stateKey];
  if (!(accesses instanceof Map)) fail('The page uses the reserved access identity.');
  let access = accesses.get(args.accessId);
  if (!access) {
    const listeners = [];
    access = {snapshots: new Map(), nodes: new Map(), ids: new WeakMap(), next: 0, nextSnapshot: 0,
      errors: [], droppedErrors: 0, pointer: null, documents: new WeakSet(),
      dispose() {
        clearTimeout(this.timer);
        for (const [target, type, listener] of listeners) target.removeEventListener(type, listener, true);
        accesses.delete(args.accessId);
        if (!accesses.size) delete window[stateKey];
      },
      observe(doc) {
        if (this.documents.has(doc)) return;
        this.documents.add(doc);
        const listen = (target, type, listener) => {
          target.addEventListener(type, listener, true);
          listeners.push([target, type, listener]);
        };
        listen(doc, 'pointermove', event => {
          const element = event.composedPath().find(node => node.nodeType === 1);
          if (!element || element.getRootNode().host?.id === 'services-devtools-page-guide') return;
          this.pointer = {element: new WeakRef(element),
            x: event.clientX, y: event.clientY, at: Date.now(), inside: true};
        });
        listen(doc, 'pointerleave', () => { if (this.pointer) this.pointer.inside = false; });
        const record = (kind, message) => {
          this.errors.push({sequence: this.droppedErrors + this.errors.length, kind,
            message: clip(message, 800), at: new Date().toISOString()});
          if (this.errors.length > 256) { this.errors.shift(); this.droppedErrors++; }
        };
        listen(doc.defaultView, 'error', event => record('error',
          event.message || `Resource load failed: ${event.target?.localName || 'unknown'}`));
        listen(doc.defaultView, 'unhandledrejection', event => record('unhandledrejection',
          typeof event.reason === 'string' ? event.reason : event.reason?.message || 'Non-text rejection'));
      }};
    accesses.set(args.accessId, access);
  }
  clearTimeout(access.timer);
  access.timer = setTimeout(() => access.dispose(), 300000);
  access.observe(document);
  const selector = element => {
    if (element.id && element.id.isWellFormed()) {
      const id = '#' + CSS.escape(element.id);
      if (element.getRootNode() === document && id.length <= 160
        && document.querySelectorAll(id).length === 1) return id;
    }
    if (!access.ids.has(element)) {
      const id = `@node-${++access.next}`;
      access.ids.set(element, id);
      access.nodes.set(id, new WeakRef(element));
    }
    return access.ids.get(element);
  };
  const find = (value, optional = false) => {
    if (typeof value !== 'string' || !value || value.length > 1024)
      fail('A selector from a fresh page read is required.');
    const matches = value.startsWith('@node-')
      ? [access.nodes.get(value)?.deref()].filter(node => node && current(node))
      : [...document.querySelectorAll(value)];
    if (optional && !matches.length) return null;
    if (matches.length !== 1) fail('The selector must match exactly one current element. Read fresh page state.');
    return matches[0];
  };
  const integer = (name, fallback, maximum = Number.MAX_SAFE_INTEGER) => {
    const value = args[name] === undefined ? fallback : args[name];
    if (!Number.isSafeInteger(value) || value < 0 || value > maximum) fail(`Invalid ${name}.`);
    return value;
  };
  const control = 'button,a[href],input,textarea,select,summary,[role="button"],[role="link"],[role="checkbox"],[role="radio"],[role="tab"],[contenteditable="true"]';
  const label = element => element.getAttribute('aria-label')
    || [...element.labels || []].map(node => textOf(node, Infinity, Infinity)).join(' ')
    || element.getAttribute('placeholder') || textOf(element, Infinity, Infinity)
    || element.getAttribute('title') || '';
  const describe = element => ({
    selector: selector(element), tag: element.localName, type: clip(element.getAttribute('type') || '', 40) || null,
    role: clip(element.getAttribute('role') || '', 80) || null, label: clip(label(element), 200),
    labelTruncated: label(element).length > 200,
    disabled: element.matches(':disabled,[aria-disabled="true"]'),
    readOnly: element.readOnly === true,
    ...(['checkbox', 'radio'].includes(element.type) ? {checked: element.checked} : {}),
    ...(element.localName === 'select' ? {optionsCount: element.options.length} : {}),
  });
  const collect = root => {
    const parts = [], segments = [], elements = [], labels = [], frames = [];
    let length = 0;
    const stack = [root];
    while (stack.length) {
      const node = stack.pop();
      if (node.nodeType === 3) {
        const parent = node.parentElement;
        if (!parent || privateNode(parent) || !visible(parent)) continue;
        const text = node.textContent.toWellFormed().trim();
        if (text) {
          segments.push({start: length, end: length + text.length, selector: selector(parent)});
          parts.push(text + '\n'); length += text.length + 1;
        }
        continue;
      }
      if (node.nodeType === 1) {
        if (node.id === 'services-devtools-page-guide' || node.closest('[hidden],[aria-hidden="true"],[inert]')
          || node.ownerDocument.defaultView.getComputedStyle(node).display === 'none') continue;
        if (node.matches(control) && node.type !== 'hidden' && visible(node)) {
          elements.push(describe(node)); labels.push(label(node));
        }
        if (privateNode(node)) continue;
        if (node.localName === 'iframe' && visible(node)) {
          if (node.contentDocument?.documentElement) {
            access.observe(node.contentDocument);
            stack.push(node.contentDocument.documentElement);
          } else frames.push({selector: selector(node), accessible: false});
        }
        if (node.shadowRoot) stack.push(node.shadowRoot);
      }
      const children = [...node.childNodes];
      for (let index = children.length - 1; index >= 0; index--) stack.push(children[index]);
    }
    return {text: parts.join(''), segments, elements, labels, frames};
  };
  const snapshot = () => {
    if (args.source !== undefined && !['text', 'label'].includes(args.source)) fail('Unknown page read source.');
    if (args.snapshotId !== undefined) {
      if (typeof args.snapshotId !== 'string') fail('snapshotId must be a returned string.');
      const saved = access.snapshots.get(args.snapshotId);
      if (!saved) fail('This snapshot expired or was evicted. Read a fresh snapshot; do not skip unread content.');
      if (saved.kind !== 'page') fail('This snapshot belongs to DOM inspection, not a rendered page read.');
      if (args.selector !== undefined && args.selector !== saved.scope)
        fail('The snapshot belongs to another scope. Continue its original selector or read a fresh snapshot.');
      if (args.source !== undefined && args.source !== saved.source)
        fail('The snapshot belongs to another read source.');
      return saved;
    }
    if (integer('textOffset', 0) || integer('elementOffset', 0))
      fail('Continuation offsets require the returned snapshotId and pageId.');
    const root = args.selector ? find(args.selector) : document.body || document.documentElement;
    if (args.source !== 'label' && root.localName !== 'option' && privateNode(root))
      fail('Field contents and executable sources are not exposed.');
    const explicitText = root.localName === 'option' ? root.label.toWellFormed()
      : args.source === 'label' ? label(root).toWellFormed() : null;
    const content = explicitText === null ? collect(root) : {text: explicitText,
      segments: [{start: 0, end: explicitText.length, selector: selector(root)}],
      elements: [], labels: [], frames: []};
    const saved = {...content, kind: 'page', id: String(++access.nextSnapshot), scope: args.selector ?? null, source: args.source || 'text'};
    access.snapshots.set(saved.id, saved);
    if (access.snapshots.size > 4) access.snapshots.delete(access.snapshots.keys().next().value);
    return saved;
  };
  const base = () => ({...info(), untrusted: true});
  if (args.tool === 'page_inspect') {
    let saved;
    if (args.snapshotId !== undefined) {
      if (typeof args.snapshotId !== 'string') fail('snapshotId must be a returned string.');
      saved = access.snapshots.get(args.snapshotId);
      if (!saved) fail('This DOM snapshot expired or was evicted. Start a fresh inspection.');
      if (saved.kind !== 'dom') fail('This snapshot belongs to a rendered page read, not DOM inspection.');
      if (args.selector !== undefined && args.selector !== saved.scope)
        fail('This DOM snapshot belongs to another selector.');
    } else if (integer('offset', 0)) fail('DOM continuation requires its snapshotId.');
    const mode = args.mode ?? saved?.mode ?? 'tree';
    if (!['tree', 'node'].includes(mode)) fail('DOM mode must be tree or node.');
    if (saved && mode !== saved.mode) fail('This DOM snapshot belongs to another inspection mode.');
    if (!saved) {
      const root = args.selector ? find(args.selector) : document.documentElement;
      const properties = element => {
        const result = {};
        for (const name of ['value', 'defaultValue', 'checked', 'defaultChecked', 'indeterminate',
          'selected', 'selectedIndex', 'disabled', 'readOnly', 'required', 'multiple', 'open']) {
          const value = element[name];
          if (['string', 'number', 'boolean'].includes(typeof value)) result[name] = value;
        }
        return result;
      };
      const frames = [], items = [];
      let data = null;
      if (mode === 'node') {
        data = JSON.stringify({
          selector: selector(root), tag: root.localName, namespace: root.namespaceURI,
          attributes: Object.fromEntries([...root.attributes].map(attribute => [attribute.name, attribute.value])),
          properties: properties(root), textContent: root.textContent,
          childNodes: [...root.childNodes].map(node => node.nodeType === 1
            ? {nodeType: 1, selector: selector(node)}
            : {nodeType: node.nodeType, name: node.nodeName, text: node.textContent}),
          parent: root.parentElement ? selector(root.parentElement) : null,
          children: [...root.children].map(element => selector(element)),
          shadowChildren: root.shadowRoot ? [...root.shadowRoot.children].map(element => selector(element)) : [],
          frameRoot: root.localName === 'iframe' && root.contentDocument?.documentElement
            ? selector(root.contentDocument.documentElement) : null,
          visible: visible(root),
        });
      } else {
        const stack = [{element: root, parent: null, relation: 'root'}];
        while (stack.length) {
          const {element, parent, relation} = stack.pop();
          if (element.id === 'services-devtools-page-guide') continue;
          const id = selector(element), state = properties(element);
          const value = typeof state.value === 'string' ? state.value : null;
          const text = [...element.childNodes].filter(node => node.nodeType === 3)
            .map(node => node.textContent).join('');
          items.push({
            selector: id, parent, relation, tag: element.localName, visible: visible(element),
            ...(element.id ? {id: clip(element.id, 100)} : {}),
            ...(element.getAttribute('name') ? {name: clip(element.getAttribute('name'), 100)} : {}),
            ...(element.getAttribute('type') ? {type: clip(element.getAttribute('type'), 40)} : {}),
            ...(value !== null ? {value: clip(value, 160), valueLength: value.length,
              valueTruncated: value.length > 160} : {}),
            ...(typeof state.value === 'number' ? {value: state.value} : {}),
            ...Object.fromEntries(Object.entries(state).filter(([name]) =>
              ['checked', 'selected', 'selectedIndex', 'disabled', 'readOnly'].includes(name))),
            text: clip(text, 120), ...(text.length > 120 ? {textTruncated: true} : {}),
          });
          if (element.localName === 'iframe') {
            if (element.contentDocument?.documentElement) {
              access.observe(element.contentDocument);
              stack.push({element: element.contentDocument.documentElement, parent: id, relation: 'frame'});
            } else frames.push(id);
          }
          if (element.shadowRoot) {
            const children = [...element.shadowRoot.children];
            for (let index = children.length - 1; index >= 0; index--)
              stack.push({element: children[index], parent: id, relation: 'shadow'});
          }
          const children = [...element.children];
          for (let index = children.length - 1; index >= 0; index--)
            stack.push({element: children[index], parent: id, relation: 'child'});
        }
      }
      saved = {kind: 'dom', mode, scope: args.selector ?? null, id: String(++access.nextSnapshot),
        items, data, frames};
      access.snapshots.set(saved.id, saved);
      if (access.snapshots.size > 4) access.snapshots.delete(access.snapshots.keys().next().value);
    }
    const offset = integer('offset', 0), limit = integer('limit', 50, 100);
    if (!limit) fail('DOM limit must be positive.');
    const total = mode === 'tree' ? saved.items.length : saved.data.length;
    if (offset > total) fail('DOM offset is out of range.');
    const result = {...base(), mode, snapshotId: saved.id, offset, total, nextOffset: null,
      offsetUnit: mode === 'tree' ? 'nodes' : 'utf16', inaccessibleFrames: saved.frames.length,
      limitation: 'Open shadow DOM and same-origin frames only; no closed shadow roots, cross-origin frame DOM or canvas pixels. Tree values/text are previews; mode node returns complete attributes, properties and text as paginated JSON data.'};
    let end = offset;
    if (mode === 'tree') {
      result.items = [];
      for (; end < total && result.items.length < limit; end++) {
        result.items.push(saved.items[end]);
        if (bytes(result) > maxBytes - 40) { result.items.pop(); break; }
      }
    } else {
      if (offset && /[\uD800-\uDBFF]/.test(saved.data[offset - 1])
        && /[\uDC00-\uDFFF]/.test(saved.data[offset]))
        fail('DOM offset splits a Unicode character. Use the returned nextOffset.');
      result.data = '';
      for (const character of saved.data.slice(offset)) {
        result.data += character;
        if (bytes(result) > maxBytes - 40) { result.data = result.data.slice(0, -character.length); break; }
        end += character.length;
      }
    }
    if (offset < total && end === offset) fail('This DOM item exceeds the reply budget.');
    result.nextOffset = end < total ? end : null;
    return result;
  }
  const pageItems = (items, extra = {}) => {
    const offset = integer('offset', 0), limit = integer('limit', 20, 100);
    if (!limit) fail('Invalid page range.');
    const result = {...base(), ...extra, offset, total: 0, items: [], nextOffset: 0};
    let total = 0, full = false;
    for (const item of items) {
      if (total >= offset && !full && result.items.length < limit) {
        result.items.push(item);
        if (bytes(result) > maxBytes - 40) { result.items.pop(); full = true; }
      }
      total++;
    }
    if (offset > total) fail('Invalid page range.');
    result.total = total;
    result.nextOffset = offset + result.items.length < total ? offset + result.items.length : null;
    if (offset < total && !result.items.length) fail('This item exceeds the page budget; read its element as text.');
    return result;
  };
  if (args.tool === 'page_snapshot' || args.tool === 'page_read') {
    const saved = snapshot();
    const startText = integer('textOffset', 0), startElement = integer('elementOffset', 0);
    if (startText > saved.text.length || startElement > saved.elements.length) fail('Snapshot offset is out of range.');
    if (startText && /[\uD800-\uDBFF]/.test(saved.text[startText - 1])
      && /[\uDC00-\uDFFF]/.test(saved.text[startText]))
      fail('The text offset splits a Unicode character. Use the returned continuation offsets.');
    const result = {...base(), snapshotId: saved.id, text: '', elements: [],
      textOffset: startText, elementOffset: startElement,
      totalTextChars: saved.text.length, totalElements: saved.elements.length,
      nextTextOffset: startText < saved.text.length ? startText : null,
      nextElementOffset: startElement < saved.elements.length ? startElement : null,
      viewport: {width: innerWidth, height: innerHeight, x: scrollX, y: scrollY},
      inaccessibleFrames: saved.frames.length,
      limitations: ['Rendered DOM including open shadow roots and same-origin frames, not closed roots or canvas pixels.',
        'Field contents, cookies and storage are excluded. Virtualized content requires scrolling and a fresh snapshot.',
        'Continue this frozen snapshot using both returned offsets; a fresh snapshot may reflect different content.']};
    let end = startText;
    for (const character of saved.text.slice(startText)) {
      result.text += character;
      if (bytes(result) > 3300) { result.text = result.text.slice(0, -character.length); break; }
      end += character.length;
    }
    result.nextTextOffset = end < saved.text.length ? end : null;
    for (let index = startElement; index < saved.elements.length; index++) {
      result.elements.push(saved.elements[index]);
      result.nextElementOffset = index + 1 < saved.elements.length ? index + 1 : null;
      if (bytes(result) > maxBytes) { result.elements.pop(); result.nextElementOffset = index; break; }
    }
    return result;
  }
  if (args.tool === 'page_search') {
    if (typeof args.query !== 'string' || !args.query.trim() || args.query.length > 200) fail('A nonempty query of at most 200 characters is required.');
    const saved = snapshot();
    const pattern = args.query.trim().split(/\s+/).map(part => part.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('\\s+');
    const matches = function* () {
      let segment = 0;
      for (const match of saved.text.matchAll(new RegExp(pattern, 'giu'))) {
        while (segment + 1 < saved.segments.length && saved.segments[segment].end <= match.index) segment++;
        yield {kind: 'text', selector: saved.segments[segment].selector, textOffset: match.index,
          excerpt: clip(saved.text.slice(Math.max(0, match.index - 60), match.index + match[0].length + 120), 400)};
      }
      const expression = new RegExp(pattern, 'iu');
      for (let index = 0; index < saved.elements.length; index++) {
        if (expression.test(saved.labels[index])) yield {kind: 'control', ...saved.elements[index]};
      }
    };
    return pageItems(matches(), {snapshotId: saved.id});
  }
  if (args.tool === 'page_options') {
    const element = find(args.selector);
    if (element.localName !== 'select') fail('Options require a select control.');
    return pageItems([...element.options].map((option, index) => ({index, selector: selector(option),
      label: clip(option.label, 500), labelTruncated: option.label.length > 500,
      disabled: option.disabled || option.parentElement.disabled === true})));
  }
  if (args.tool === 'page_context') {
    const selected = typeof $0 !== 'undefined' && $0?.nodeType === 1 ? $0 : null;
    const hovered = access.pointer?.element.deref();
    return {...base(), selected: selected?.isConnected && visible(selected) ? describe(selected) : null,
      pointer: hovered?.isConnected && visible(hovered) ? {element: describe(hovered),
        x: access.pointer.x, y: access.pointer.y, ageMs: Date.now() - access.pointer.at,
        inside: access.pointer.inside} : null,
      limitation: 'Pointer tracking starts with the first page access. Null means unavailable, not no error.'};
  }
  if (args.tool === 'page_errors') {
    return pageItems(access.errors, {droppedBefore: access.droppedErrors,
      limitation: 'Only errors/unhandled rejections observed since first page access, not past Console history. Messages are bounded to 800 characters.'});
  }
  if (args.tool === 'page_wait') {
    const element = find(args.selector, true);
    const state = args.state || 'visible';
    if (!['visible', 'hidden', 'enabled', 'text'].includes(state)) fail('Unknown wait state.');
    if (state === 'text' && (typeof args.text !== 'string' || !args.text || args.text.length > 200))
      fail('Text waits require at most 200 nonempty characters.');
    const shown = !!element && visible(element);
    if (state === 'text' && element && privateNode(element))
      fail('Field contents cannot be inspected by text waits.');
    const matched = state === 'hidden' ? !shown : state === 'visible' ? shown
      : state === 'enabled' ? shown && !element.matches(':disabled,[aria-disabled="true"]')
      : shown && collect(element).text.includes(args.text);
    return {...base(), matched, state};
  }
  if (args.tool === 'page_scroll') {
    const x = args.x === undefined ? 0 : args.x;
    if (!Number.isInteger(x) || !Number.isInteger(args.y)
      || Math.abs(x) > 2000 || Math.abs(args.y) > 2000) fail('Scroll deltas must be integers within 2000 pixels.');
    const target = args.selector ? find(args.selector) : window;
    target.scrollBy({left: x, top: args.y, behavior: 'instant'});
    return {...base(), x: target === window ? scrollX : target.scrollLeft,
      y: target === window ? scrollY : target.scrollTop};
  }
  if (!['page_click', 'page_fill', 'page_select', 'page_focus', 'page_set_checked', 'page_press_key', 'page_guide_show'].includes(args.tool)) fail('Unknown page tool.');
  const element = find(args.selector);
  if (!visible(element) || element.matches(':disabled,[aria-disabled="true"]')) fail('The target is hidden or disabled.');
  const owner = element.ownerDocument.defaultView;
  if (args.tool === 'page_guide_show') {
    if (element.ownerDocument !== document) fail('Guides currently require a target in the main document.');
    if (typeof args.guideId !== 'string' || !args.guideId || args.guideId.length > 100)
      fail('A controller-owned guide identity is required.');
    if (typeof args.title !== 'string' || !args.title.trim() || args.title.length > 120
      || typeof args.text !== 'string' || !args.text.trim() || args.text.length > 1200)
      fail('Guide title and text must be nonempty plain text within 120 and 1200 characters.');
    if (!Number.isInteger(args.step) || args.step < 1 || args.step > 1000
      || (args.total !== undefined && (!Number.isInteger(args.total) || args.total < args.step || args.total > 1000)))
      fail('Guide steps must be integers from 1 to 1000; total cannot be less than step.');
    const previous = window[guideKey];
    if (previous?.action) {
      previous.dispose();
      delete window[guideKey];
    } else if (previous) fail('Another page guide is already active.');
    const host = document.createElement('div');
    host.id = 'services-devtools-page-guide';
    if (document.getElementById(host.id)) fail('The page already uses the guide container identity.');
    const root = host.attachShadow({mode: 'open'});
    const sheet = new CSSStyleSheet();
    sheet.replaceSync(`
      :host { all: initial !important; position: fixed !important; inset: 0 !important;
        width: 100vw !important; height: 100vh !important; margin: 0 !important;
        padding: 0 !important; border: 0 !important; overflow: visible !important;
        background: transparent !important; pointer-events: none !important; z-index: 2147483647 !important; }
      * { box-sizing: border-box; }
      .shade { position: fixed; background: rgb(0 0 0 / 38%); pointer-events: none; }
      .frame { position: fixed; border: 3px solid #ffd84d; border-radius: 8px;
        box-shadow: 0 0 0 1px #171717; pointer-events: none; }
      .bubble { position: fixed; width: min(360px, calc(100vw - 24px)); max-height: calc(100vh - 24px);
        overflow: auto; padding: 16px; background: #fff8cf; color: #171717; border: 3px solid #171717;
        border-radius: 24px 22px 24px 8px; box-shadow: 6px 7px 0 rgb(0 0 0 / 28%);
        font: 15px/1.4 "Comic Sans MS", "Trebuchet MS", system-ui, sans-serif;
        overflow-wrap: anywhere; pointer-events: auto; color-scheme: light; }
      .tail { position: fixed; width: 18px; height: 18px; background: #fff8cf;
        border: 3px solid #171717; transform: rotate(45deg); pointer-events: none; }
      .step { font: 12px/1.4 system-ui, sans-serif; margin-bottom: 7px; }
      .title { font-weight: 700; font-size: 18px; margin-bottom: 8px; }
      .text { white-space: pre-wrap; }
      .controls { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 14px; }
      button { font: 600 13px/1.3 system-ui, sans-serif; border: 2px solid #171717;
        border-radius: 10px; padding: 7px 10px; background: #fff; color: #171717; cursor: pointer; }
      button:focus-visible { outline: 3px solid #4679db; outline-offset: 2px; }
      button[disabled] { opacity: .4; cursor: default; }
      .next { background: #ffd84d; }
    `);
    root.adoptedStyleSheets = [sheet];
    const node = (tag, className, text) => {
      const result = document.createElement(tag);
      result.className = className;
      if (text !== undefined) result.textContent = text;
      return result;
    };
    const shades = Array.from({length: 4}, () => node('div', 'shade'));
    const frame = node('div', 'frame');
    const tail = node('div', 'tail');
    const bubble = node('section', 'bubble');
    bubble.setAttribute('role', 'region');
    bubble.setAttribute('aria-label', 'Services DevTools page guide');
    bubble.setAttribute('aria-live', 'polite');
    const step = node('div', 'step', args.total ? `Step ${args.step} of ${args.total}` : `Step ${args.step}`);
    const title = node('div', 'title', args.title);
    const text = node('div', 'text', args.text);
    const controls = node('div', 'controls');
    bubble.append(step, title, text, controls);
    root.append(...shades, frame, tail, bubble);
    let raf = 0, retention = 0, geometry = '';
    const guide = {
      id: args.guideId, action: null, lastSeen: performance.now(),
      dispose() {
        cancelAnimationFrame(raf);
        clearTimeout(retention);
        window.removeEventListener('keydown', escape, true);
        host.remove();
      },
      finish(action) {
        if (!guide.action) guide.action = action;
        guide.dispose();
        retention = setTimeout(() => {
          if (window[guideKey] === guide) delete window[guideKey];
        }, 30000);
      },
      update() {
        if (guide.action) return;
        if (!host.isConnected || !element.isConnected || !visible(element)) {
          guide.finish('target_lost');
          return;
        }
        if (performance.now() - guide.lastSeen > 30000) {
          guide.finish('disconnected');
          return;
        }
        const rect = element.getBoundingClientRect();
        const w = innerWidth, h = innerHeight;
        if (rect.bottom <= 0 || rect.top >= h || rect.right <= 0 || rect.left >= w) {
          root.querySelectorAll('.shade,.frame,.tail,.bubble').forEach(part => { part.style.visibility = 'hidden'; });
          geometry = '';
          return;
        }
        root.querySelectorAll('.shade,.frame,.tail,.bubble').forEach(part => { part.style.visibility = 'visible'; });
        const left = Math.max(0, rect.left - 6), top = Math.max(0, rect.top - 6);
        const right = Math.min(w, rect.right + 6), bottom = Math.min(h, rect.bottom + 6);
        const bw = bubble.offsetWidth, bh = bubble.offsetHeight;
        const nextGeometry = [left, top, right, bottom, w, h, bw, bh].join(',');
        if (geometry === nextGeometry) return;
        geometry = nextGeometry;
        const box = (part, x, y, width, height) => Object.assign(part.style, {
          left: `${x}px`, top: `${y}px`, width: `${Math.max(0, width)}px`, height: `${Math.max(0, height)}px`,
        });
        box(frame, left, top, right - left, bottom - top);
        box(shades[0], 0, 0, w, top);
        box(shades[1], 0, top, left, bottom - top);
        box(shades[2], right, top, w - right, bottom - top);
        box(shades[3], 0, bottom, w, h - bottom);
        const candidates = [
          {x: Math.max(12, Math.min(left, w - bw - 12)), y: bottom + 20, side: 'below'},
          {x: Math.max(12, Math.min(left, w - bw - 12)), y: top - bh - 20, side: 'above'},
          {x: right + 20, y: Math.max(12, Math.min(top, h - bh - 12)), side: 'right'},
          {x: left - bw - 20, y: Math.max(12, Math.min(top, h - bh - 12)), side: 'left'},
        ];
        const chosen = candidates.find(p => p.x >= 12 && p.y >= 12 && p.x + bw <= w - 12 && p.y + bh <= h - 12);
        const place = chosen || {x: Math.max(12, Math.min(w - bw - 12, right + 20)), y: Math.max(12, Math.min(h - bh - 12, bottom + 20))};
        bubble.style.left = `${place.x}px`;
        bubble.style.top = `${place.y}px`;
        tail.style.display = chosen ? 'block' : 'none';
        const horizontal = chosen && ['left', 'right'].includes(chosen.side);
        tail.style.left = `${horizontal ? chosen.side === 'right' ? place.x - 8 : place.x + bw - 10 : Math.max(place.x + 22, Math.min(place.x + bw - 32, (left + right) / 2 - 9))}px`;
        tail.style.top = `${horizontal ? place.y + 25 : chosen?.side === 'above' ? place.y + bh - 10 : place.y - 8}px`;
      },
    };
    const escape = event => {
      if (event.key === 'Escape') {
        event.preventDefault();
        event.stopImmediatePropagation();
        guide.finish('dismissed');
      }
    };
    for (const [action, label] of [['back', 'Back'], ['next', 'Next'], ['finish', 'Finish']]) {
      const button = node('button', action, label);
      button.type = 'button';
      button.disabled = action === 'back' && args.step === 1;
      button.addEventListener('click', event => { event.stopPropagation(); guide.finish(action); });
      controls.append(button);
    }
    host.setAttribute('popover', 'manual');
    document.documentElement.append(host);
    window[guideKey] = guide;
    window.addEventListener('keydown', escape, true);
    host.showPopover();
    element.scrollIntoView({block: 'center', inline: 'nearest', behavior: 'instant'});
    const tick = () => {
      guide.update();
      if (!guide.action) raf = requestAnimationFrame(tick);
    };
    tick();
    return {...info(), shown: true, untrusted: true};
  } else if (args.tool === 'page_click') {
    if (!element.matches(control))
      fail('Click targets must be interactive controls from the page snapshot.');
    if (element.matches('input[type="file"],input[type="password"],input[type="hidden"]'))
      fail('This input type is not available to page tools.');
    const anchor = element.closest('a[href]');
    const submits = element.form && ['submit', 'image'].includes(element.type);
    const baseTarget = element.ownerDocument.querySelector('base[target]')?.getAttribute('target') || '';
    const target = anchor ? anchor.getAttribute('target') || baseTarget
      : submits ? element.getAttribute('formtarget') || element.form.getAttribute('target') || baseTarget : '';
    if (target && target !== '_self') fail('Opening another tab or frame is not allowed.');
    const href = anchor?.href;
    if (href && !['http:', 'https:'].includes(new URL(href, location.href).protocol))
      fail('Only HTTP(S) links are allowed.');
    element.scrollIntoView({block: 'center', inline: 'nearest'});
    element.click();
  } else if (args.tool === 'page_focus' || args.tool === 'page_press_key') {
    if (element.matches('input[type="file"],input[type="password"],input[type="hidden"]') || privateNode(element, '[contenteditable]'))
      fail('This input type is not available to page tools.');
    if (args.tool === 'page_press_key') {
      if (!['Enter', 'Escape', 'Tab', 'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight',
        'Home', 'End', 'PageUp', 'PageDown', 'Backspace', 'Delete'].includes(args.key))
        fail('Unsupported key. Browser shortcuts and trusted input are not available.');
    }
    element.focus();
    if (args.tool === 'page_press_key') {
      for (const type of ['keydown', 'keyup'])
        element.dispatchEvent(new owner.KeyboardEvent(type, {key: args.key, bubbles: true, composed: true}));
    }
    return {...base(), performed: args.tool, selector: args.selector,
      focused: element.getRootNode().activeElement === element,
      trustedInput: false, verification: 'Read fresh page state to verify effects; synthetic keys do not guarantee native default actions.'};
  } else if (args.tool === 'page_set_checked') {
    if (element.localName !== 'input' || !['checkbox', 'radio'].includes(element.type)
      || typeof args.checked !== 'boolean') fail('Checked state requires a checkbox/radio and a boolean.');
    if (element.type === 'radio' && !args.checked) fail('Select another radio instead of clearing a radio directly.');
    if (element.checked !== args.checked) element.click();
    return {...base(), performed: args.tool, selector: args.selector,
      checked: element.checked, matchesRequested: element.checked === args.checked};
  } else {
    if (args.tool === 'page_select') {
      if (!(element instanceof owner.HTMLSelectElement) || element.multiple) fail('Select requires a single-choice select control.');
      if ((args.value !== undefined) === (args.optionIndex !== undefined))
        fail('Select requires exactly one of value or optionIndex.');
      if (args.optionIndex === undefined && (typeof args.value !== 'string' || args.value.length > 8000))
        fail('A value of at most 8000 characters is required.');
      const option = args.optionIndex !== undefined
        ? element.options[integer('optionIndex', 0)] : [...element.options].find(option => option.value === args.value);
      if (!option || option.disabled || option.parentElement?.disabled) fail('The requested option is unavailable.');
      if (args.optionIndex !== undefined) element.selectedIndex = args.optionIndex;
      else element.value = option.value;
    } else {
      if (typeof args.value !== 'string' || args.value.length > 8000) fail('A value of at most 8000 characters is required.');
      if (!(element instanceof owner.HTMLInputElement || element instanceof owner.HTMLTextAreaElement)
        || element.readOnly || (element instanceof owner.HTMLInputElement
          && !['text', 'search', 'email', 'url', 'tel', 'number'].includes(element.type)))
        fail('Fill supports editable text fields only, never passwords or files.');
      const prototype = element instanceof owner.HTMLTextAreaElement ? owner.HTMLTextAreaElement.prototype : owner.HTMLInputElement.prototype;
      Object.getOwnPropertyDescriptor(prototype, 'value').set.call(element, args.value);
    }
    element.scrollIntoView({block: 'center', inline: 'nearest'});
    element.focus();
    element.dispatchEvent(new owner.Event('input', {bubbles: true, composed: true}));
    element.dispatchEvent(new owner.Event('change', {bubbles: true, composed: true}));
    return {...base(), performed: args.tool, selector: args.selector,
      matchesRequested: args.tool === 'page_fill' ? element.value === args.value
        : args.optionIndex !== undefined ? element.selectedIndex === args.optionIndex : element.value === args.value};
  }
  return {...base(), performed: args.tool, selector: args.selector,
    verification: 'Event dispatched, not proof of success. Read fresh page state; do not retry an ambiguous navigation.'};
})
