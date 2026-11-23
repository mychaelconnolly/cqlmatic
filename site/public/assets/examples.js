(() => {
  const container = document.querySelector('.code-example');
  if (!container) return;
  const slides = Array.from(container.querySelectorAll('.example-slide'));
  const controls = container.querySelector('.example-controls');
  if (slides.length < 2 || !controls) return;

  const INTERVAL_MS = 9500;
  const CROSS_DELAY_MS = 110;
  const SETTLE_MS = 640;
  const motion = window.matchMedia ? window.matchMedia('(prefers-reduced-motion: reduce)') : { matches: false };

  let current = 0;
  let hovered = false;
  let focused = false;
  let rotateTimer = null;
  let enterTimer = null;
  let settleTimer = null;

  const tabs = slides.map((slide, index) => {
    const label = slide.dataset.exampleLabel || `Example ${index + 1}`;
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'example-tab';
    button.setAttribute('role', 'tab');
    button.setAttribute('aria-selected', String(index === 0));
    button.textContent = label;
    button.addEventListener('click', () => {
      switchTo(index);
      schedule();
    });
    controls.appendChild(button);
    return button;
  });

  const progress = container.querySelector('.example-progress');
  const body = container.querySelector('.code-body');

  function clearTransitionTimers() {
    clearTimeout(enterTimer);
    clearTimeout(settleTimer);
    enterTimer = null;
    settleTimer = null;
  }

  function bodyPaddingY() {
    if (!body || typeof window.getComputedStyle !== 'function') return 0;
    const cs = window.getComputedStyle(body);
    return (parseFloat(cs.paddingTop) || 0) + (parseFloat(cs.paddingBottom) || 0);
  }

  function resetProgress() {
    if (!progress) return;
    progress.classList.remove('is-ticking');
    if (motion.matches || document.hidden) return;
    void progress.offsetWidth;
    progress.classList.add('is-ticking');
  }

  function syncPauseState() {
    const paused = hovered || focused || Boolean(document.hidden) || Boolean(motion.matches);
    container.classList.toggle('is-paused', paused);
    if (paused) {
      clearTimeout(rotateTimer);
      rotateTimer = null;
    } else if (!rotateTimer) {
      rotateTimer = setTimeout(() => {
        switchTo((current + 1) % slides.length);
        schedule();
      }, INTERVAL_MS);
    }
  }

  function schedule() {
    clearTimeout(rotateTimer);
    rotateTimer = null;
    resetProgress();
    syncPauseState();
  }

  function switchTo(nextIndex) {
    if (nextIndex < 0 || nextIndex >= slides.length || nextIndex === current) return;
    clearTransitionTimers();

    const prev = slides[current];
    const next = slides[nextIndex];
    current = nextIndex;

    tabs.forEach((tab, index) => {
      tab.setAttribute('aria-selected', String(index === current));
    });

    slides.forEach(slide => {
      if (slide !== prev && slide !== next) {
        slide.classList.remove('is-active', 'is-leaving', 'is-entering');
        slide.setAttribute('aria-hidden', 'true');
        slide.inert = true;
      }
    });

    if (motion.matches) {
      if (body && body.style) body.style.height = '';
      prev.classList.remove('is-active', 'is-leaving', 'is-entering');
      prev.setAttribute('aria-hidden', 'true');
      prev.inert = true;
      next.classList.remove('is-leaving', 'is-entering');
      next.classList.add('is-active');
      next.setAttribute('aria-hidden', 'false');
      next.inert = false;
      return;
    }

    const fromHeight = body && body.offsetHeight ? body.offsetHeight : 0;
    const toHeight = next.offsetHeight ? next.offsetHeight + bodyPaddingY() : 0;
    if (body && body.style && fromHeight && toHeight) {
      body.style.height = `${fromHeight}px`;
      void body.offsetHeight;
    }

    prev.classList.remove('is-active', 'is-entering');
    prev.classList.add('is-leaving');
    prev.setAttribute('aria-hidden', 'true');
    prev.inert = true;

    next.classList.remove('is-leaving', 'is-entering', 'is-active');
    if (body && body.style && fromHeight && toHeight) {
      body.style.height = `${toHeight}px`;
    }

    enterTimer = setTimeout(() => {
      next.classList.add('is-active', 'is-entering');
      next.setAttribute('aria-hidden', 'false');
      next.inert = false;
    }, CROSS_DELAY_MS);

    settleTimer = setTimeout(() => {
      prev.classList.remove('is-leaving');
      next.classList.remove('is-entering');
      if (body && body.style) body.style.height = '';
    }, SETTLE_MS);
  }

  slides.forEach((slide, index) => {
    slide.hidden = false;
    const active = index === 0;
    slide.classList.toggle('is-active', active);
    slide.setAttribute('aria-hidden', String(!active));
    slide.inert = !active;
  });

  controls.setAttribute('role', 'tablist');
  controls.setAttribute('aria-label', 'Example hunt queries');
  controls.hidden = false;
  container.classList.add('is-ready');

  container.addEventListener('mouseenter', () => {
    hovered = true;
    syncPauseState();
  });
  container.addEventListener('mouseleave', () => {
    hovered = false;
    schedule();
  });
  container.addEventListener('focusin', () => {
    focused = true;
    syncPauseState();
  });
  container.addEventListener('focusout', event => {
    if (!container.contains(event.relatedTarget)) {
      focused = false;
      schedule();
    }
  });
  document.addEventListener('visibilitychange', () => {
    if (document.hidden) {
      syncPauseState();
    } else {
      schedule();
    }
  });
  if (typeof motion.addEventListener === 'function') {
    motion.addEventListener('change', () => schedule());
  }

  schedule();
})();
