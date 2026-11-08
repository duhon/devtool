import init from './wasm/app.js';

try {
  await init();
} catch (error) {
  document.getElementById('app').textContent = `Could not start Services DevTools: ${error.message}`;
  console.error(error);
}
