// Registers the "Services" DevTools panel. Runs once per opened DevTools
// window (the devtools_page named in manifest.json); panel.html is loaded
// lazily by Chrome only once the user actually clicks the panel's tab.
chrome.devtools.panels.create('Services', '', 'panel.html', panel => {
  let panelWindow;
  panel.onShown.addListener(window => {
    panelWindow = window;
    window.dispatchEvent(new window.Event('services-console-shown'));
  });
  panel.onHidden.addListener(() => {
    panelWindow?.dispatchEvent(new panelWindow.Event('services-console-hidden'));
  });
});
