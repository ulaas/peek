// Peek site: the eye that follows the pointer, the pinned tour window, scroll reveals.
(() => {
  const still = matchMedia('(prefers-reduced-motion: reduce)').matches;

  // Top bar gets a hairline once the page scrolls.
  const top = document.querySelector('.top');
  const onScroll = () => top.classList.toggle('scrolled', scrollY > 8);
  addEventListener('scroll', onScroll, { passive: true });
  onScroll();

  // The eye looks toward the pointer (or keyboard focus) and blinks now and then.
  const eye = document.querySelector('.eye');
  const pupil = eye.querySelector('.pupil');
  const lid = eye.querySelector('.lid');
  if (!still) {
    const look = (x, y) => {
      const r = eye.getBoundingClientRect();
      const dx = x - (r.left + r.width / 2);
      const dy = y - (r.top + r.height / 2);
      const d = Math.hypot(dx, dy) || 1;
      const k = Math.min(1, d / 400);
      // Pupil rests at (55, 49); keep it inside the lens.
      const tx = (dx / d) * 12 * k - 5 * (1 - k);
      const ty = (dy / d) * 7 * k;
      pupil.style.transform = `translate(${tx.toFixed(2)}px, ${ty.toFixed(2)}px)`;
    };
    addEventListener('pointermove', e => look(e.clientX, e.clientY), { passive: true });
    addEventListener('focusin', e => {
      const r = e.target.getBoundingClientRect();
      look(r.left + r.width / 2, r.top + r.height / 2);
    });
    const blink = () => {
      lid.classList.remove('blink');
      void lid.getBBox();
      lid.classList.add('blink');
      setTimeout(blink, 3500 + Math.random() * 5000);
    };
    lid.addEventListener('animationend', () => lid.classList.remove('blink'));
    setTimeout(blink, 4200);
  }

  // Pinned tour window: the chapter in the middle of the viewport picks the scene.
  const win = document.querySelector('.stage .win');
  const chapters = [...document.querySelectorAll('.chapter')];
  const show = ch => {
    chapters.forEach(c => c.classList.toggle('active', c === ch));
    win.dataset.scene = ch.dataset.scene;
  };
  show(chapters[0]);
  const io = new IntersectionObserver(entries => {
    entries.forEach(e => e.isIntersecting && show(e.target));
  }, { rootMargin: '-45% 0px -45% 0px' });
  chapters.forEach(c => io.observe(c));

  // The connect field types the kinds of address Peek accepts.
  const typed = document.querySelector('.typed');
  if (typed && !still) {
    const words = typed.dataset.words.split('|');
    let w = 0, i = 0, del = false;
    const tick = () => {
      if (win.dataset.scene !== 'home') return setTimeout(tick, 400);
      const word = words[w];
      i += del ? -1 : 1;
      typed.textContent = word.slice(0, i);
      let wait = del ? 35 : 70 + Math.random() * 60;
      if (!del && i === word.length) { del = true; wait = 1600; }
      else if (del && i === 0) { del = false; w = (w + 1) % words.length; wait = 350; }
      setTimeout(tick, wait);
    };
    typed.textContent = '';
    setTimeout(tick, 600);
  }

  // Point out the download for this computer.
  const ua = (navigator.userAgentData?.platform || navigator.platform || navigator.userAgent).toLowerCase();
  const os = /win/.test(ua) ? 'windows' : /mac/.test(ua) ? 'mac' : /linux|x11/.test(ua) && !/android/.test(ua) ? 'linux' : null;
  const mine = os && document.querySelector(`.dl [data-os="${os}"]`);
  if (mine) {
    mine.classList.add('mine');
    const tag = document.createElement('span');
    tag.className = 'mine-tag';
    tag.textContent = 'This computer';
    mine.querySelector('h3').append(tag);
  }

  // Headings and blocks rise in once as they enter the viewport.
  const reveals = document.querySelectorAll('.reveal');
  const rio = new IntersectionObserver(entries => {
    entries.forEach(e => {
      if (!e.isIntersecting) return;
      e.target.classList.add('in');
      rio.unobserve(e.target);
    });
  }, { rootMargin: '0px 0px -10% 0px' });
  reveals.forEach(r => rio.observe(r));
})();
