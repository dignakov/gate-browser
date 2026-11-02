// Watches this frame's media for the browser: called once per page context
// with the native `report(state)`, before the page's own scripts. Returns the
// control the browser uses to play and pause it.
//
// Media counts once it has played audibly (muted autoplay previews never
// do); then the frame reports "playing" while any of it plays and "paused"
// while it all waits, until it goes away ("none").
// Media events don't bubble, but they do pass the document while capturing.
(report) => {
  const started = new Set();
  // What the browser paused, to resume exactly that.
  let resumable = [];
  let last = "none";
  const sync = () => {
    for (const el of started) if (!el.isConnected) started.delete(el);
    const playing = [...started].some((el) => !el.paused && !el.ended);
    const now = playing ? "playing" : started.size > 0 ? "paused" : "none";
    if (now !== last) {
      last = now;
      report(now);
    }
  };
  const look = (el, type) => {
    if (!(el instanceof HTMLMediaElement)) return;
    if (type === "emptied") started.delete(el);
    else if (!el.paused && !el.muted && el.volume > 0) started.add(el);
    sync();
  };
  for (const type of ["play", "playing", "pause", "ended", "emptied", "volumechange", "abort", "error"]) {
    document.addEventListener(type, (e) => look(e.target, e.type), true);
  }
  addEventListener("pagehide", () => {
    started.clear();
    sync();
  });
  return (command) => {
    sync();
    if (command === "pause") {
      resumable = [...started].filter((el) => !el.paused && !el.ended);
      for (const el of resumable) el.pause();
    } else if (command === "play") {
      const targets = resumable.filter((el) => started.has(el));
      const chosen = targets.length > 0 ? targets : [...started].slice(-1);
      resumable = [];
      for (const el of chosen) el.play().catch(() => {});
    }
  };
}
