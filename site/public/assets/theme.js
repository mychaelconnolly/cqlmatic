(() => {
  const modes = ['dark', 'light', 'auto'];
  const root = document.documentElement;
  const system = window.matchMedia('(prefers-color-scheme: dark)');
  let mode = 'dark';
  let button;
  try {
    const saved = localStorage.getItem('cqlmatic-theme');
    if (modes.includes(saved)) mode = saved;
  } catch {}

  function apply() {
    const theme = mode === 'auto' ? (system.matches ? 'dark' : 'light') : mode;
    root.dataset.theme = theme;
    root.dataset.themePreference = mode;
    document.querySelector('meta[name="theme-color"]').setAttribute('content', theme === 'dark' ? '#151517' : '#ffffff');
    if (!button) return;
    const label = `Theme: ${mode}. Switch to ${modes[(modes.indexOf(mode) + 1) % modes.length]}.`;
    button.setAttribute('aria-label', label);
    button.setAttribute('title', label);
    button.querySelectorAll('[data-theme-icon]').forEach(icon => { icon.toggleAttribute('hidden', icon.dataset.themeIcon !== mode); });
  }

  apply();
  system.addEventListener('change', () => { if (mode === 'auto') apply(); });
  document.addEventListener('DOMContentLoaded', () => {
    button = document.querySelector('.theme-toggle');
    button.addEventListener('click', () => {
      mode = modes[(modes.indexOf(mode) + 1) % modes.length];
      try { localStorage.setItem('cqlmatic-theme', mode); } catch {}
      apply();
    });
    apply();
    button.hidden = false;
  });
})();
