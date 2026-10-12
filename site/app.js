/* SPACE · how it works — brief edition, vanilla JS */
(() => {
  "use strict";

  /* ---------------- nav scroll-spy ---------------- */
  const links = [...document.querySelectorAll("#nav a")];
  const spy = new IntersectionObserver(
    (entries) => entries.forEach((e) => {
      if (!e.isIntersecting) return;
      const id = "#" + e.target.id;
      links.forEach((l) => l.classList.toggle("active", l.getAttribute("href") === id));
    }),
    { rootMargin: "-35% 0px -55% 0px" }
  );
  document.querySelectorAll("section[id]").forEach((s) => spy.observe(s));

  /* ---------------- resolver stepper ---------------- */
  const steps = [...document.querySelectorAll("#steps li")];
  const noteEl = document.getElementById("step-note");
  const statusEl = document.getElementById("step-status");
  const runBtn = document.getElementById("run-btn");
  const resultCard = document.getElementById("result-card");
  let timer = null;

  function show(i) {
    steps.forEach((s, j) => {
      s.classList.toggle("active", j === i);
      s.classList.toggle("done", j < i);
    });
    if (steps[i]) noteEl.textContent = "// " + steps[i].textContent.trim() + " — " + steps[i].dataset.note;
  }
  function stop() {
    if (timer) clearInterval(timer);
    timer = null;
    runBtn.textContent = "▶ Run pipeline";
  }
  function run() {
    stop();
    resultCard.classList.add("hidden");
    steps.forEach((s) => s.classList.remove("active", "done"));
    let idx = 0;
    show(0);
    statusEl.textContent = "resolving…";
    runBtn.textContent = "■ stop";
    timer = setInterval(() => {
      idx += 1;
      if (idx >= steps.length) {
        stop();
        statusEl.textContent = "200 OK";
        noteEl.textContent = "// done — grant expires in 600 s, watermark bound to this session";
        resultCard.classList.remove("hidden");
        runBtn.textContent = "↻ run again";
        return;
      }
      show(idx);
      statusEl.textContent = `stage ${idx + 1}/${steps.length}`;
    }, 560);
  }
  runBtn.addEventListener("click", () => (timer ? (stop(), statusEl.textContent = "stopped") : run()));
  steps.forEach((s, i) =>
    s.addEventListener("click", () => {
      if (timer) return;
      steps.forEach((x, j) => { x.classList.toggle("active", j === i); x.classList.toggle("done", j < i); });
      noteEl.textContent = "// " + s.textContent.trim() + " — " + s.dataset.note;
      resultCard.classList.toggle("hidden", i !== steps.length - 1);
      statusEl.textContent = `stage ${i + 1}/${steps.length}`;
    })
  );

  /* ---------------- event feed simulation ---------------- */
  const feedPanel = document.querySelector(".card.wide");
  const feed = document.createElement("div");
  feed.className = "feed";
  feedPanel?.querySelector("h3")?.after(feed);

  const EVENTS = [
    ["resolution.issued", { kind: "hls", quality: "1080p", source: "archive" }],
    ["playback.authorized", { grant_id: "0192…", fingerprint: "bound ✓" }],
    ["grant.segment", { scope: "seg:12", ttl: 120 }],
    ["progress.updated", { media_type: "manga", percent: 41.5 }],
    ["offline.license.issued", { ttl_hours: 24, device: "keystore-bound" }],
    ["security.flag", { decision: "challenge", score: 46 }, true],
    ["playback.started", { device_name: "living-room-tv" }],
    ["journey.served", { anchor: "anime S1", next: "manga v4" }],
  ];
  let on = true, idx = 0;

  setInterval(() => {
    if (!on) return;
    const [event, data, flag] = EVENTS[idx++ % EVENTS.length];
    const line = document.createElement("div");
    line.className = "ev" + (flag ? " sec" : "");
    line.innerHTML =
      `<span class="t">${new Date().toLocaleTimeString()}</span>{ "event": "<b>${event}</b>", "data": ${JSON.stringify(data)} }`;
    feed.prepend(line);
    while (feed.children.length > 18) feed.lastChild.remove();
  }, 1500);

  feed.addEventListener("click", () => {
    on = !on;
    feed.style.opacity = on ? "1" : ".55";
  });

  /* first paint */
  show(0);
})();
