import init from './wasm/services_ui.js';

try {
  await init();
} catch (error) {
  document.getElementById('app').textContent = `Could not start Services DevTools: ${error.message}`;
  console.error(error);
}
