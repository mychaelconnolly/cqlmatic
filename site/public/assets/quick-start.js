(() => {
  const button = document.querySelector('.copy-command');
  if (!button || !navigator.clipboard?.writeText) return;
  const command = document.querySelector('#quick-start-command');
  const status = document.querySelector('.copy-status');
  button.hidden = false;
  button.addEventListener('click', async () => {
    try {
      await navigator.clipboard.writeText(command.textContent.trim());
      status.textContent = 'Copied to clipboard.';
    } catch {
      status.textContent = 'Could not copy. Select the command and copy it.';
    }
  });
})();
