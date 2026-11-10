(async () => {
  const {assert, wait, click, testid, editorOf, choose, openPanel, closePanel, visible} = testDriver;
  const GHOST = '\u200b';
  const status = () => testid('console-diff-status').textContent;
  const counter = () => testid('diff-counter').textContent;
  const light = () => testid('console-diff-toggle').querySelector('.indicator-light')?.dataset.state;
  const set = (side, text) => editorOf(`diff-${side}`).setValue(text);
  const text = side => editorOf(`diff-${side}`).getValue();
  const marked = (side, cls) => (testid(`diff-${side}`).closest('.ui-editor')?.querySelectorAll(`.${cls}`).length ?? 0);
  const mode = value => choose(testid('diff-mode'), value);
  const open = async args => {
    window.__testDiffArgs = JSON.stringify(args);
    window.dispatchEvent(new Event('services-test-diff-open'));
  };
  const reply = () => wait(() => window.__testDiffReply ? JSON.parse(window.__testDiffReply) : null,
    'The opener must hear the panel');
  await testDriver.openConsole();

  await openPanel('diff');
  await wait(() => testDriver.editorAt('diff-left') && testDriver.editorAt('diff-right'));
  assert(text('left') === '' && text('right') === '', 'The panel opened by its button must be empty');
  await wait(() => status().includes('Identical'), () => `Empty panel status: ${status()}`);
  assert(counter() === '0 of 0', `Counter of an empty panel: ${counter()}`);
  assert(!document.querySelector('[data-testid="diff-done"]'), 'Done is only for a comparison another module opened');
  assert(light() !== 'running', 'The light must not run for an empty panel');

  set('left', 'a\nb\nc\nd');
  set('right', 'a\nx\nc\nd\ne');
  await wait(() => status().includes('Diff · 2 differences'), () => `Two differences expected: ${status()}`);
  assert(counter() === '1 of 2', `Counter: ${counter()}`);
  await wait(() => text('left') === `a\nb\nc\nd\n${GHOST}` && text('right') === 'a\nx\nc\nd\ne',
    () => `Columns must line up: ${JSON.stringify([text('left'), text('right')])}`);
  await wait(() => marked('left', 'editor-diff-changed') === 1 && marked('right', 'editor-diff-changed') === 1,
    () => `The changed line must be highlighted in both columns: ${document.querySelector('[data-testid="diff-left"]')?.outerHTML.slice(0, 1500)}`);
  await wait(() => marked('right', 'editor-diff-added') === 1, 'The added line must be highlighted on the right');

  await click(testid('diff-next'));
  assert(counter() === '2 of 2', `Next: ${counter()}`);
  await click(testid('diff-next'));
  assert(counter() === '1 of 2', `Next wraps: ${counter()}`);
  await click(testid('diff-previous'));
  assert(counter() === '2 of 2', `Previous: ${counter()}`);

  // Both columns scroll together.
  const many = Array.from({length: 200}, (_, index) => `line ${index}`).join('\n');
  set('left', many);
  set('right', many + '\nend');
  await wait(() => status().includes('1 difference'), () => `Long texts: ${status()}`);
  const scroller = side => testid(`diff-${side}`).closest('.ui-editor').querySelector('.editor-scroll');
  scroller('left').scrollTop = 300;
  await wait(() => scroller('right').scrollTop === scroller('left').scrollTop && scroller('left').scrollTop > 0,
    () => `Scrolling must follow: ${scroller('left').scrollTop} ${scroller('right').scrollTop}`);

  set('left', 'a\nb\nc');
  set('right', 'a\nx\nc');
  await wait(() => status().includes('1 difference'));
  await click(testid('diff-take-left-0'));
  await wait(() => status().includes('Identical') && text('right') === 'a\nb\nc',
    () => `Taking the left piece must make them equal: ${status()} ${JSON.stringify(text('right'))}`);
  set('right', 'a\nx\nc');
  await wait(() => status().includes('1 difference'));
  await click(testid('diff-take-right-0'));
  await wait(() => status().includes('Identical') && text('left') === 'a\nx\nc',
    () => `Taking the right piece must make them equal: ${status()} ${JSON.stringify(text('left'))}`);

  set('left', 'one two three');
  set('right', 'one 2 three');
  await wait(() => status().includes('1 difference'));
  assert(marked('left', 'editor-diff-removed-text') === 0, 'Lines mode marks no pieces inside a line');
  mode('words');
  await wait(() => marked('left', 'editor-diff-removed-text') === 1 && marked('right', 'editor-diff-added-text') === 1,
    'Words mode must mark the differing word on both sides');
  mode('chars');
  await wait(() => marked('left', 'editor-diff-removed-text') >= 1, 'Characters mode must mark characters');
  mode('lines');

  set('left', 'a  b');
  set('right', 'a b');
  await wait(() => status().includes('1 difference'));
  await click(testid('diff-ignore'));
  await wait(() => status().includes('Identical'), () => `Ignoring whitespace: ${status()}`);
  await click(testid('diff-ignore'));
  await wait(() => status().includes('1 difference'));

  // Another module opens the panel with a comparison and waits for the answer.
  await closePanel('diff');
  await open({left: 'one\ntwo', right: 'one\n2', left_title: 'Mine', right_title: 'Theirs'});
  await openPanel('diff');
  await wait(() => text('left') === 'one\ntwo' && text('right') === 'one\n2', 'The comparison must open');
  assert(testid('diff-left-title').textContent === 'Mine' && testid('diff-right-title').textContent === 'Theirs',
    'The columns must have the titles of the opener');
  await wait(() => light() === 'running', () => `A pending comparison must light the button: ${light()}`);
  assert(!testid('diff-stop').disabled, 'Stop must be enabled while the comparison is pending');
  set('right', 'one\ntwo!');
  await wait(() => status().includes('1 difference'));

  // Hiding is not cancelling.
  await closePanel('diff');
  await wait(() => light() === 'running', 'The light must run while the panel is hidden');
  assert(!window.__testDiffReply, 'Hiding must not answer the opener');
  await openPanel('diff');
  await wait(() => text('right') === 'one\ntwo!', 'The same comparison must come back');
  await click(testid('diff-take-left-0'));
  await wait(() => status().includes('Identical') && text('right') === 'one\ntwo', () => `Accepting: ${status()}`);
  await click(testid('diff-done'));
  await wait(() => text('left') === '' && text('right') === '', () => `Done must clear the panel: ${status()} ${window.__testDiffReply}`);
  const done = await reply();
  assert(done.ok?.text === 'one\ntwo', `Done must return the right text: ${JSON.stringify(done)}`);
  await wait(() => light() !== 'running' && text('left') === '' && text('right') === '',
    'After Done the panel is empty and the light stops');

  // Opening again replaces the comparison; Stop cancels.
  await open({left: 'a', right: 'b'});
  await wait(() => text('left') === 'a' && light() === 'running');
  await open({left: 'c', right: 'd'});
  const replaced = await reply();
  await wait(() => text('left') === 'c' && text('right') === 'd', 'The new comparison must replace the old one');
  assert(replaced.error === undefined || /replac/i.test(replaced.error), 'Replacing is not an answer to the new opener');
  await click(testid('diff-stop'));
  await wait(() => window.__testDiffReply && JSON.parse(window.__testDiffReply).error === 'Cancelled',
    'Stop must cancel for the opener');
  await wait(() => text('left') === '' && text('right') === '' && light() !== 'running',
    'After Stop the panel is empty');
  assert(visible(testid('diff-left')), 'The panel stays open after Stop');
  return true;
})();
