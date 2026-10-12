/* SPACE feature index — renders data.js with text + category filtering */
(() => {
  "use strict";

  const grid = document.getElementById("grid");
  const catsEl = document.getElementById("cats");
  const q = document.getElementById("q");
  const countLine = document.getElementById("count-line");

  const slug = (c) => c.toLowerCase().replace(/[^a-z]+/g, "-");
  const tagClass = (c) => "t-" + slug(c).replace(/^t-/, "");
  // category → css tag class mapping table (matches styles.css)
  const TAG = {
    "Auth & Identity": "t-auth",
    "Admin Fortress": "t-security",
    "Catalog & Metadata": "t-catalog",
    "Search & Discovery": "t-search",
    "Sources & Plugins": "t-sources",
    "Resolution & Playback": "t-playback",
    "Playback Security": "t-playback2",
    "Streaming & Protection": "t-streaming",
    "Tracking & Progress": "t-tracking",
    "Social & Chat": "t-social",
    "Watch Parties": "t-social",
    "Moderation & Safety": "t-moderation",
    Intelligence: "t-intelligence",
    "Media Graph": "t-graph",
    Realtime: "t-realtime",
    "Card Economy": "t-billing",
    "Ops & Observability": "t-ops",
    "Developer Experience": "t-dx",
    "User Platform": "t-auth",
    "Library & Playlists": "t-streaming",
    "File Management": "t-streaming",
  };

  let activeCat = "All";

  const counts = { All: FEATURES.length };
  for (const f of FEATURES) counts[f.c] = (counts[f.c] || 0) + 1;

  function renderCats() {
    const list = ["All", ...CATS].filter((c) => counts[c]);
    catsEl.innerHTML = "";
    for (const c of list) {
      const b = document.createElement("button");
      b.className = "cat" + (c === activeCat ? " active" : "");
      b.innerHTML = `${c}<span class="n">${counts[c]}</span>`;
      b.onclick = () => {
        activeCat = c;
        renderCats();
        render();
      };
      catsEl.appendChild(b);
    }
  }

  function matches(f, needle) {
    if (!needle) return true;
    const hay = [f.n, f.d, f.c, ...(f.e || [])].join(" ").toLowerCase();
    return needle
      .toLowerCase()
      .split(/\s+/)
      .every((w) => hay.includes(w));
  }

  function render() {
    const needle = q.value.trim();
    const items = FEATURES.filter(
      (f) => (activeCat === "All" || f.c === activeCat) && matches(f, needle)
    );

    grid.innerHTML = "";
    if (!items.length) {
      grid.innerHTML = '<div class="empty">nothing matches — try a different filter</div>';
    }
    for (const f of items) {
      const card = document.createElement("article");
      card.className = "feat";
      const eps = (f.e || [])
        .map((e) => {
          const m = e.match(/^(GET|POST|PUT|PATCH|DELETE)\s+(.*)$/);
          return m
            ? `<span class="ep"><b>${m[1]}</b> ${m[2]}</span>`
            : `<span class="ep">${e}</span>`;
        })
        .join("");
      card.innerHTML = `
        <h3><span class="tag ${TAG[f.c] || "t-ops"}">${f.c}</span></h3>
        <h3 style="margin-top:2px">${f.n}</h3>
        <p>${f.d}</p>
        <div class="eps">${eps}</div>`;
      grid.appendChild(card);
    }

    countLine.textContent = `${items.length} of ${FEATURES.length} features shown · ${CATS.length} categories`;
  }

  q.addEventListener("input", render);
  renderCats();
  render();
})();

