/* SPACE — Netflix-style client. Talks ONLY to the SPACE API.
   Sources are server-managed: search returns opaque tokens, the backend
   auto-resolves the best playable stream (with failover). */

const API = location.origin;
const state = {
  token: localStorage.getItem("space_token") || null,
  refresh: localStorage.getItem("space_refresh") || null,
  user: JSON.parse(localStorage.getItem("space_user") || "null"),
  ws: null,
  partyRoom: null,
  mangaTokens: {}, // tokenId -> token
};

/* ---------- tiny helpers ---------- */
const $ = (sel) => document.querySelector(sel);
const el = (html) => { const d = document.createElement("div"); d.innerHTML = html.trim(); return d.firstElementChild; };
const esc = (s) => String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
function toast(msg, ms = 2600) {
  const t = $("#toast");
  t.textContent = msg; t.classList.remove("hidden");
  clearTimeout(t._h); t._h = setTimeout(() => t.classList.add("hidden"), ms);
}

async function api(path, opts = {}) {
  const headers = { ...(opts.headers || {}) };
  if (state.token) headers.Authorization = `Bearer ${state.token}`;
  if (opts.body && typeof opts.body !== "string") {
    opts = { ...opts, body: JSON.stringify(opts.body) };
    headers["Content-Type"] = "application/json";
  }
  let res = await fetch(API + path, { ...opts, headers });
  if ((res.status === 401 || res.status === 422) && state.refresh && !path.includes("/auth/refresh")) {
    if (await tryRefresh()) return api(path, opts);
  }
  if (!res.ok) {
    let msg = res.statusText;
    try { const j = await res.json(); msg = j.message || j.error?.message || msg; } catch {}
    throw new Error(msg);
  }
  if (res.status === 204) return null;
  return res.json();
}

async function tryRefresh() {
  try {
    const res = await fetch(API + "/api/auth/refresh", {
      method: "POST", headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ refresh_token: state.refresh }),
    });
    if (!res.ok) throw new Error();
    saveAuth(await res.json());
    return true;
  } catch { logout(); return false; }
}

function saveAuth(data) {
  state.token = data.access_token; state.refresh = data.refresh_token; state.user = data.user;
  localStorage.setItem("space_token", data.access_token);
  localStorage.setItem("space_refresh", data.refresh_token);
  localStorage.setItem("space_user", JSON.stringify(data.user));
}
function logout() {
  ["space_token", "space_refresh", "space_user"].forEach((k) => localStorage.removeItem(k));
  location.hash = "#/"; location.reload();
}

const FALLBACK_POSTER = `data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' width='170' height='240'><rect fill='%2320263a' width='100%25' height='100%25'/></svg>`;
const poster = (m) => m?.poster || m?.poster_url || FALLBACK_POSTER;
const yearOf = (m) => m?.year ?? (m?.release_date ? String(m.release_date).slice(0, 4) : "");
function mediaCard(m) {
  const id = m.token ? `token:${m.token}` : m.id;
  const cat = m.category || m.media_type || "";
  return `<a class="card" href="#/item/${encodeURIComponent(id)}?cat=${cat}">
    <img loading="lazy" src="${esc(poster(m))}" alt=""/>
    <div class="card-body">
      <div class="title">${esc(m.title)}</div>
      <div class="meta">${esc(String(yearOf(m)))} ${m.score ? "· " + Number(m.score).toFixed(1) : m.rating_avg ? "· " + Number(m.rating_avg).toFixed(1) + "★" : ""}</div>
    </div></a>`;
}
function rail(title, items, renderCard = mediaCard) {
  if (!items?.length) return "";
  return `<h2 class="rail-title">${esc(title)}</h2><div class="row">${items.map(renderCard).join("")}</div>`;
}

/* ---------- auth ---------- */
function showAuth() { $("#auth-screen").classList.remove("hidden"); $("#topbar").classList.add("hidden"); }
function hideAuth() {
  $("#auth-screen").classList.add("hidden"); $("#topbar").classList.remove("hidden");
  $("#user-chip").textContent = state.user?.display_name || state.user?.username || "";
}
document.querySelectorAll(".tab").forEach((tab) => tab.addEventListener("click", () => {
  document.querySelectorAll(".tab").forEach((t) => t.classList.remove("active"));
  tab.classList.add("active");
  $("#login-form").classList.toggle("hidden", tab.dataset.tab !== "login");
  $("#register-form").classList.toggle("hidden", tab.dataset.tab !== "register");
}));
$("#login-form").addEventListener("submit", async (e) => {
  e.preventDefault(); const f = new FormData(e.target);
  try { saveAuth(await api("/api/auth/login", { method: "POST", body: { identifier: f.get("identifier"), password: f.get("password") } })); boot(); }
  catch (err) { authError(err.message); }
});
$("#register-form").addEventListener("submit", async (e) => {
  e.preventDefault(); const f = new FormData(e.target);
  try { saveAuth(await api("/api/auth/register", { method: "POST", body: { username: f.get("username"), email: f.get("email"), display_name: f.get("display_name") || null, password: f.get("password") } })); boot(); }
  catch (err) { authError(err.message); }
});
$("#forgot-link").addEventListener("click", async (e) => {
  e.preventDefault();
  const email = prompt("Account email:"); if (!email) return;
  await api("/api/auth/password/forgot", { method: "POST", body: { email } }).catch(() => {});
  toast("If that address exists, a reset link was sent.");
});
$("#logout-btn").addEventListener("click", () => api("/api/auth/logout", { method: "POST" }).catch(() => {}).then(logout));
function authError(msg) { const p = $("#auth-error"); p.textContent = msg; p.classList.remove("hidden"); }

/* ---------- websocket ---------- */
function connectWS() {
  if (!state.token || state.ws) return;
  const proto = location.protocol === "https:" ? "wss" : "ws";
  const ws = new WebSocket(`${proto}://${location.host}/ws?token=${state.token}`);
  state.ws = ws;
  ws.onmessage = (ev) => {
    try {
      const frame = JSON.parse(ev.data);
      if (frame.type === "hello") $("#presence-badge").textContent = `● ${frame.data.online_users.length}`;
      if (frame.type === "event" && frame.data?.type === "presence")
        $("#presence-badge").textContent = `● ${frame.data.data.online_count}`;
      if (frame.type === "event" && frame.data?.type === "reaction") spawnReactionOverlay(frame.data.data.emoji);
      if (frame.type === "event" && frame.data?.type === "notification")
        toast(`🔔 ${frame.data.data.title}`, 4000);
    } catch {}
  };
  ws.onclose = () => { state.ws = null; setTimeout(connectWS, 3000); };
}
function wsSend(obj) { if (state.ws?.readyState === 1) state.ws.send(JSON.stringify(obj)); }

/* ---------- routing ---------- */
window.addEventListener("hashchange", route);
boot();
async function boot() {
  if (!state.token) return showAuth();
  hideAuth(); connectWS(); route();
}

async function route() {
  const hash = location.hash.slice(1) || "/";
  const [path, queryString] = hash.split("?");
  const parts = path.split("/").filter(Boolean);

  if (!state.token) return showAuth();
  hideAuth();
  const view = $("#view");
  view.innerHTML = `<p class="muted">Loading…</p>`;

  try {
    if (parts.length === 0) return renderHome(view);
    switch (parts[0]) {
      case "browse":   return renderBrowse(view, new URLSearchParams(queryString || ""));
      case "sources":  return renderSourcesPage(view);
      case "search":   return renderSearch(view);
      case "item":     return renderDetail(view, decodeURIComponent(parts[1]), new URLSearchParams(queryString || ""));
      case "music":    return renderMusic(view);
      case "manga":    return renderManga(view);
      case "read":     return renderReader(view, parts[1], parts[2]);
      case "parties":  return renderParties(view, parts[1]);
      case "settings": return renderSettings(view);
      default: view.innerHTML = "<p>Not found</p>";
    }
  } catch (err) {
    view.innerHTML = `<div class="panel"><p class="error">${esc(err.message)}</p></div>`;
  }
}

/* ---------- HOME ---------- */
async function renderHome(view) {
  const [home, films, music, manga, books] = await Promise.all([
    api("/api/home"),
    api("/api/search/all?q=&limit=18&category=movie").catch(() => ({ results: [] })),
    api("/api/search/all?q=songs&limit=14&category=music").catch(() => ({ results: [] })),
    api("/api/search/all?q=manga&limit=14&category=manga").catch(() => ({ results: [] })),
    api("/api/search/all?q=classics&limit=14&category=book").catch(() => ({ results: [] })),
  ]);

  const cwCards = home.continue_watching.map((item) => `
    <a class="card" href="#/media/${item.media_id}">
      <img src="${esc(poster(item))}" alt=""/>
      <div class="card-body">
        <div class="title">${esc(item.title)}</div>
        <div class="meta">${Math.round(item.percent_watched)}% · ${esc(item.episode_label || "movie")}</div>
        <div class="progress"><div style="width:${Math.min(100, item.percent_watched)}%"></div></div>
      </div></a>`);

  const because = home.because_you_watched
    ? rail(`Because you watched “${home.because_you_watched.anchor_title}”`, home.because_you_watched.items)
    : "";

  view.innerHTML = `
    <section class="hero">
      <h1>Tonight on SPACE</h1>
      <p class="muted">Movies · Series · Anime · Music · Manga · Books — one library, every source, zero setup.</p>
      <button class="accent" onclick="location.hash='#/search'">🔍 Search everything</button>
      <button class="ghost" onclick="location.hash='#/music'">🎵 Music</button>
      <button class="ghost" onclick="location.hash='#/manga'">📖 Manga</button>
    </section>
    <div id="cw-slot"></div>
    ${because}
    ${rail("Trending this week", home.trending)}
    ${rail("Films from the Archive", films.results)}
    ${rail("Recommended for you", home.recommended.map((r) => ({ id: r.media_id, title: r.title, poster_url: r.poster_url, score: r.score })))}
    ${rail("Vibe picks · cozy", home.vibe_picks.map((v) => ({ token: null, id: v.media_id, title: v.title, poster_url: v.poster_url, score: v.score })))}
    ${rail("Popular on MangaDex", manga.results)}
    ${rail("Trending on Audius", music.results.map((t) => musicCard(t)))}
    ${rail("Free books", books.results)}
  `;
  $("#cw-slot").outerHTML = rail("Continue watching", cwCards, (c) => c);
}

function musicCard(t) {
  const id = t.token ? `token:${t.token}` : t.id;
  return `<a class="card" href="#/item/${encodeURIComponent(id)}?cat=music">
    <img loading="lazy" src="${esc(poster(t))}" alt=""/>
    <div class="card-body"><div class="title">${esc(t.title)}</div>
    <div class="meta">▶ tap to play</div></div></a>`;
}

/* ---------- BROWSE ---------- */
async function renderBrowse(view, params) {
  const qs = new URLSearchParams();
  for (const [k, v] of params) if (v) qs.set(k, v);
  const [data, unified] = await Promise.all([
    api(`/api/discover?${qs.toString()}`),
    api(`/api/search/all?q=${encodeURIComponent(params.get("q") || "")}&limit=24`).catch(() => ({ results: [] })),
  ]);

  view.innerHTML = `
    <div class="panel">
      <form id="filter-form" style="display:flex;gap:10px;flex-wrap:wrap">
        <input name="q" placeholder="search everything…" value="${params.get("q") || ""}" />
        <select name="media_type">
          <option value="">all types</option>
          ${["movie","series","book","music","manga"].map((t) => `<option value="${t}" ${params.get("media_type") === t ? "selected" : ""}>${t}</option>`).join("")}
        </select>
        <select name="sort">
          <option value="">newest</option>
          ${["title","popularity","rating"].map((s) => `<option value="${s}" ${params.get("sort") === s ? "selected" : ""}>${s}</option>`).join("")}
        </select>
        <input name="genre" placeholder="genre slug" value="${params.get("genre") || ""}"/>
        <input name="year_from" type="number" placeholder="from" style="width:100px" value="${params.get("year_from")||""}"/>
        <input name="year_to" type="number" placeholder="to" style="width:100px" value="${params.get("year_to")||""}"/>
        <button class="accent">Filter</button>
      </form>
    </div>
    ${rail("From your sources", unified.results)}
    <h2 class="rail-title">Library (${data.items.length})</h2>
    <div class="grid">${data.items.map(mediaCard).join("")}</div>
  `;
  $("#filter-form").addEventListener("submit", (e) => {
    e.preventDefault();
    const q = new URLSearchParams();
    for (const [k, v] of new FormData(e.target)) if (v) q.set(k, v);
    location.hash = `#/browse?${q.toString()}`;
  });
}

/* ---------- SEARCH (unified) ---------- */
async function renderSearch(view) {
  const moods = await api("/api/intelligence/moods");
  view.innerHTML = `
    <div class="panel stack">
      <form id="search-form" class="stack">
        <input id="search-input" placeholder='Search everything — try "lofi beats", "sherlock", "one piece", "action 2020-2025"'/>
        <div class="chips" id="mood-chips"></div>
      </form>
    </div>
    <div id="search-results"></div>`;
  $("#mood-chips").innerHTML = moods
    .map((m) => `<button type="button" class="chip" data-q="${esc(m.label.toLowerCase())}">${esc(m.label)}</button>`).join("");
  $("#mood-chips").addEventListener("click", (e) => { if (e.target.dataset.q) doSearch(e.target.dataset.q); });
  $("#search-form").addEventListener("submit", (e) => { e.preventDefault(); doSearch($("#search-input").value.trim()); });

  async function doSearch(q) {
    if (!q) return;
    const everything = await api(`/api/search/all?q=${encodeURIComponent(q)}&limit=40`);
    const groups = {};
    for (const r of everything.results) (groups[r.category] ??= []).push(r);

    let html = "";
    const order = [["movie","Films & TV"],["series","Series"],["manga","Manga"],["music","Music"],["book","Books"]];
    for (const [cat, label] of order) {
      html += rail(label, groups[cat] || []);
    }
    if (!everything.results.length) {
      const fts = await api(`/api/search?q=${encodeURIComponent(q)}&limit=24`).catch(() => ({ items: [] }));
      html = rail("Library results", fts.items);
    }
    $("#search-results").innerHTML = html || `<p class="muted">No results.</p>`;
    saveHistory(q);
  }

  async function saveHistory(q) {
    try {
      const hist = await api("/api/me/search-history");
      renderHistory(hist, q);
    } catch {}
  }
  function renderHistory(rows, activeQ) {
    if (!rows.length) return;
    const box = document.getElementById("history-chips") || el(`<div class="chips" id="history-chips"></div>`);
    box.innerHTML = `<span class="muted small">recent:</span> ` + rows.slice(0, 10)
      .map((r) => `<button class="chip" data-q="${esc(r.query)}">${esc(r.query)}</button>`).join("");
    if (!box.parentNode) $("#search-form").appendChild(box);
    box.onclick = (e) => { if (e.target.dataset.q) { $("#search-input").value = e.target.dataset.q; doSearch(e.target.dataset.q); } };
    void activeQ;
  }
  api("/api/me/search-history").then((rows) => renderHistory(rows)).catch(() => {});
}

/* ---------- DETAIL (catalog id OR unified token) ---------- */
async function renderDetail(view, ref, params) {
  if (ref.startsWith("token:")) return renderTokenDetail(view, decodeURIComponent(ref.slice(6)), params.get("cat"));
  return renderCatalogDetail(view, ref);
}

async function renderTokenDetail(view, token, category) {
  view.innerHTML = `<p class="muted">Resolving best source…</p>`;
  const resolved = await api("/api/playback/resolve", { method: "POST", body: { token, title_hint: "" } });
  view.innerHTML = `
    <div class="detail-hero panel">
      <div style="flex:1">
        <h1>${esc(resolved.title)}</h1>
        <div class="chips"><span class="badge">${esc(resolved.kind)}</span><span class="badge">auto-selected stream</span></div>
        <div class="stack" style="max-width:420px;margin-top:16px">
          <button id="play-btn" class="accent">▶ Play now</button>
          ${category === "music" ? `<p class="muted small">Streams as audio.</p>` : ""}
        </div>
      </div>
    </div>`;
  $("#play-btn").addEventListener("click", () => openPlayerUrl(resolved.url, resolved.kind, resolved.title));
}

async function renderCatalogDetail(view, id) {
  const [detail, similar] = await Promise.all([
    api(`/api/media/${id}`),
    api(`/api/media/${id}/similar?limit=12`).catch(() => []),
  ]);
  const stars = Math.round(detail.rating_avg / 2);
  const episodesFlat = [];
  for (const season of detail.seasons) for (const ep of season.episodes) episodesFlat.push({ season: season.season.number, ...ep });

  view.innerHTML = `
    <div class="detail-hero panel">
      <img class="poster" src="${esc(poster(detail))}"/>
      <div style="flex:1">
        <h1>${esc(detail.title)}</h1>
        <div>
          ${detail.is_anime ? '<span class="badge">anime</span>' : ""}
          <span class="badge">${esc(detail.media_type)}</span>
          <span class="badge">${esc(detail.status)}</span>
          <span class="stars">${"★".repeat(stars)}${"☆".repeat(5 - stars)}</span>
          <span class="muted small">${detail.rating_avg.toFixed(1)} (${detail.rating_count})</span>
        </div>
        <p>${esc(detail.overview || "")}</p>
        <div class="chips">${detail.genres.map((g) => `<a class="chip" href="#/browse?genre=${esc(g.slug)}">${esc(g.name)}</a>`).join("")}</div>
        <div style="display:flex;gap:8px;flex-wrap:wrap;margin:14px 0">
          <button id="play-btn" class="accent">▶ Play</button>
          <select id="status-select">
            <option value="">+ Add to list</option>
            ${["planned","watching","completed","on_hold","dropped"].map((s) => `<option ${detail.my?.status === s ? "selected" : ""}>${s}</option>`).join("")}
          </select>
          <select id="score-select">
            <option value="">rate</option>${[10,9,8,7,6,5,4,3,2,1].map((n) => `<option ${detail.my?.score === n ? "selected" : ""}>${n}</option>`).join("")}
          </select>
          <button id="fav-btn" class="ghost">♥ Favorite</button>
          <button id="party-btn" class="ghost">🎉 Party</button>
        </div>
        ${episodesFlat.length ? `<h3>Episodes</h3><table class="list">${episodesFlat.map((ep, i) => `
          <tr><td>S${ep.season}E${ep.number}</td><td>${esc(ep.title)}</td>
          <td><button class="accent play-episode" data-ep="${ep.id}" data-next="${episodesFlat[i + 1]?.id || ""}">▶</button></td></tr>`).join("")}
        </table>` : detail.book_format ? `<p class="muted">Book file attached.</p>` : ""}
      </div>
    </div>
    ${rail("Similar titles", similar)}
    <h2 class="rail-title">Reviews</h2>
    <div class="panel">
      <form id="review-form" class="stack">
        <input name="title" placeholder="headline"/>
        <textarea name="body" rows="3" placeholder="what did you think?"></textarea>
        <div><button class="accent">Post review</button></div>
      </form>
      <div id="reviews"></div>
    </div>`;

  $("#play-btn").addEventListener("click", () => openPlayerFileId(null, detail.title, nextEpisodeContext(episodesFlat, -1)));
  document.querySelectorAll(".play-episode").forEach((btn) =>
    btn.addEventListener("click", () => {
      const idx = episodesFlat.findIndex((e) => e.id === btn.dataset.ep);
      openPlayerEpisode(btn.dataset.ep, btn.dataset.next, `${detail.title} · S${episodesFlat[idx].season}E${episodesFlat[idx].number}`);
    })
  );
  $("#status-select").addEventListener("change", async (e) => {
    if (!e.target.value) return;
    await api(`/api/me/media/${id}/status`, { method: "PUT", body: { status: e.target.value } });
    toast("List updated");
  });
  $("#score-select").addEventListener("change", async (e) => {
    if (!e.target.value) return;
    await api(`/api/media/${id}/rating`, { method: "PUT", body: { score: Number(e.target.value) } });
    toast("Rated ★");
  });
  $("#fav-btn").addEventListener("click", async () => {
    const r = await api(`/api/media/${id}/favorite`, { method: "POST" });
    toast(r.favorite ? "Added to favorites ♥" : "Removed");
  });
  $("#party-btn").addEventListener("click", async () => {
    const p = await api("/api/parties", { method: "POST", body: { title: `Watching ${detail.title}`, media_id: id } });
    toast(`Party code: ${p.party.join_code}`);
    location.hash = `#/parties/${p.party.id}`;
  });

  loadReviews(id);
  $("#review-form").addEventListener("submit", async (e) => {
    e.preventDefault();
    const fd = new FormData(e.target);
    await api(`/api/media/${id}/reviews`, { method: "POST", body: { title: fd.get("title") || "", body: fd.get("body") } });
    e.target.reset(); loadReviews(id);
  });

  async function loadReviews(mid) {
    const reviews = await api(`/api/media/${mid}/reviews?limit=20`).catch(() => []);
    $("#reviews").innerHTML = reviews.filter((r) => r.body)
      .map((r) => `<div style="border-top:1px solid #1e2537;padding:12px 0">
        <strong>${esc(r.username)}</strong> ${r.score ? `<span class="stars">${r.score.toFixed(0)}★</span>` : ""}
        <p class="muted" style="margin:6px 0 0">${esc(r.body)}</p></div>`).join("");
  }
}

function nextEpisodeContext(flat, currentIndex) {
  return flat.length ? { list: flat, index: currentIndex, title: "" } : null;
}

/* ---------- PLAYER (file / episode / url) ---------- */
let playerCtx = { hls: null, markers: null, partyRoom: null, sessionId: null, heartbeat: null, next: null };
function currentPlaybackSeconds() { const v = $("#video"); return v ? v.currentTime : 0; }

async function openPlayerEpisode(episodeId, nextEpId, title) {
  const resume = await api(`/api/playback/resume/episode/${episodeId}`);
  playerCtx.next = nextEpId ? { episodeId: nextEpId } : null;
  startPlayerWithResume(resume, title);
}

async function openPlayerFileId(_fileId, title, nextCtx) {
  const mediaId = location.hash.match(/media\/([^?/]+)/)?.[1];
  if (!mediaId) return toast("Open from a title page");
  const resume = await api(`/api/playback/resume/${mediaId}`);
  playerCtx.next = nextCtx?.list ? { episodeId: nextCtx.list[Math.min(nextCtx.index + 1, nextCtx.list.length - 1)]?.id } : null;
  startPlayerWithResume(resume, title);
}

function attachMediaSource(video, url, kind) {
  if (kind === "audio" || kind === "mp4" || /\.(mp3|m4a|wav)(\?|$)/.test(url)) {
    video.src = url;
  } else if (/\.m3u8(\?|$)/.test(url)) {
    if (video.canPlayType("application/vnd.apple.mpegurl")) video.src = url;
    else if (window.Hls) { playerCtx.hls = new Hls(); playerCtx.hls.loadSource(url); playerCtx.hls.attachMedia(video); }
    else video.src = url;
  } else {
    video.src = url;
  }
}

function startPlayerWithResume(resume, title) {
  const overlay = $("#player-overlay");
  overlay.classList.remove("hidden");
  const video = $("#video");
  $("#now-playing-title").textContent = title;
  attachMediaSource(video, `${API}/api/files/${resume.file_id}/master.m3u8`, "hls-or-fallback");

  video.currentTime = resume.position_seconds || 0;
  video.play().catch(() => {});

  createSessionAndHeartbeat(resume.file_id);

  api(`/api/files/${resume.file_id}/markers`).then((markers) => {
    playerCtx.markers = markers || [];
    video.ontimeupdate = () => {
      const t = video.currentTime;
      const introEnd = playerCtx.markers.find((m) => m.kind === "intro_end");
      $("#skip-intro").classList.toggle("hidden",
        !(introEnd && t < introEnd.seconds && t > introEnd.seconds - 95));
    };
    $("#skip-intro").onclick = () => {
      const marker = playerCtx.markers.find((m) => m.kind === "intro_end");
      if (marker) video.currentTime = marker.seconds;
    };
  }).catch(() => {});
}

function createSessionAndHeartbeat(fileId) {
  api("/api/playback/sessions", { method: "POST", body: { file_id: fileId, device_name: "web", client: "space-web" } })
    .then((session) => {
      playerCtx.sessionId = session.id;
      playerCtx.heartbeat = setInterval(() => {
        api(`/api/playback/sessions/${playerCtx.sessionId}/heartbeat`,
          { method: "POST", body: { position_seconds: currentPlaybackSeconds() } }).catch(() => {});
      }, 30000);
    })
    .catch(() => {});
}

/* URL-based playback for aggregated sources */
async function openPlayerUrl(url, kind, title) {
  if (kind === "page") return toast("Opening reader…");
  if (kind === "file") return window.open(url.startsWith("http") ? url : API + url, "_blank");
  const overlay = $("#player-overlay");
  overlay.classList.remove("hidden");
  playerCtx.partyRoom = null;
  const video = $("#video");
  $("#now-playing-title").textContent = title;
  attachMediaSource(video, url.startsWith("http") ? url : API + url, kind);
  video.play().catch(() => {});
}

$("#handoff-btn").addEventListener("click", async () => {
  const ticket = await api("/api/playback/handoff", { method: "POST", body: {} });
  const box = $("#handoff-code");
  box.textContent = ticket.code;
  box.classList.remove("hidden");
  toast(`Enter code on another device: ${ticket.code}`);
});

// Auto-next countdown overlay
$("#video").addEventListener("ended", () => {
  if (!playerCtx.next?.episodeId) return;
  let seconds = 8;
  const bar = el(`<div id="next-overlay" class="panel" style="position:fixed;bottom:90px;left:50%;transform:translateX(-50%);z-index:300;display:flex;gap:12px;align-items:center">
    <span>Next episode in <b id="next-count">${seconds}</b>s</span>
    <button class="accent" id="next-now">Play now</button>
    <button class="ghost" id="next-cancel">Cancel</button></div>`);
  document.body.appendChild(bar);
  const timer = setInterval(() => {
    seconds -= 1; bar.querySelector("#next-count").textContent = seconds;
    if (seconds <= 0) { clearInterval(timer); bar.remove(); $("#player-close").click(); openPlayerEpisode(playerCtx.next.episodeId, "", "Next episode"); }
  }, 1000);
  bar.querySelector("#next-now").onclick = () => { clearInterval(timer); bar.remove(); openPlayerEpisode(playerCtx.next.episodeId, "", "Next episode"); };
  bar.querySelector("#next-cancel").onclick = () => { clearInterval(timer); bar.remove(); };
});

$("#player-close").addEventListener("click", () => {
  const v = $("#video");
  if (v) { v.pause(); v.removeAttribute("src"); v.load?.(); }
  playerCtx.hls?.destroy?.(); playerCtx.hls = null;
  if (playerCtx.heartbeat) clearInterval(playerCtx.heartbeat);
  if (playerCtx.sessionId) {
    api(`/api/playback/sessions/${playerCtx.sessionId}`, { method: "DELETE" }).catch(() => {});
    playerCtx.sessionId = null;
  }
  $("#player-overlay").classList.add("hidden");
  $("#handoff-code").classList.add("hidden");
  $("#skip-intro").classList.add("hidden");
});
document.querySelectorAll("#skip-intro").length; // noop keep-alive

function spawnReactionOverlay(emoji) {
  const s = document.createElement("span");
  s.textContent = emoji;
  s.style.cssText = "position:fixed;bottom:80px;left:" + (20 + Math.random() * 60) + "%;font-size:34px;z-index:300;transition:transform 2s ease, opacity 2s ease;opacity:.9";
  document.body.appendChild(s);
  requestAnimationFrame(() => { s.style.transform = "translateY(-220px)"; s.style.opacity = "0"; });
  setTimeout(() => s.remove(), 2100);
}

/* ---------- SOURCES (browse feeds + live health) ---------- */
async function renderSourcesPage(view) {
  const [statusData, list] = await Promise.all([
    api("/api/sources/status").catch(() => ({ sources: [] })),
    api("/api/sources").catch(() => ({ sources: [] })),
  ]);
  const health = Object.fromEntries((statusData.sources || []).map((s) => [s.name, s]));
  const isAdmin = ["admin", "moderator"].includes(state.user?.role);

  view.innerHTML = `
    <section class="hero"><h1>📡 Sources</h1>
      <p class="muted">Every connected catalog, live health-checked. The backend auto-picks whichever is fastest and fails over silently.</p></section>
    <div class="grid" id="source-cards"></div>
    ${isAdmin ? `<div class="panel" style="max-width:720px">
      <h3>Add a source by URL</h3>
      <form id="add-source" style="display:flex;gap:8px;flex-wrap:wrap">
        <input name="url" placeholder="https://my-plugin.example.com" style="flex:1;min-width:260px"/>
        <button class="accent">Detect & register</button>
      </form>
      <p class="muted small">SPACE probes manifest.json → /items → /search and registers whatever protocol it finds. Metadata sources appear in search; source plugins appear here for browsing/import.</p>
    </div>` : ""}`;

  const grid = $("#source-cards");
  for (const src of list.sources || []) {
    const h = health[src.name] || { status: "unknown" };
    const dot = { healthy: "ok", degraded: "", down: "err", unknown: "" }[h.status] ?? "";
    const latency = h.latency_ms != null ? `${h.latency_ms}ms` : "—";
    const card = el(`<div class="panel" style="padding:14px">
      <div style="display:flex;justify-content:space-between;align-items:center">
        <b>${esc(src.label || src.name)}</b>
        <span class="${dot ? (dot === "ok" ? "ok" : "err") : "muted"} small">● ${esc(h.status)}</span>
      </div>
      <div class="muted small">${esc(src.category || "")} · ${esc(src.transport)} · ${latency}</div>
      ${h.last_error ? `<div class="err small">${esc(h.last_error)}</div>` : ""}
      <div style="display:flex;gap:8px;margin-top:10px;flex-wrap:wrap">
        <button class="accent browse-btn" data-name="${esc(src.name)}">Browse feed</button>
        ${isAdmin && src.transport !== undefined ? `<button data-name="${esc(src.name)}" class="import-btn ghost">Import 20</button>` : ""}
      </div>
      <div class="feed-slot" style="margin-top:12px"></div>
    </div>`);
    grid.appendChild(card);
  }

  grid.addEventListener("click", async (e) => {
    const browseBtn = e.target.closest(".browse-btn");
    if (browseBtn) {
      const name = browseBtn.dataset.name;
      const slot = browseBtn.closest(".panel").querySelector(".feed-slot");
      slot.innerHTML = `<p class="muted small">loading feed…</p>`;
      try {
        const feed = await api(`/api/sources/${name}/browse?limit=12`);
        slot.innerHTML = `<div class="grid" style="grid-template-columns:repeat(auto-fill,minmax(140px,1fr))">${feed.items.map((it) =>
          mediaCard({ token: it.token || btoa(JSON.stringify({ s: name, e: it.external_id })), title: it.title, poster_url: it.poster, release_date: it.year ? String(it.year) : "" })
        ).join("")}</div>`;
        // route cards through unified detail
        slot.querySelectorAll("a.card").forEach((a) => {
          const raw = a.getAttribute("href");
          if (raw.includes("token%3A") || raw.includes("token:")) {
            /* already token-shaped */
          }
        });
      } catch (err) { slot.innerHTML = `<p class="err small">${esc(err.message)}</p>`; }
    }
    const importBtn = e.target.closest(".import-btn");
    if (importBtn) {
      try {
        const r = await api(`/api/admin/sources/${importBtn.dataset.name}/sync`, { method: "POST", body: { limit: 20 } });
        toast(`Imported ${r.imported}${r.skipped != null ? `, skipped ${r.skipped}` : ""}`);
      } catch (err) { toast(err.message); }
    }
  });

  const form = $("#add-source");
  if (form) form.addEventListener("submit", async (e) => {
    e.preventDefault();
    try {
      const r = await api("/api/admin/sources/register-url", { method: "POST", body: { url: new FormData(form).get("url") } });
      toast(`Registered as '${r.detected_kind}' ✓ — reloading`);
      setTimeout(() => renderSourcesPage(view), 800);
    } catch (err) { toast(err.message); }
  });
}
async function renderMusic(view) {
  const trending = await api("/api/search/all?q=songs&limit=30&category=music").catch(() => ({ results: [] }));
  view.innerHTML = `
    <section class="hero"><h1>🎵 Music</h1><p class="muted">Streamable via Audius — artist-uploaded and licensed.</p></section>
    <div class="panel stack">
      <form id="music-search" style="display:flex;gap:10px">
        <input name="q" placeholder="search tracks, artists…" style="flex:1"/>
        <button class="accent">Search</button>
      </form>
      <table class="list" id="track-table">
        ${trending.results.map((t) => trackRow(t)).join("")}
      </table>
    </div>
    <audio id="audio-bar" controls style="width:100%;margin-top:14px;display:none"></audio>`;
  bindTrackRows(view);
  $("#music-search").addEventListener("submit", async (e) => {
    e.preventDefault();
    const q = new FormData(e.target).get("q");
    const results = await api(`/api/search/all?q=${encodeURIComponent(q)}&limit=30&category=music`);
    $("#track-table").innerHTML = results.results.map(trackRow).join("") || "<tr><td class='muted'>No tracks.</td></tr>";
    bindTrackRows(view);
  });
  function trackRow(t) {
    return `<tr class="track-row" data-token="${esc(t.token)}" data-title="${esc(t.title)}">
      <td style="width:52px"><img src="${esc(poster(t))}" style="width:44px;height:44px;border-radius:6px;object-fit:cover"/></td>
      <td>${esc(t.title)}</td><td class="muted small">${esc(t.overview || "")}</td>
      <td><button class="accent play-track">▶</button></td></tr>`;
  }
  function bindTrackRows() {
    view.querySelectorAll(".play-track").forEach((btn) =>
      btn.addEventListener("click", async (e) => {
        const row = e.target.closest("tr");
        const audio = $("#audio-bar");
        audio.style.display = "block";
        audio.src = "about:blank";
        const resolved = await api("/api/playback/resolve", { method: "POST", body: { token: row.dataset.token, title_hint: row.dataset.title } });
        audio.src = resolved.url; audio.play().catch(() => {});
        toast(`▶ ${resolved.title}`);
      }));
  }
}

/* ---------- MANGA ---------- */
async function renderManga(view) {
  const popular = await api("/api/search/all?q=manga&limit=36&category=manga").catch(() => ({ results: [] }));
  view.innerHTML = `
    <section class="hero"><h1>📖 Manga</h1><p class="muted">Licensed chapters via MangaDex. Tap any cover to read instantly.</p></section>
    <div class="panel stack">
      <form id="manga-search" style="display:flex;gap:10px">
        <input name="q" placeholder="search manga…" style="flex:1"/><button class="accent">Search</button>
      </form>
    </div>
    <div id="manga-grid"><div class="grid">${popular.results.map(mediaCard).join("")}</div></div>`;
  $("#manga-search").addEventListener("submit", async (e) => {
    e.preventDefault();
    const q = new FormData(e.target).get("q");
    const results = await api(`/api/search/all?q=${encodeURIComponent(q)}&limit=36&category=manga`);
    $("#manga-grid").innerHTML = `<div class="grid">${results.results.map(mediaCard).join("")}</div>`;
  });
}

async function renderReader(view, tokenId, _chapterId) {
  const token = state.mangaTokens[tokenId];
  if (!token) return toast("Open this manga from Browse first");
  view.innerHTML = `<div class="panel"><h2>Manga</h2>
    <div class="chips" id="chapter-chips"><span class="muted">loading chapters…</span></div>
    <label class="small muted"><input type="checkbox" id="reader-mode" checked/> webtoon mode (long strip)</label>
    </div>
    <div id="pages" class="reader"></div>`;
  const data = await api("/api/playback/manga/open", { method: "POST", body: { token } });

  const chips = $("#chapter-chips");
  chips.innerHTML = "";
  for (const ch of data.chapters.slice(0, 120)) {
    const chip = el(`<button class="chip" data-cid="${esc(ch.id)}">Ch ${esc(ch.chapter)}</button>`);
    chip.addEventListener("click", () => loadChapter(ch.id));
    chips.appendChild(chip);
  }

  async function loadChapter(chapterId) {
    $("#pages").innerHTML = `<p class="muted">Loading pages…</p>`;
    const result = await api("/api/playback/manga/pages", { method: "POST", body: { token, chapter_id: chapterId } });
    $("#pages").innerHTML = result.pages
      .map((url) => `<img class="page" loading="lazy" src="${esc(url)}"/>`).join("");
    chips.querySelectorAll(".chip").forEach((c) => c.classList.toggle("active", c.dataset.cid === chapterId));
  }
  if (data.first_chapter_id) loadChapter(data.first_chapter_id);
}

/* ---------- PARTIES ---------- */
async function renderParties(view, partyId) {
  if (!partyId) {
    const mine = await api("/api/me/parties").catch(() => []);
    view.innerHTML = `
      <h1>Watch parties</h1>
      <div class="panel stack" style="max-width:480px">
        <button id="create-party" class="accent">🎉 Create a party</button>
        <form id="join-code" style="display:flex;gap:8px">
          <input name="code" placeholder="join code"/><button>Join</button>
        </form>
      </div>
      ${mine.length ? `<h2 class="rail-title">Your parties</h2><table class="list">
        ${mine.map((p) => `<tr><td><a href="#/parties/${p.id}">${esc(p.title || "untitled")}</a></td>
        <td class="muted">${esc(p.join_code)}</td><td>${p.is_playing ? "▶ playing" : "⏸ paused"}</td></tr>`).join("")}
      </table>` : ""}`;
    $("#create-party").addEventListener("click", async () => {
      const p = await api("/api/parties", { method: "POST", body: { title: "Movie night" } });
      location.hash = `#/parties/${p.party.id}`;
    });
    $("#join-code").addEventListener("submit", async (e) => {
      e.preventDefault();
      const p = await api("/api/parties/join-by-code", { method: "POST", body: { join_code: new FormData(e.target).get("code") } });
      location.hash = `#/parties/${p.id}`;
    });
    return;
  }

  const data = await api(`/api/parties/${partyId}`);
  const room = `party:${partyId}`;
  state.partyRoom = room;
  wsSend({ type: "subscribe", room });

  view.innerHTML = `
    <div class="panel">
      <h1>${esc(data.party.title || "Watch party")} <span class="code">${esc(data.party.join_code)}</span></h1>
      <p class="muted">${data.members.length}/${data.party.max_participants} watching · you ${data.is_host ? "are the host" : "follow along"}</p>
      <div style="display:flex;gap:10px;margin:14px 0;flex-wrap:wrap">
        ${data.can_control ? `<button id="ctl-play" class="accent">▶ Play</button><button id="ctl-pause" class="accent">⏸ Pause</button><button id="ctl-seek" class="ghost">Seek to 0</button>` : `<span class="badge">synced mode</span>`}
        ${data.party.media_id ? `<button id="party-watch" class="accent">🎬 Open player synced</button>` : ""}
      </div>
      <div class="grid" style="grid-template-columns: 1fr 320px">
        <div class="panel">
          <h3>Party chat</h3>
          <div id="party-messages" style="height:260px;overflow-y:auto"></div>
          <form id="party-chat-form" style="display:flex;gap:8px;margin-top:10px">
            <input name="body" placeholder="say something…" style="flex:1"/><button class="accent">Send</button>
          </form>
          <div class="chips" id="party-reactions">
            ${["❤","😂","😮","😱","🔥"].map((e) => `<button class="chip react" data-e="${e}">${e}</button>`).join("")}
          </div>
        </div>
        <div class="panel">
          <h3>Viewers</h3>
          <ul>${data.members.map((m) => `<li>${esc(m.display_name)} <span class="muted small">${esc(m.role)}</span></li>`).join("")}</ul>
          <h3>Timeline</h3><div class="muted small">Reactions build a heatmap for the session.</div>
        </div>
      </div>
    </div>`;

  if (data.can_control) {
    const ctl = (action, position_seconds) =>
      api(`/api/parties/${partyId}/control`, { method: "POST", body: { action, position_seconds } })
        .then((s) => toast(`${action} @ ${Math.floor(s.server_position_seconds)}s`))
        .catch((err) => toast(err.message));
    $("#ctl-play").onclick = () => ctl("play");
    $("#ctl-pause").onclick = () => ctl("pause");
    $("#ctl-seek").onclick = () => ctl("seek", 0);
  }
  if (data.party.media_id) {
    $("#party-watch").onclick = async () => {
      const resume = await api(`/api/playback/resume/${data.party.media_id}`);
      playerCtx.partyRoom = room;
      startPlayerWithResume(resume, data.party.title || "Party watch");
    };
  }

  $("#party-chat-form").addEventListener("submit", async (e) => {
    e.preventDefault();
    const body = new FormData(e.target).get("body");
    if (!body) return;
    wsSend({ type: "typing", room });
    appendPartyMessage({ sender_name: "you", body }, true);
    const conv = await api("/api/chat/conversations").catch(() => []);
    const partyConv = (conv || []).find((c) => c.kind === "party" && c.party_id === partyId);
    if (partyConv) await api(`/api/chat/conversations/${partyConv.id}/messages`, { method: "POST", body }).catch((err) => toast(err.message));
    e.target.reset();
  });

  $("#party-reactions").addEventListener("click", (e) => {
    if (!e.target.dataset.e) return;
    wsSend({ type: "reaction", room, emoji: e.target.dataset.e, second_offset: Math.floor(currentPlaybackSeconds()) });
  });
}
function appendPartyMessage(msg, mine = false) {
  const box = $("#party-messages");
  if (!box) return;
  box.appendChild(el(`<div style="padding:6px 0;border-bottom:1px solid #1e2537">
    <strong class="${mine ? "ok" : ""}">${esc(msg.sender_name)}</strong>
    <span class="muted"> ${esc(msg.body)}</span></div>`));
  box.scrollTop = box.scrollHeight;
}

/* ---------- SETTINGS ---------- */
async function renderSettings(view) {
  const settings = await api("/api/users/me/settings");
  view.innerHTML = `
    <h1>Settings</h1>
    <div class="panel stack" style="max-width:520px">
      <label class="small muted">theme</label>
      <select name="theme">${["dark","light","system"].map((t) => `<option ${settings.theme === t ? "selected" : ""}>${t}</option>`).join("")}</select>
      <label class="small muted">max streaming quality</label>
      <select name="max_streaming_quality">${["480p","720p","1080p","4k"].map((q) => `<option ${settings.max_streaming_quality === q ? "selected" : ""}>${q}</option>`).join("")}</select>
      <label class="small muted">preferred audio language</label>
      <input name="preferred_audio_language" value="${esc(settings.preferred_audio_language)}"/>
      <label class="small muted">preferred subtitle language</label>
      <input name="preferred_subtitle_language" value="${esc(settings.preferred_subtitle_language || "")}"/>
      <label><input type="checkbox" name="autoplay_next_episode" ${settings.autoplay_next_episode ? "checked" : ""}/> autoplay next episode</label>
      <label><input type="checkbox" name="show_continue_watching" ${settings.show_continue_watching ? "checked" : ""}/> show continue watching</label>
      <div class="panel" style="background:var(--panel-2)">
        <b>Email verification</b>
        <p class="muted small" id="email-status">status: checking…</p>
        <div style="display:flex;gap:8px">
          <button id="send-code" class="ghost">Send code</button>
          <input id="verify-code" placeholder="6-digit code" maxlength="6" style="width:140px"/>
          <button id="verify-btn" class="accent">Verify</button>
        </div>
      </div>
      <button id="save-settings" class="accent">Save</button>
    </div>`;
  $("#save-settings").addEventListener("click", async () => {
    const patch = {};
    document.querySelectorAll(".panel [name]").forEach((input) => {
      patch[input.name] = input.type === "checkbox" ? input.checked : input.value;
    });
    await api("/api/users/me/settings", { method: "PATCH", body: patch });
    toast("Settings saved");
  });

  // email verification flow
  const me = await api("/api/users/me");
  $("#email-status").textContent = `status: ${me.email_verified ? "verified ✓" : "NOT verified"}`;
  $("#send-code").addEventListener("click", async () => {
    const r = await api("/api/auth/email/request-code", { method: "POST" });
    if (r.already_verified) return toast("Already verified ✓");
    if (r.dev_code) { $("#verify-code").value = r.dev_code; toast("Dev mode: code filled in"); }
    else toast("Code sent to your email");
  });
  $("#verify-btn").addEventListener("click", async () => {
    try {
      await api("/api/auth/email/verify", { method: "POST", body: { code: $("#verify-code").value.trim() } });
      $("#email-status").textContent = "status: verified ✓";
      toast("Email verified ✓");
    } catch (err) { toast(err.message); }
  });
}
