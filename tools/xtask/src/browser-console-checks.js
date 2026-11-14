// Shared Console checks reused by several scenarios; loaded after browser-driver.js on every page.
(() => {
  const { assert, sleep, seen, wait, waitFor, settle, visible, find, testid, editorOf, key, type, move, input, click, button,
    openConsole, activateSession, setSource, candidates, pick, assertNativeScrollbars, nativeTrackPaint, sessionLibrary, nativeHold, holds } = testDriver;
  // Waits for Chrome's overlay scrollbar to fade on its own instead of sleeping past the fade delay.
  const overlayScrollbars = element => element.offsetWidth === element.clientWidth;
  const trackFaded = async (element, axes, message) => {
    let last = {};
    await settle();
    return waitFor(async () => {
      for (const axis of axes) {
        last = { axis, ...(await nativeTrackPaint(element, axis)) };
        if (last.brightRunRatio >= 0.03) return false;
      }
      return true;
    }, { timeout: 8000, interval: 100, message: () => `${message}: ${JSON.stringify(last)}` });
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
        const marker = editor.lineInfo(line).gutterMarkers?.['editor-fold-gutter'];
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
      line > checks[0] && line < firstEnd && !output(line) && /^\s*#target(?:\s|$)/i.test(text));
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
    await openConsole();
    await click(testid('console-service-terminal'));
    let editor = editorOf('console-editor');
    const block = (name, command) => `#check ${name}\n#target\n/bin/sh -c\n#payload\n${command}`;
    setSource(editor, [
      block('move-first', "printf '%s\\n' 'first-executed' '#check literal-output' '#target' 'not-a-request'"),
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
      await wait(() => !find('.status-bar').classList.contains('is-running')
        && find('.status-bar').textContent.includes('OK')
        && editor.getValue().split('\n').includes(output),
        () => `Moved check must execute its own command: ${JSON.stringify({name, status:find('.status-bar').textContent, source:editor.getValue(), regions:regions()})}`);
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
    const restored = editorOf('console-editor');
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
    const lights = [...document.querySelectorAll('.console-root .indicator-light')].filter(visible);
    assert(lights.length > 0, 'Navigation signals must exist before an operation');
    for (const light of lights) {
      const bounds = light.getBoundingClientRect();
      const tab = light.parentElement.getBoundingClientRect();
      const left = light.parentElement.matches('.check-rail-link');
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
          && (style.animationName==='indicator-flow'||matchMedia('(prefers-reduced-motion: reduce)').matches),
          'Running signals must use a green flowing gradient, respecting reduced motion');
      } else if(light.dataset.state==='error') {
        assert(style.backgroundColor==='rgb(255, 137, 128)' && style.animationName==='none',
          'Errors must be steady red, never animated');
      }
    }
  };
  const checkConsoleTools = async endpoint => {
    await openConsole();
    await click(testid('console-service-rest'));
    const editor = editorOf('console-editor');
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
        && getComputedStyle(badge).textOverflow==='ellipsis' && getComputedStyle(badge).borderTopWidth==='2px',
        'Even a long session name must remain an ellipsized badge bounded by the tool menu');
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
      const heading = [...document.querySelectorAll('.console-tool-panel .ui-panel-title')].find(visible);
      if (heading) {
        const headingBounds = heading.getBoundingClientRect();
        assert(Math.abs(headingBounds.top - tabBounds.top) < 1
          && Math.abs(headingBounds.bottom - 1 - body.top) < 1,
        'Right-panel headings must sit in the service-menu row, their bottom line on the central frame top');
      }
      assert(Math.abs(body.left - status.left) < 1 && Math.abs(body.right - status.right) < 1,
        'Console status must align with the inner workspace, excluding both side menus');
      const statusStyle = getComputedStyle(find('.status-bar'));
      for (const area of document.querySelectorAll('.console-root .editor, .console-root .editor-gutters')) {
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
      const debug = document.querySelector('.status-bar-debug');
      if (visible(debug)) {
        const bounds = debug.getBoundingClientRect();
        assert(Math.abs(mainStatus.left - bounds.left) < 1 && Math.abs(mainStatus.right - bounds.right) < 1,
          'Expanded Debug must align with its own status column');
      }
      return [status.left, rightPane?.right??status.right];
    };
    const closedStatusBounds = assertStatusBounds();
    const links = () => [...testid('console-check-menu').querySelectorAll('button')];
    const active = () => testid('console-check-menu').querySelector('.active .check-rail-number')?.textContent;
    const top = () => editor.lineAtHeight(editor.getScrollInfo().top, 'local');
    setSource(editor, ['cursor-one', 'cursor-two', 'cursor-three'].map(name =>
      `#check ${name}\n#target\n${endpoint}`).join('\n\n'));
    editor.setCursor(0, 0);
    editor.scrollTo(null, 0);
    await wait(() => links().length === 3 && active() === '1');
    const checkBackground = name => {
      const line=[...editor.getWrapperElement().querySelectorAll('pre.editor-line')]
        .find(line=>line.textContent.includes(`#check ${name}`));
      assert(line,`A visible check line must be rendered: ${name}`);
      return getComputedStyle(line,'::before').backgroundColor;
    };
    assert(checkBackground('cursor-one')===checkBackground('cursor-three')
      && checkBackground('cursor-one')!==checkBackground('cursor-two'),
      'Whole checks must alternate between two neutral backgrounds');
    const firstEndpoint=[...editor.getWrapperElement().querySelectorAll('pre.editor-line')]
      .find(line=>line.textContent.includes(endpoint));
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
    const responseHeading=[...editor.getWrapperElement().querySelectorAll('pre.editor-line')]
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
    const comment=links()[0].querySelector('.check-rail-comment');
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
    assert(getComputedStyle(links()[0].querySelector('.check-rail-comment')).display==='none',
      'The compact rail must show numbers only');
    await move(1, 1);
    const tabStyle = element => {
      const style = getComputedStyle(element);
      return ['backgroundColor', 'color', 'fontSize', 'fontWeight', 'borderTopColor', 'borderTopWidth']
        .map(property => style[property]).join('|');
    };
    assert(tabStyle(testid('console-service-rest')) === tabStyle(links()[0]),
      'Active service tabs and check tabs must share the same visual style');
    assert(tabStyle(testid('console-service-mysql')) === tabStyle(links().find(link => !link.classList.contains('active'))),
      'Inactive service tabs and inactive check tabs must share the same visual style');
    assert(links().map(el => el.dataset.line).join(',') === '1,100,105',
      'Check numbers must point to actual document lines');
    await click(testid('console-agent-toggle'));
    for (const [index, line] of [[1, 99], [2, 104], [0, 0]]) {
      await click(links()[index]);
      await wait(() => top() === line && active() === String(index + 1),
        () => `Check jump must align even the last check: ${JSON.stringify({index, line, top:top(), scroll:editor.getScrollInfo()})}`);
      const wrapper=editor.getWrapperElement();
      assertNativeScrollbars(editor.getScrollerElement());
      assert(!wrapper.querySelector('.editor-vscrollbar,.editor-hscrollbar,.editor-scrollbar-filler'),
        'Console must use its real native scroller, not a synchronized proxy scrollbar or corner filler');
      if(overlayScrollbars(editor.getScrollerElement())) {
        await trackFaded(editor.getScrollerElement(),['vertical','horizontal'],
          `Native scrollbar paint must fade after check jump ${index} without moving the pointer`);
        const box=wrapper.getBoundingClientRect();
        const point={x:box.left+box.width/2,y:box.top+box.height/2};
        await move(point.x,point.y);
        const beforeWheel=editor.getScrollInfo().top;
        await input('Input.dispatchMouseEvent',{type:'mouseWheel',...point,deltaX:80,deltaY:index===0?60:-60});
        await wait(()=>editor.getScrollInfo().top!==beforeWheel,
          'Manual wheel scrolling must work immediately after a check jump with the Agent open');
        await trackFaded(editor.getScrollerElement(),['vertical','horizontal'],
          `Native scrollbar paint must fade after jump ${index} then wheel with the Agent open, without moving the pointer to its track`);
      }
    }
    const wheelBox=editor.getWrapperElement().getBoundingClientRect();
    await move(wheelBox.left+wheelBox.width/2,wheelBox.top+wheelBox.height/2);
    await input('Input.dispatchMouseEvent',{type:'mouseWheel',x:wheelBox.left+wheelBox.width/2,
      y:wheelBox.top+wheelBox.height/2,deltaX:80,deltaY:100});
    await wait(()=>editor.getScrollInfo().left>0 && editor.getScrollInfo().top>0,
      'Native editor scrollbars must preserve real wheel scrolling in both directions');
    assertNativeScrollbars(editor.getScrollerElement());
    if(overlayScrollbars(editor.getScrollerElement())) {
      const bar=editor.getScrollerElement();
      const b=bar.getBoundingClientRect();
      const x=b.right-5;
      const y=b.top+(bar.scrollTop+bar.clientHeight/2)*bar.clientHeight/bar.scrollHeight;
      await move(x,y);
      await waitFor(async()=>(await nativeTrackPaint(bar)).brightRatio>0.5,
        {timeout:8000,interval:100,message:'Hovering the native track must reveal it before pressing its thumb'});
      const beforeDrag=editor.getScrollInfo().top;
      await input('Input.dispatchMouseEvent',{type:'mousePressed',x,y,button:'left',clickCount:1});
      // Behaviour under test: hold longer than Chrome's overlay fade delay; a held thumb must stay visible.
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
      await trackFaded(bar,['vertical'],'The real native track must fade after dragging ends and the pointer leaves it');
      editor.scrollTo(0,editor.heightAtLine(80,'local'));
      await trackFaded(bar,['vertical'],'The native track must fade after a programmatic scroll before clicking text beneath its edge');
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
    assert(links().map(el => el.querySelector('.check-rail-number').textContent).join(',') === '1,2,3,4'
      && links()[2].dataset.line === '101', 'Inserting a check renumbers later links and updates line targets');
    editor.replaceRange('', {line:50,ch:0}, {line:51,ch:0});
    await wait(() => links().length === 3 && links()[1].dataset.line === '100');
    const source = editor.getValue();
    setSource(editor, Array.from({length: 60}, (_, index) =>
      `#check Scroll ${index + 1}\n#target\n${endpoint}`).join('\n\n'));
    await wait(() => links().length === 60);
    const checkMenu = testid('console-check-menu').querySelector('.check-rail-links');
    checkMenu.scrollTop = checkMenu.scrollHeight;
    assert(checkMenu.scrollTop > 0 && checkMenu.scrollHeight > checkMenu.clientHeight,
      'Check navigation must scroll when its entries overflow');
    assert(testid('console-check-resizer').getBoundingClientRect().top
      ===testid('console-check-menu').getBoundingClientRect().top,
      'The check-menu resize handle must remain accessible while its entries scroll');
    setSource(editor, source);
    await wait(() => links().length === 3);
    const { openPanel, closePanel, fill, doubleClick } = testDriver;
    const sessionId = () => Number(testid('console-session-name').dataset.sessionId);
    // A panel module's session kv key in the saved session.
    const stored = async (module, key) =>
      (await sessionLibrary()).sessions.find(s => s.id === sessionId()).snapshot.modules?.[module]?.[key];
    const toolStatus = () => [...document.querySelectorAll('.console-right-status .status-bar')].find(visible);
    const mainStatusText = () => testid('console-main-status').textContent;
    const toolLight = tool => testid(`console-${tool}-toggle`).querySelector('.indicator-light')?.dataset.state;
    const editorColumn = find('.console-top-bar');
    const hintsPanel = await openPanel('hints');
    await wait(() => Math.abs(hintsPanel.closest('.console-right-pane').getBoundingClientRect().width
      - editorColumn.clientWidth) < 1, () => `Side panel width ${hintsPanel.closest('.console-right-pane')?.getBoundingClientRect().width} vs ${editorColumn.clientWidth}`);
    assert(JSON.stringify(assertStatusBounds()) === JSON.stringify(closedStatusBounds),
      'Opening the right panel must not change status boundaries');
    assert(getComputedStyle(find('.console-toolbar')).flexDirection === 'column'
      && getComputedStyle(testid('console-check-menu')).flexDirection === 'column',
      'Both side menus must be vertical');
    const hintsTitle = await wait(() => find('.ui-panel-title .tab-button', hintsPanel));
    await wait(() => hintsTitle.textContent === 'REST completion hints', 'Hints must be titled by the active service');
    assert(tabStyle(hintsTitle) === tabStyle(testid('console-service-rest')),
      'Hints must have a tab-styled title matching the active service tabs');
    const hints = await wait(() => testDriver.editorAt('console-hints'));
    const originalHints = hints.getValue();
    setSource(hints, '#target\nhttp://\n');
    await type('#');
    await wait(() => JSON.stringify(candidates().sort()) === JSON.stringify(['#action', '#metadata', '#options', '#payload']),
      () => `Typing # in Hints must offer the sections it lacks: ${JSON.stringify(candidates())}`);
    await pick(hints, '#payload');
    assert(hints.getLine(2) === '#payload', 'Picking a Hints section must insert its header');
    const changedHints = `${originalHints.trimEnd()}\n#metadata\nX-Console-Panel-Fixture: saved\n`;
    hints.setValue(changedHints);
    await wait(() => toolStatus()?.textContent.includes('Hints save · OK'), () => `Edited hints must be saved to Local: ${toolStatus()?.textContent}`);
    await closePanel('hints');
    await openPanel('hints');
    await wait(() => testDriver.editorAt('console-hints')?.getValue() === changedHints);
    editorOf('console-hints').setValue('## accidental-service-boundary\ninvalid fixture');
    await wait(() => toolStatus()?.textContent.includes('ERROR')
      && toolStatus().textContent.includes('service headers'));
    const hintsError = toolStatus().textContent;
    const hintsMainStatus = mainStatusText();
    await click(toolStatus());
    assertStatusBounds();
    assert(testid('console-hints-status-debug').textContent.includes('Hints validation'),
      'Hints errors must reach the panel Debug, not the check Debug');
    await click(toolStatus());
    await closePanel('hints');
    assert(mainStatusText() === hintsMainStatus, 'Hints validation must not overwrite the main check status');
    assert(editor.getValue() === source, 'Panel errors must not modify the Console document');
    await openPanel('hints');
    await wait(() => testDriver.editorAt('console-hints')?.getValue().startsWith('## accidental'));
    assert(toolStatus().textContent === hintsError, 'Reopening hints must retain its own error status');
    await openPanel('variables');
    assert(JSON.stringify(assertStatusBounds()) === JSON.stringify(closedStatusBounds),
      'Switching panels must not change status boundaries');
    assert(!document.querySelector('[data-testid="hints-panel"]')
      && document.querySelectorAll('.module-panel').length === 1,
      'Variables and Hints must share one side panel without overlapping');
    const panelTabs = () => [...document.querySelectorAll('.module-panel [role="tab"]')];
    assert(panelTabs().map(el => el.textContent).join(',') === 'var,env (read-only)'
      && !document.querySelector('.module-panel input'),
      'Variables must contain only fixed var and read-only env, without set CRUD');
    await click(testid('variables-tab-var'));
    const variables = () => testDriver.editorAt('console-variables');
    await wait(() => variables()?.getOption('readOnly') !== true);
    const originalVars = variables().getValue();
    assertSignalGeometry();
    variables().setValue(`${originalVars}\nCONSOLE_PANEL_FIXTURE: saved`);
    await wait(() => toolStatus().textContent.includes('Variables save · OK') && toolLight('variables') === 'ready',
      'A visible save result must be acknowledged without clearing its status');
    variables().setValue('missing delimiter');
    await wait(() => toolStatus().textContent.includes('ERROR')
      && toolStatus().textContent.includes('NAME: value') && toolLight('variables') === 'ready');
    await seen();
    const varsError = toolStatus().textContent;
    const varsMainStatus = mainStatusText();
    await closePanel('variables');
    assert(mainStatusText() === varsMainStatus, 'Variables validation must not overwrite the main check status');
    assert(toolLight('variables') === 'ready', 'Closing Variables must not relight an already viewed failure');
    await openPanel('variables');
    await wait(() => variables()?.getValue() === 'missing delimiter');
    assert(toolStatus().textContent === varsError, 'Reopening Variables must retain its own validation error');
    await click(testid('variables-tab-env'));
    await wait(() => variables()?.getOption('readOnly') === true);
    await click(testid('variables-tab-var'));
    await wait(() => variables()?.getValue() === 'missing delimiter');
    variables().setValue(`${originalVars}\nCONSOLE_PANEL_FIXTURE: shared`);
    await wait(() => toolStatus().textContent.includes('Variables save · OK'));
    await closePanel('variables');
    await openPanel('variables');
    await click(testid('variables-tab-var'));
    await wait(() => variables()?.getValue().includes('CONSOLE_PANEL_FIXTURE: shared'),
      'Reopening Variables must show the saved variables');
    variables().setValue(originalVars);
    await wait(() => toolStatus().textContent.includes('Variables save · OK') && toolLight('variables') === 'ready');
    await openPanel('dictionary');
    assertStatusBounds();
    assert(panelTabs().every(el => !['var', 'env (read-only)'].includes(el.textContent))
      && !document.querySelector('[data-testid="variables-panel"]'),
      'Dictionary must be a separate panel with no var or env tabs');
    const dictionary = () => testDriver.editorAt('console-dictionary');
    const dictionarySet = panelTabs().find(el => el.getAttribute('aria-selected') === 'true').querySelector('.tab-label').textContent;
    const originalDictionary = (await wait(() => dictionary())).getValue();
    dictionary().setValue(`${originalDictionary}\nDICTIONARY_PANEL_FIXTURE: shared`);
    await wait(() => toolStatus().textContent.includes('Dictionary save · OK'));
    await closePanel('dictionary');
    await openPanel('dictionary');
    await wait(() => dictionary()?.getValue().includes('DICTIONARY_PANEL_FIXTURE: shared'),
      'Reopening Dictionary must show the saved set');
    const invalidDictionary = `${originalDictionary}\nmissing delimiter`;
    dictionary().setValue(invalidDictionary);
    // Left before it counts as seen, the error stays lit.
    await wait(() => toolStatus().textContent.includes('ERROR') && toolLight('dictionary') === 'error');
    await openPanel('variables');
    await click(testid('variables-tab-var'));
    await wait(() => variables()?.getValue() === originalVars, 'Dictionary edits and drafts must not alter var');
    assert(toolLight('dictionary') === 'error' && toolLight('variables') === 'ready',
      'An unseen Dictionary error must stay lit without contaminating the Variables signal');
    await openPanel('dictionary');
    await wait(() => dictionary()?.getValue() === invalidDictionary && toolStatus().textContent.includes('ERROR'),
      'Reopening Dictionary must keep its draft and status');
    dictionary().setValue(originalDictionary);
    await wait(() => toolStatus().textContent.includes('Dictionary save · OK'));
    window.prompt = () => 'console-panel-fixture';
    await click(testid('dictionary-tabs-add'));
    await wait(() => document.querySelector('[data-testid="dictionary-set-console-panel-fixture"]')?.getAttribute('aria-selected') === 'true'
      && toolStatus().textContent.includes('Dictionary add · OK'), 'A new set opens');
    await doubleClick(testid('dictionary-set-console-panel-fixture'));
    const rename = await wait(() => document.querySelector('[data-testid="dictionary-tabs-rename"]'));
    rename.value = 'console-panel-renamed';
    rename.dispatchEvent(new InputEvent('input', {bubbles:true}));
    await key('Enter');
    await wait(() => document.querySelector('[data-testid="dictionary-set-console-panel-renamed"]')
      && toolStatus().textContent.includes('Dictionary rename · OK'), 'A set of the user\'s own can be renamed in its tab');
    testid('dictionary-set-console-panel-renamed').dispatchEvent(new DragEvent('dragstart', {bubbles:true}));
    panelTabs()[0].dispatchEvent(new DragEvent('drop', {bubbles:true,cancelable:true}));
    await wait(() => toolStatus().textContent.includes('Dictionary reorder · OK')
      && panelTabs()[0].dataset.testid === 'dictionary-set-console-panel-renamed', 'Dictionary sets reorder by dragging their tabs');
    await click(testid('dictionary-set-console-panel-renamed'));
    window.confirm = () => true;
    await click(testid('dictionary-tabs-close-dictionary-set-console-panel-renamed'));
    await wait(() => !document.querySelector('[data-testid="dictionary-set-console-panel-renamed"]')
      && toolStatus().textContent.includes('Dictionary delete · OK'), 'A set of the user\'s own can be deleted');
    await click(testid(`dictionary-set-${dictionarySet}`));
    await wait(() => dictionary()?.getValue() === originalDictionary);
    await openPanel('hints');
    await wait(() => testDriver.editorAt('console-hints')?.getValue().startsWith('## accidental'),
      'Invalid hints drafts survive panel switches');
    editorOf('console-hints').setValue(originalHints);
    await wait(() => toolStatus().textContent.includes('Hints save · OK'));
    editorOf('console-hints').setValue('{{incomplete:');
    await wait(() => toolStatus().textContent.includes('ERROR'));
    await closePanel('hints');
    setSource(editor, `#check panel-status\n#target\n${endpoint}\n`);
    await key('Shift+Enter');
    await wait(() => find('.status-bar').textContent.startsWith('Request · ')
      && find('.status-bar').textContent.includes('OK'),
      'An ordinary request must use only its own status, independent of the retained panel error');
    const frame = document.createElement('iframe');
    frame.src = location.href;
    frame.style.cssText = 'position:fixed;inset:0;width:100vw;height:100vh;z-index:9999';
    document.body.append(frame);
    try {
      await wait(() => frame.contentDocument?.querySelector('[data-testid="console-hints-toggle"]'));
      const doc = frame.contentDocument;
      await activateSession(sessionId(), doc);
      await click(testid('console-hints-toggle', doc));
      await wait(() => doc.querySelector('[data-testid="console-hints"]')
        && editorOf('console-hints', doc).getValue() === '{{incomplete:',
        'An invalid hints draft must survive a new application instance');
    } finally {
      frame.remove();
    }
    await openPanel('hints');
    assert(editorOf('console-hints').getValue() === '{{incomplete:', 'A request must not discard an invalid hints draft');
    editorOf('console-hints').setValue(originalHints);
    await wait(() => toolStatus().textContent.includes('Hints save · OK'));
    await closePanel('hints');
    await checkConsoleMoves();
  };
  Object.assign(testDriver, { checkConsoleFolding, checkConsoleTools, assertSignalGeometry });
})();
