// Keep one started emulator per page. Other frames return to their small poster.
(() => {
  const players = [...document.querySelectorAll('.example-player')].map(root => ({
    root, frame: root.querySelector('iframe'), status: root.querySelector('.player-status'),
    toggle: root.querySelector('[data-command="toggle"]'),
    reset: root.querySelector('[data-command="reset"]'), state: 'idle', timer: null,
  }));
  let active = null;
  const send = (player, command) => player.frame.contentWindow?.postMessage(
    {type: 'psoxide-command', command}, location.origin);
  const state = (player, value, message) => {
    player.state = value;
    player.root.dataset.state = value;
    player.status.textContent = message;
    player.toggle.disabled = !['running', 'paused'].includes(value);
    player.reset.disabled = player.toggle.disabled;
    player.toggle.textContent = value === 'paused' ? 'Resume' : 'Pause';
  };
  const stopOthers = player => {
    for (const other of players) {
      if (other !== player && ['running', 'paused', 'loading'].includes(other.state)) {
        clearTimeout(other.timer);
        send(other, 'pause');
        other.frame.src = other.frame.src;
        state(other, 'idle', 'Stopped to free memory. Click to run again.');
      }
    }
    active = player;
  };
  window.addEventListener('message', event => {
    if (event.origin !== location.origin || event.data?.type !== 'psoxide-event') return;
    const player = players.find(p => p.frame.contentWindow === event.source);
    if (!player) return;
    switch (event.data.event) {
      case 'starting':
        stopOthers(player);
        state(player, 'loading', 'Loading the emulator and example…');
        player.timer = setTimeout(() => {
          if (player.state === 'loading') state(player, 'error', 'Loading is taking too long. Reload the page or open the player separately.');
        }, 60000);
        break;
      case 'running':
        clearTimeout(player.timer);
        if (active && active !== player) { send(player, 'pause'); return; }
        active = player;
        state(player, 'running', 'Running');
        break;
      case 'paused': state(player, 'paused', 'Paused. Resume when ready.'); break;
      case 'error':
        clearTimeout(player.timer);
        state(player, 'error', `Could not run this example: ${String(event.data.message || 'unknown error')}. Try the separate player or download the EXE.`);
        break;
    }
  });
  for (const player of players) {
    player.toggle.addEventListener('click', () => {
      if (player.state === 'paused') { stopOthers(player); send(player, 'resume'); player.frame.focus(); }
      else send(player, 'pause');
    });
    player.reset.addEventListener('click', () => { send(player, 'reset'); player.frame.focus(); });
    const full = player.root.querySelector('[data-command="fullscreen"]');
    if (!player.frame.requestFullscreen) full.hidden = true;
    else full.addEventListener('click', () => player.frame.requestFullscreen().catch(() => {
      player.status.textContent = 'Full screen is unavailable. Open the player separately instead.';
    }));
  }
  const observer = new IntersectionObserver(entries => {
    for (const entry of entries) if (!entry.isIntersecting) {
      const player = players.find(p => p.root === entry.target);
      if (player?.state === 'running' || player?.state === 'loading') send(player, 'pause');
    }
  });
  players.forEach(player => observer.observe(player.root));
})();
