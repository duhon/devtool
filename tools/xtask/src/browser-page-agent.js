(async () => {
  const {assert, wait, holds, click, testid, key, openConsole, input} = testDriver;
  await openConsole();
  if (!document.querySelector('[data-testid="agent-composer"]')?.getBoundingClientRect().width)
    await click(testid('console-agent-toggle'));
  await click(testid('agent-new-session'));
  const composer = () => testid('agent-composer');
  const assistants = () => [...testid('agent-transcript').querySelectorAll('.agent-message-assistant')];
  let before = 0;
  const send = async message => {
    await wait(() => testid('agent-stop').disabled);
    before = assistants().length;
    composer().value = message;
    composer().dispatchEvent(new InputEvent('input', {bubbles:true}));
    composer().focus();
    await key('Enter');
  };
  const completed = async text => {
    await wait(() => assistants().length > before && assistants().at(-1).textContent.includes(text),
      () => `Missing page Agent reply "${text}": ${testid('agent-transcript').textContent}; ${document.querySelector('.agent-error')?.textContent}`);
    await wait(() => testid('agent-stop').disabled);
    assert(!document.querySelector('[data-testid="agent-page-approval"]'), 'Finished turn retained page access approval');
  };
  const pending = () => wait(() => document.querySelector('[data-testid="agent-page-approval"]'),
    () => `Page access approval did not appear: ${testid('agent-transcript').textContent}`);
  const page = async expression => {
    const result = await input('Test.inspectedPage', {expression});
    assert(!result.exceptionDetails, `Inspected fixture exception: ${JSON.stringify(result.exceptionDetails)}`);
    return result.result.value;
  };
  await send('agent page standalone');
  await completed('Page rejected: Page tools are available only in the DevTools extension');
  const original = Object.getOwnPropertyDescriptor(chrome, 'devtools');
  let pageReads = 0;
  let guideCleanups = 0;
  let pageWaitReads = 0;
  let pageWaitPending = 0;
  let enabledWaitReads = 0;
  Object.defineProperty(chrome, 'devtools', {configurable:true, value:{inspectedWindow:{
    tabId:9001,
    eval(expression, callback) {
      if (expression.includes('"tool":"page_snapshot"')) pageReads++;
      const pageWait = expression.includes('"tool":"page_wait"');
      if (pageWait) { pageWaitReads++; pageWaitPending++; }
      if (expression.includes('"tool":"page_wait"') && expression.includes('"state":"enabled"')) {
        enabledWaitReads++;
        if (enabledWaitReads === 1) expression = `(() => {
          document.querySelector('#apply').disabled=true;
          // Fixture behaviour: the page enables #apply later so the Agent's page_wait must poll for it.
          setTimeout(()=>{document.querySelector('#apply').disabled=false;},350);
          return ${expression};
        })()`;
      }
      input('Test.inspectedPage', {expression}).then(result => {
        callback(result.result?.value, result.exceptionDetails
          ? {isException:true,value:result.exceptionDetails.exception?.description || result.exceptionDetails.text}
          : undefined);
        if (expression.includes('"tool":"guide_cleanup"')) guideCleanups++;
        if (pageWait) pageWaitPending--;
      });
    },
  }, network: {getHAR(callback) {callback({entries:[
    {startedDateTime:new Date().toISOString(),request:{url:'https://user:password-secret@api.example.test/safe?token=cookie-secret#storage-secret',
      method:'GET',headers:[{name:'Authorization',value:'field-secret'}]},
      response:{status:503,bodySize:123,content:{text:'field-secret'}},time:17},
    {startedDateTime:'1970-01-01T00:00:00Z',request:{url:'https://api.example.test/old-navigation',method:'GET'},
      response:{status:200},time:1},
  ]});}}}});
  try {
    await send('agent page read');
    await completed('Page snapshot ready');
    assert(pageReads === 1, 'Reading must access the inspected page without a permission prompt');
    await send('agent page inspect');
    await completed('DOM values ready');

    await send('agent page deny');
    assert((await pending()).textContent.includes(new URL(fixtureUrl).origin), 'Approval must identify the inspected site');
    assert(pageReads > 1, 'The Agent should read page state before requesting permission to act');
    await click(testid('agent-deny-page'));
    await completed('Page writing denied; reading available');
    assert(await page('window.clicks') === 0, 'Denying writes still allowed a site action');

    await send('agent page stop');
    await pending();
    await click(testid('agent-stop'));
    await wait(() => testid('agent-stop').disabled && !document.querySelector('[data-testid="agent-page-approval"]'));
    assert(await page('window.clicks') === 0, 'Stopped approval caused a site action');

    await send('agent page read');
    await completed('Page snapshot ready');
    assert(!document.querySelector('[data-testid="agent-page-approval"]'), 'Reading asked for write permission');
    assert(!document.querySelector('[data-testid="agent-run-approval"]'), 'Page permission must be separate from Console run permission');
    await send('agent page extended');
    await pending();
    await click(testid('agent-approve-page'));
    await completed('Extended page tools complete');
    assert(enabledWaitReads > 1, 'The controller did not actually wait for asynchronous website state');
    await wait(async () => !await page(`!!window.__servicesDevtoolsPageAccess`));

    const waitsBefore = pageWaitReads;
    await send('agent page wait stop');
    await wait(() => pageWaitReads > waitsBefore);
    await click(testid('agent-stop'));
    await wait(() => testid('agent-stop').disabled);
    await wait(() => pageWaitPending === 0, 'The page_wait read in flight at Stop must finish');
    const stoppedWaitReads = pageWaitReads;
    // Window longer than the 250 ms page_wait poll delay: no further poll may start after Stop.
    await holds(() => pageWaitReads === stoppedWaitReads, 'Stop left the page wait polling active', 300);
    await wait(async () => !await page(`!!window.__servicesDevtoolsPageAccess`));

    await page(`document.head.insertAdjacentHTML('beforeend','<base target="_blank">');true`);
    await send('agent page actions');
    await completed('Page actions complete');
    const state = await page(`({name:document.querySelector('#name').value,choice:document.querySelector('#choice').value,
      clicks:window.clicks,y:scrollY,password:document.querySelector('#password').value})`);
    assert(state.name === 'Agent filled' && state.choice === 'two' && state.clicks === 1 && state.y >= 400,
      `Agent actions did not affect the fixed inspected page: ${JSON.stringify(state)}`);
    assert(state.password === 'password-secret', 'Agent modified a password');
    await send('agent page write');
    await completed('Page write complete');
    assert(!document.querySelector('[data-testid="agent-page-approval"]'),
      'The same document requested interaction permission again in another turn');

    await send('agent page constraints');
    await completed('Page constraints verified');
    assert(await page('window.clicks') === 1, 'Rejected/stale page operation caused a click');

    const command = (method, params) => input('Test.inspectedCommand', {method, params});
    const mouse = async (x, y) => {
      await command('Input.dispatchMouseEvent', {type:'mousePressed',x,y,button:'left',clickCount:1});
      await command('Input.dispatchMouseEvent', {type:'mouseReleased',x,y,button:'left',clickCount:1});
    };
    const guideStep = step => wait(async () => await page(
      `document.getElementById('services-devtools-page-guide')?.shadowRoot.querySelector('.step').textContent === 'Step ${step} of 2'`));
    const guideButton = async action => {
      const box = await page(`(() => {
        const button=document.getElementById('services-devtools-page-guide').shadowRoot.querySelector('button.${action}');
        const r=button.getBoundingClientRect(); return {x:r.x+r.width/2,y:r.y+r.height/2};
      })()`);
      await mouse(box.x,box.y);
    };
    const beginGuide = async mode => {
      await send(`agent page guide ${mode}`);
    };
    for (const mode of ['permission','permission focus','permission checked','permission key']) {
      await beginGuide(mode);
      await guideStep(1);
      await guideButton('next');
      await completed('Guide mutation blocked');
    }
    assert(await page('window.clicks') === 1, 'Guide-only consent allowed a website action');

    assert(await page(`(() => {
      const meta=document.createElement('meta');meta.httpEquiv='Content-Security-Policy';
      meta.content="style-src 'none'; require-trusted-types-for 'script'";document.head.append(meta);
      try {document.createElement('div').innerHTML='blocked';return false;} catch(error) {return error instanceof TypeError;}
    })()`), 'Guide fixture must enforce Trusted Types rather than silently bypassing CSP coverage');
    const originalTarget = await page(`({style:document.querySelector('#name').getAttribute('style'),className:document.querySelector('#name').className})`);
    await beginGuide('walkthrough');
    await guideStep(1);
    const layout = await page(`(() => {
      const host=document.getElementById('services-devtools-page-guide'),root=host.shadowRoot;
      const target=document.querySelector('#name'),rect=target.getBoundingClientRect();
      const frame=root.querySelector('.frame').getBoundingClientRect(),bubble=root.querySelector('.bubble').getBoundingClientRect();
      return {x:rect.x+rect.width/2,y:rect.y+rect.height/2,width:innerWidth,height:innerHeight,
        frame:{x:frame.x,y:frame.y,width:frame.width,height:frame.height},
        hit:document.elementFromPoint(rect.x+rect.width/2,rect.y+rect.height/2)===target,
        untouched:target.getAttribute('style')===${JSON.stringify(originalTarget.style)} && target.className===${JSON.stringify(originalTarget.className)},
        literal:root.querySelector('.text').textContent.includes('<img src=x') && !root.querySelector('img,script'),
        backDisabled:root.querySelector('.back').disabled,topLayer:host.matches(':popover-open'),
        bubbleInside:bubble.left>=0 && bubble.top>=0 && bubble.right<=innerWidth && bubble.bottom<=innerHeight,
        overlap:Math.max(0,Math.min(bubble.right,rect.right)-Math.max(bubble.left,rect.left))
          *Math.max(0,Math.min(bubble.bottom,rect.bottom)-Math.max(bubble.top,rect.top)),
        color:getComputedStyle(root.querySelector('.bubble')).backgroundColor};
    })()`);
    assert(layout.hit && layout.untouched && layout.literal && layout.backDisabled && layout.topLayer
      && layout.bubbleInside && layout.overlap===0 && layout.color==='rgb(255, 248, 207)',
      `Guide must be an isolated, styled, nonblocking overlay: ${JSON.stringify(layout)}`);
    const screenshot = await command('Page.captureScreenshot', {format:'png',captureBeyondViewport:false});
    const image = new Image(); image.src=`data:image/png;base64,${screenshot.data}`; await image.decode();
    const canvas = document.createElement('canvas');canvas.width=image.width;canvas.height=image.height;
    const drawing=canvas.getContext('2d');drawing.drawImage(image,0,0);
    const pixel=drawing.getImageData(Math.floor((layout.frame.x+1)*image.width/layout.width),
      Math.floor((layout.frame.y+layout.frame.height/2)*image.height/layout.height),1,1).data;
    assert(pixel[0]>220 && pixel[1]>170 && pixel[2]<130, `Guide frame did not paint over the real page: ${[...pixel]}`);
    await page(`document.querySelector('#name').style.transform='translateX(35px)';true`);
    await wait(async () => await page(`(() => {
      const target=document.querySelector('#name').getBoundingClientRect();
      const frame=document.getElementById('services-devtools-page-guide').shadowRoot.querySelector('.frame').getBoundingClientRect();
      return Math.abs(frame.left-Math.max(0,target.left-6))<1;
    })()`), 'Guide frame did not follow a moving target');
    await page(`document.querySelector('#name').style.removeProperty('transform');window.scrollBy(0,30);true`);
    await wait(async () => await page(`(() => {
      const target=document.querySelector('#name').getBoundingClientRect();
      const frame=document.getElementById('services-devtools-page-guide').shadowRoot.querySelector('.frame').getBoundingClientRect();
      return Math.abs(frame.top-Math.max(0,target.top-6))<1;
    })()`), 'Guide frame did not follow real page scrolling');
    const inputBox = await page(`(() => {const e=document.querySelector('#name'),r=e.getBoundingClientRect();e.select();return{x:r.x+r.width/2,y:r.y+r.height/2}})()`);
    await mouse(inputBox.x,inputBox.y);
    await page(`document.querySelector('#name').select();true`);
    await command('Input.insertText', {text:'Manual product'});
    assert(await page(`document.querySelector('#name').value`) === 'Manual product', 'Overlay blocked manual typing into the site');
    await guideButton('next');
    await guideStep(2);
    assert(!testid('agent-stop').disabled && await page('window.clicks') === 1, 'Guide clicked the site or ended before the user decision');
    const saveBox=await page(`(() => {const r=document.querySelector('#apply').getBoundingClientRect();return{x:r.x+r.width/2,y:r.y+r.height/2}})()`);
    await mouse(saveBox.x,saveBox.y);
    assert(await page('window.clicks') === 2 && !testid('agent-stop').disabled,
      'A manual site click must remain possible and must not automatically advance the guide');
    await guideButton('back');
    await guideStep(1);
    await guideButton('next');
    await guideStep(2);
    await guideButton('finish');
    await completed('Guide ended: finish');
    assert(!await page(`!!document.getElementById('services-devtools-page-guide') || !!window.__servicesDevtoolsPageGuide`),
      'Finished guide leaked overlay or page state');

    await beginGuide('mutation');
    await guideStep(1);
    await guideButton('next');
    await completed('Guide mutation blocked');
    assert(await page('window.clicks') === 2, 'Starting guidance failed to lock out later agent mutations');
    await beginGuide('escape');
    await guideStep(1);
    await command('Input.dispatchKeyEvent', {type:'keyDown',key:'Escape',code:'Escape',windowsVirtualKeyCode:27});
    await command('Input.dispatchKeyEvent', {type:'keyUp',key:'Escape',code:'Escape',windowsVirtualKeyCode:27});
    await completed('Guide ended: dismissed');
    await beginGuide('stop');
    await guideStep(1);
    const stoppedCleanups = guideCleanups;
    await click(testid('agent-stop'));
    await wait(async () => testid('agent-stop').disabled && !await page(`!!document.getElementById('services-devtools-page-guide')`));
    // Revocation removes the overlay before the pending guide poll unwinds.
    await wait(() => guideCleanups >= stoppedCleanups + 2);
    await beginGuide('target lost');
    await guideStep(1);
    await page(`document.querySelector('#name').hidden=true;true`);
    await completed('Guide ended: target_lost');
    await page(`document.querySelector('#name').hidden=false;true`);
    await beginGuide('invalid');
    await completed('Guide rejected:');
    assert(!await page(`!!document.getElementById('services-devtools-page-guide')`), 'Invalid guide created a stranded overlay');
    await beginGuide('dynamic');
    await wait(async () => await page(`document.getElementById('services-devtools-page-guide')?.shadowRoot.querySelector('.step').textContent === 'Step 1'`));
    await guideButton('finish');
    await completed('Guide ended: finish');
    await beginGuide('lease');
    await guideStep(1);
    await page(`window.__servicesDevtoolsPageGuide.lastSeen=-100000;window.__servicesDevtoolsPageGuide.update();true`);
    await completed('Guide ended: disconnected');
    await beginGuide('navigation');
    await guideStep(1);
    // Fixture behaviour: the inspected page navigates on its own shortly after this step.
    await page(`setTimeout(() => {location.href=${JSON.stringify(fixtureUrl + '/page-agent-fixture?guide-reload=1')}},25);true`);
    await completed('Guide rejected:');
    assert(!await page(`!!document.getElementById('services-devtools-page-guide') || !!window.__servicesDevtoolsPageGuide`),
      'Navigation retained a stale guide');
    await click(testid('agent-new-session'));
    await beginGuide('closing');
    await guideStep(1);
    const closedCleanups = guideCleanups;
    await click(testid('agent-sessions').querySelector('.active [data-testid="agent-session-close"]'));
    await wait(async () => !await page(`!!document.getElementById('services-devtools-page-guide')`));
    await wait(() => guideCleanups >= closedCleanups + 2);
    await beginGuide('application hidden');
    await guideStep(1);
    const hiddenCleanups = guideCleanups;
    window.dispatchEvent(new Event('services-console-hidden'));
    await wait(async () => testid('agent-stop').disabled && !await page(`!!document.getElementById('services-devtools-page-guide')`));
    await wait(() => guideCleanups >= hiddenCleanups + 2);
    window.dispatchEvent(new Event('services-console-shown'));

    await send('agent page navigation');
    await pending();
    const changed = fixtureUrl.replace('127.0.0.1', 'localhost');
    assert(changed !== fixtureUrl, 'Origin-change fixture must use a distinct origin');
    // Fixture behaviour: the inspected page navigates on its own shortly after this step.
    await page(`setTimeout(() => {location.href=${JSON.stringify(changed + '/page-agent-fixture')}},50);true`);
    let navigationResult;
    await wait(async () => {
      navigationResult = await input('Test.inspectedPage', {expression:'location.origin'});
      return navigationResult.result?.value === changed;
    }, () => `Inspected page did not finish navigating: ${JSON.stringify(navigationResult)}`);
    await click(testid('agent-approve-page'));
    await completed('Page rejected: The document changed');
    await send('agent page write');
    assert((await pending()).textContent.includes(changed), 'Cross-origin navigation reused a previous document grant');
    await click(testid('agent-approve-page'));
    await completed('Page write complete');
    await send('agent page write another turn');
    await completed('Page write complete');
    // Fixture behaviour: the inspected page navigates on its own shortly after this step.
    await page(`setTimeout(() => {location.href=${JSON.stringify(changed + '/page-agent-fixture?reload=1')}},25);true`);
    let reloadResult;
    await wait(async () => {
      // A poll may coincide with the navigation itself, which Chrome reports as a navigated target.
      reloadResult = await input('Test.inspectedPage', {expression:`location.search === '?reload=1'`});
      return reloadResult.result?.value === true;
    }, () => `Inspected page did not finish reloading: ${JSON.stringify(reloadResult)}`);
    await send('agent page write after reload');
    await pending();
    await click(testid('agent-deny-page'));
    await completed('Page rejected: Page writing was denied');
    await send('agent page read');
    await completed('Page snapshot ready');
  } finally {
    if (original) Object.defineProperty(chrome, 'devtools', original);
    else delete chrome.devtools;
    await click(testid('agent-sessions').querySelector('.active [data-testid="agent-session-close"]'));
    await click(testid('console-agent-toggle'));
  }
  return true;
})()
