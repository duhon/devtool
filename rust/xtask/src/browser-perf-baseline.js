// The 3.0 browser performance baseline: panel open,
// a Console REST round trip and a large response, each as median/p90 in milliseconds.
(async () => {
  const {assert, wait, waitFor, find, testid, cm, key, click, openConsole, setSource, visible} = testDriver;
  const stats = values => {
    const sorted = [...values].sort((a, b) => a - b);
    const at = q => sorted[Math.round((sorted.length - 1) * q)];
    return {median: +at(0.5).toFixed(1), p90: +at(0.9).toFixed(1), n: sorted.length};
  };
  await openConsole();
  // Panel open: a same-origin frame loads the panel until its Console editor is visible.
  const opens = [];
  for (let round = 0; round < 12; round++) {
    const frame = document.createElement('iframe');
    frame.style.cssText = 'position:fixed;left:0;top:0;width:1000px;height:700px;opacity:0;pointer-events:none;border:0';
    const started = performance.now();
    frame.src = 'panel.html';
    document.body.appendChild(frame);
    await waitFor(() => [...(frame.contentDocument?.querySelectorAll('.console-root .CodeMirror') || [])].some(visible),
      {message: 'A reopened panel must show its Console', interval: 2});
    const elapsed = performance.now() - started;
    frame.remove();
    if (round >= 2) opens.push(elapsed);
    await new Promise(resolve => setTimeout(resolve, 300));
  }
  // Runs one Console check and returns the time from Shift+Enter to the finished response.
  const run = async (source, done, message) => {
    await click(testid('console-service-rest'));
    const editor = cm('console-editor');
    setSource(editor, source);
    const status = find('.console-status');
    const started = performance.now();
    await key('Shift+Enter');
    const output = () => {
      const value = editor.getValue();
      const marker = value.indexOf('\n#responce ');
      return marker < 0 ? '' : value.slice(value.indexOf('\n', marker + 1) + 1);
    };
    await waitFor(() => !status.classList.contains('is-running') && done(output()),
      {message, interval: 2, timeout: 30000});
    return performance.now() - started;
  };
  const rest = [];
  for (let round = 0; round < 25; round++) {
    const elapsed = await run(`#check perf\n#endpoint\nGET ${fixtureUrl}/perf-baseline?round=${round}`,
      output => output.includes(`round=${round}`), 'The REST fixture must answer');
    if (round >= 5) rest.push(elapsed);
  }
  const large = [];
  for (let round = 0; round < 7; round++) {
    const elapsed = await run(`#check large ${round}\n#endpoint\nGET ${fixtureUrl}/native-large-response`,
      output => output.includes('response truncated ('), 'The large response must render');
    if (round >= 2) large.push(elapsed);
  }
  assert(opens.length && rest.length && large.length, 'Every metric must have samples');
  globalThis.perfBaselineReceipt = JSON.stringify({
    panelOpenMs: stats(opens), restRoundTripMs: stats(rest), largeResponseMs: stats(large),
  });
  return true;
})()
