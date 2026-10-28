(args => {
  const fail = message => { throw new Error(message); };
  const guideKey = '__servicesDevtoolsPageGuide';
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
    url: location.origin + location.pathname,
    title: document.title.slice(0, 200),
    pageId: String(performance.timeOrigin),
  });
  if (args.tool === 'metadata') return info();
  if (args.origin !== location.origin) fail('Page origin changed; new approval is required.');
  if (args.tool !== 'page_snapshot' && args.pageId !== info().pageId)
    fail('The document changed. Read a fresh page snapshot before acting.');
  if (args.tool === 'guide_status') {
    const guide = window[guideKey];
    if (!guide || guide.id !== args.guideId) return {...info(), action: 'missing'};
    guide.lastSeen = performance.now();
    guide.update();
    return {...info(), action: guide.action || 'waiting', untrusted: true};
  }
  const visible = element => {
    const box = element.getBoundingClientRect();
    const style = getComputedStyle(element);
    return box.width > 0 && box.height > 0 && style.visibility !== 'hidden'
      && style.display !== 'none' && !element.closest('[hidden],[aria-hidden="true"],[inert]');
  };
  const textOf = (root, limit, maxNodes) => {
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    let text = '', scanned = 0;
    while (walker.nextNode() && text.length < limit && scanned++ < maxNodes) {
      const parent = walker.currentNode.parentElement;
      if (!parent || parent.closest('input,textarea,select,script,style,noscript,[contenteditable]') || !visible(parent)) continue;
      const value = walker.currentNode.textContent.trim();
      if (value) text += value + '\n';
    }
    return text.slice(0, limit).trim();
  };
  const selector = element => {
    if (element.id) {
      const id = '#' + CSS.escape(element.id);
      if (document.querySelectorAll(id).length === 1) return id;
    }
    const parts = [];
    for (let node = element; node && node.nodeType === 1; node = node.parentElement) {
      const siblings = [...node.parentElement?.children || []]
        .filter(sibling => sibling.localName === node.localName);
      parts.unshift(node.localName + (siblings.length > 1 ? `:nth-of-type(${siblings.indexOf(node) + 1})` : ''));
      if (node === document.documentElement) break;
    }
    return parts.join(' > ');
  };
  if (args.tool === 'page_snapshot') {
    const text = textOf(document.body || document.documentElement, 12000, 5000);
    const elements = [];
    for (const element of document.querySelectorAll('button,a[href],input,textarea,select,summary,[role="button"],[role="link"],[role="checkbox"],[contenteditable="true"]')) {
      if (elements.length >= 80) break;
      if (!visible(element) || element.type === 'hidden') continue;
      const targetSelector = selector(element);
      if (targetSelector.length > 1024) continue;
      elements.push({
        selector: targetSelector,
        tag: element.localName,
        type: element.getAttribute('type'),
        role: element.getAttribute('role'),
        label: (element.getAttribute('aria-label') || [...element.labels || []].map(label => textOf(label, 200, 100)).join(' ')
          || element.getAttribute('placeholder') || textOf(element, 200, 100)
          || element.getAttribute('title') || '').slice(0, 200),
        disabled: element.matches(':disabled,[aria-disabled="true"]'),
        ...(element instanceof HTMLSelectElement ? {options: [...element.options].slice(0, 40)
          .map(option => ({label: option.label.slice(0, 200), value: option.value.slice(0, 200)}))} : {}),
      });
    }
    return {...info(), untrusted: true, text: text.slice(0, 12000), elements,
      viewport: {width: innerWidth, height: innerHeight, x: scrollX, y: scrollY},
      limitations: ['Main document DOM only; no cross-origin frames, closed shadow roots or canvas controls.',
        'Field contents, cookies and browser storage are not exposed. Password/file fields cannot be filled.']};
  }
  if (args.tool === 'page_scroll') {
    const x = args.x === undefined ? 0 : args.x;
    if (!Number.isInteger(x) || !Number.isInteger(args.y)
      || Math.abs(x) > 2000 || Math.abs(args.y) > 2000) fail('Scroll deltas must be integers within 2000 pixels.');
    window.scrollBy({left: x, top: args.y, behavior: 'instant'});
    return {...info(), x: scrollX, y: scrollY};
  }
  if (!['page_click', 'page_fill', 'page_select', 'page_guide_show'].includes(args.tool)) fail('Unknown page tool.');
  if (typeof args.selector !== 'string' || !args.selector || args.selector.length > 1024)
    fail('A bounded selector from a fresh page snapshot is required.');
  const matches = document.querySelectorAll(args.selector);
  if (matches.length !== 1) fail('The selector must match exactly one element. Read a fresh snapshot.');
  const element = matches[0];
  if (!visible(element) || element.matches(':disabled,[aria-disabled="true"]')) fail('The target is hidden or disabled.');
  if (args.tool === 'page_guide_show') {
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
    if (!element.matches('button,a[href],input,summary,[role="button"],[role="link"],[role="checkbox"]'))
      fail('Click targets must be interactive controls from the page snapshot.');
    if (element.matches('input[type="file"],input[type="password"],input[type="hidden"]'))
      fail('This input type is not available to page tools.');
    const anchor = element.closest('a[href]');
    const submits = element.form && ['submit', 'image'].includes(element.type);
    const baseTarget = document.querySelector('base[target]')?.getAttribute('target') || '';
    const target = anchor ? anchor.getAttribute('target') || baseTarget
      : submits ? element.getAttribute('formtarget') || element.form.getAttribute('target') || baseTarget : '';
    if (target && target !== '_self') fail('Opening another tab or frame is not allowed.');
    const href = anchor?.href;
    if (href && !['http:', 'https:'].includes(new URL(href, location.href).protocol))
      fail('Only HTTP(S) links are allowed.');
    element.scrollIntoView({block: 'center', inline: 'nearest'});
    element.click();
  } else {
    if (typeof args.value !== 'string' || args.value.length > 8000) fail('A value of at most 8000 characters is required.');
    if (args.tool === 'page_select') {
      if (!(element instanceof HTMLSelectElement) || element.multiple) fail('Select requires a single-choice select control.');
      const option = [...element.options].find(option => option.value === args.value);
      if (!option || option.disabled || option.parentElement?.disabled) fail('The requested option is unavailable.');
      element.value = args.value;
    } else {
      if (!(element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement)
        || element.readOnly || (element instanceof HTMLInputElement
          && !['text', 'search', 'email', 'url', 'tel', 'number'].includes(element.type)))
        fail('Fill supports editable text fields only, never passwords or files.');
      const prototype = element instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
      Object.getOwnPropertyDescriptor(prototype, 'value').set.call(element, args.value);
    }
    element.scrollIntoView({block: 'center', inline: 'nearest'});
    element.focus();
    element.dispatchEvent(new Event('input', {bubbles: true}));
    element.dispatchEvent(new Event('change', {bubbles: true}));
  }
  return {...info(), performed: args.tool, selector: args.selector, untrusted: true};
})
